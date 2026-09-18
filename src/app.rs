//! The service's startup sequence, shared by the interactive `run` command and
//! the Windows service entry point (T2-1).
//!
//! This used to live in `main.rs`, which meant the SCM entry point could not
//! reach it: `win_service` is a library module and `main` is the binary. The
//! only behavioural addition is [`ShutdownToken`] — an explicit, triggerable
//! stop, because the Service Control Manager delivers `Stop` through a callback
//! and not as Ctrl-C.

use crate::{
    bootstrap, config, db, identity, instance_lock, jobs, logging, paths, profiles, server,
    service_handle,
};

use anyhow::Result;
use service_handle::ServiceHandle;
use std::sync::Arc;

/// A stop request that can be raised from anywhere, including a non-Tokio
/// thread such as the SCM control handler.
///
/// `tokio::sync::Notify` alone is not enough: the handler can fire before
/// [`ShutdownToken::wait`] is polled, and a `Notify` permit raised with no
/// waiter present is kept but a second `trigger` would be lost. The flag makes
/// the token latching, so a stop delivered during startup is still observed.
#[derive(Clone, Default)]
pub struct ShutdownToken {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    notify: tokio::sync::Notify,
    stopped: std::sync::atomic::AtomicBool,
}

impl ShutdownToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request shutdown. Safe to call from any thread, any number of times.
    pub fn trigger(&self) {
        self.inner
            .stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_triggered(&self) -> bool {
        self.inner.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolve once shutdown has been requested, now or earlier.
    pub async fn wait(&self) {
        loop {
            if self.is_triggered() {
                return;
            }
            // Register interest before re-checking, so a `trigger` racing this
            // loop cannot slip between the check and the await.
            let notified = self.inner.notify.notified();
            if self.is_triggered() {
                return;
            }
            notified.await;
        }
    }
}

/// How long shutdown waits for the HTTP server to drain. Shorter than the SCM's
/// 30 s wait hint, so a wedged connection cannot make the stop look hung.
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// How long shutdown waits for the processing thread to unwind, after the HTTP
/// drain. The two together stay under the SCM's 30 s wait hint.
const WORKER_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Boot the whole service and run until `shutdown` fires or Ctrl-C arrives.
///
/// Returns once the HTTP server has drained and the processing loop has been
/// asked to stop. The caller owns the Tokio runtime: `main` builds one for the
/// interactive path, `win_service` builds one inside `service_main`.
pub async fn run_service(
    config_path_override: Option<String>,
    shutdown: ShutdownToken,
) -> Result<()> {
    use std::path::PathBuf;

    let (app_config, _config_path) = config::AppConfig::load(config_path_override.as_deref())
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    // The UI log panel is one of the four sinks, so the handle has to exist
    // before the subscriber is installed (T2-3).
    let service_handle = ServiceHandle::new();
    let _log_guards = logging::init_service_logging(
        &app_config.logging.level,
        logging::FileLogging {
            dir: paths::log_dir(),
            file: app_config.logging.log_file.clone(),
            retain_days: app_config.logging.retain_days,
        },
        service_handle.clone(),
    );

    // Before the database is opened and before the watcher can exist: a second
    // instance on the same data directory means two watchers on one folder and
    // two writers on one registry (T2-5). Held for the whole run; released on
    // the way out, including on the error paths below, because the binding
    // lives until `run_service` returns.
    let _instance_lock = instance_lock::acquire(&paths::data_dir())
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // After the subscriber is installed, so a panic during the rest of startup
    // reaches the log files rather than a stderr nobody is reading (T3-6).
    logging::install_panic_hook();

    profiles::validate_color_constants()
        .map_err(|e| anyhow::anyhow!("Color constant misconfiguration: {}", e))?;

    // Must happen before any `audit_toolchain()` call so configured paths and
    // the download digest pin are honoured (T1-2).
    bootstrap::set_toolchain_policy(app_config.effective_toolchain_policy());

    let port = app_config.server.web_port;
    let bind_addr = app_config.server.bind_address.clone();
    let url = format!("http://{}:{}", bind_addr, port);
    println!("\n  PlayoutTranscode web UI starting at {}\n", url);
    tracing::info!("PlayoutTranscode starting on {}", url);

    let exe_dir = paths::exe_dir();
    tracing::info!("Data directory: {}", paths::data_dir().display());

    let pool = db::init_pool(&paths::database_path())
        .await
        .map_err(|e| anyhow::anyhow!("Database init failed: {}", e))?;
    let pool = Arc::new(pool);
    tracing::info!("Asset database ready");

    let (_, toolchain_status) = bootstrap::audit_toolchain();
    tracing::info!("FFmpeg: {:?}", toolchain_status.ffmpeg_version);

    // 1024, raised from 256 (T2-10). A single busy encode emits a progress
    // event every 250 ms, so with max_concurrency at 8 the old buffer held
    // about eight seconds of traffic -- less than a browser tab spends
    // throttled in the background. Capacity does not prevent a lag, it only
    // makes one rare; the `resync` event is what makes it survivable.
    let (event_tx, _rx) = tokio::sync::broadcast::channel::<std::sync::Arc<crate::jobs::SseFrame>>(1024);
    let job_queue = jobs::JobQueue::new(event_tx, Some(pool.clone()));
    // One writer for the whole service. Every job mutation is queued to it and
    // coalesced by job id, instead of each one spawning its own upsert (T2-4).
    let persister = job_queue.spawn_persister();
    if let Ok(report) = db::recover_stale_jobs(&pool).await {
        if report.requeued > 0 || report.failed_exhausted > 0 {
            tracing::info!(
                "Startup crash recovery: {} job(s) re-queued, {} job(s) marked failed",
                report.requeued,
                report.failed_exhausted
            );
        }
    }
    reconcile_pending_jobs(&pool).await;
    if let Ok(existing_jobs) = db::load_all_durable_jobs(&pool).await {
        job_queue.populate(existing_jobs);
    }

    let watch_root = PathBuf::from(&app_config.paths.watch_folder);
    let target_root = PathBuf::from(&app_config.paths.target_folder);
    let _ = std::fs::create_dir_all(&target_root);

    // One-time move of sidecars written beside their media by earlier versions
    // into the canonical `<target>/sidecars/` directory (T3-5). Idempotent, so
    // it costs one directory listing per start after the first.
    {
        let report = identity::migrate_legacy_sidecars(&target_root.join("videos"));
        if report.moved > 0 || report.failed > 0 {
            tracing::info!(
                "Sidecar migration: {} moved, {} already current, {} failed",
                report.moved,
                report.already_current,
                report.failed
            );
        }
    }

    let config_initialized = app_config.initialized;

    let server_cfg = app_config.clone();
    let bind_addr = server_cfg.server.bind_address.clone();
    let port = server_cfg.server.web_port;
    let sh = service_handle.clone();
    let jq = job_queue.clone();
    let server_pool = pool.clone();
    let server_shutdown = shutdown.clone();

    let web_ui_dir = resolve_web_ui_dir(&exe_dir);

    let mut server_task = tokio::spawn(async move {
        server::run_server_with_shutdown(
            port,
            &bind_addr,
            server::ServerDeps {
                jobs: jq,
                config: server_cfg,
                toolchain_status: toolchain_status.clone(),
                service_handle: sh,
                web_ui_dir,
                pool: server_pool,
            },
            async move { server_shutdown.wait().await },
        )
        .await
    });

    if config_initialized
        && !watch_root.to_string_lossy().is_empty()
        && !target_root.to_string_lossy().is_empty()
        && app_config.validate().is_ok()
    {
        service_handle.add_log("info", "Auto-starting service with configured watch folder");
        if let Ok(tools) = bootstrap::ensure_toolchain() {
            let _ = service_handle::start_processing_loop(
                &service_handle,
                &app_config,
                &job_queue,
                &tools,
                pool.clone(),
            );
        } else {
            service_handle.add_log("warn", "FFmpeg not found. Download from the web UI.");
        }
    }

    // Daily registry snapshot (T2-13). The asset registry is the playout source
    // of truth and nothing in it can be reconstructed from the media files, so
    // one is taken at startup and then every 24 h. Aborted on shutdown with the
    // rest of the background work.
    let backup_pool = pool.clone();
    let backup_task = tokio::spawn(async move {
        let data_dir = paths::data_dir();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
        loop {
            // Fires immediately on the first tick, which is what gives a fresh
            // install a snapshot before it has run for a day.
            ticker.tick().await;
            match db::backup_now(&backup_pool, &data_dir).await {
                Ok(path) => tracing::info!(
                    "Registry backup written: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                // Never fatal. A service that will not start because it could
                // not write a backup is worse than one running without today's.
                Err(e) => tracing::error!("Registry backup failed: {}", e),
            }
        }
    });

    // `&mut` so the handle survives the select: on the stop paths the server is
    // still draining and has to be awaited below.
    let mut server_already_exited = false;
    tokio::select! {
        result = &mut server_task => {
            server_already_exited = true;
            match result {
                Err(e) => tracing::error!("Server task failed: {}", e),
                Ok(Err(e)) => tracing::error!("Server exited with an error: {}", e),
                Ok(Ok(())) => {}
            }
        }
        _ = shutdown.wait() => {
            tracing::info!("Stop requested; shutting down");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Shutting down...");
        }
    }

    // Reached on every exit path, including the SCM `Stop` control: stop the
    // watcher, kill in-flight FFmpeg children (T0-4 tracks their PIDs) and let
    // the pool close. Without this a service stop left orphaned encoders
    // holding handles on the target folder.
    service_handle::stop_processing(&service_handle);
    shutdown.trigger();

    // The token is what tells the server to drain, so it has to be triggered
    // first. Wait for the drain rather than closing the pool underneath a
    // request that is still being served; bounded, because a wedged SSE client
    // must not hold a service stop open past the SCM's wait hint.
    if !server_already_exited {
        match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, server_task).await {
            Ok(Ok(Err(e))) => tracing::error!("Server exited with an error: {}", e),
            Ok(Err(e)) => tracing::error!("Server task failed: {}", e),
            Ok(Ok(Ok(()))) => {}
            Err(_) => tracing::warn!(
                "HTTP server did not drain within {}s; closing the database anyway",
                SHUTDOWN_DRAIN_TIMEOUT.as_secs()
            ),
        }
    }

    // The processing thread unwinds asynchronously (T2-5), and it holds the
    // pool. Wait for it before the pool closes, or a late encode completion
    // lands on a closed database. Bounded: this and the HTTP drain together
    // have to stay inside the SCM's 30 s wait hint, and the FFmpeg children
    // were already killed above, so anything still running here is wedged.
    {
        let sh = service_handle.clone();
        let stopped = tokio::task::spawn_blocking(move || {
            service_handle::wait_until_stopped(&sh, WORKER_STOP_TIMEOUT)
        })
        .await
        .unwrap_or(false);
        if !stopped {
            tracing::warn!(
                "Processing loop did not reach Stopped within {}s; continuing shutdown",
                WORKER_STOP_TIMEOUT.as_secs()
            );
        }
    }

    // Drain the coalescing persister before the pool goes away, so a stop does
    // not discard the final state of the jobs it just stopped (T2-4).
    job_queue.flush_persister().await;
    if let Some(handle) = persister {
        handle.abort();
    }
    backup_task.abort();

    pool.close().await;
    tracing::info!("Shutdown complete");

    Ok(())
}

/// Fail recovered jobs whose source file has since disappeared.
///
/// `recover_stale_jobs` re-queues interrupted work to `Pending`, but only the
/// filesystem watcher feeds the dispatcher, so a row whose source was moved or
/// deleted while the service was down stayed `Pending` forever and showed up in
/// `/api/jobs` and `/api/stats` as outstanding work that would never run
/// (F-13). Rows whose source still exists are deliberately left `Pending`: the
/// watcher re-offers the file and the dispatcher now adopts the existing record
/// rather than creating a second one for it.
async fn reconcile_pending_jobs(pool: &sqlx::SqlitePool) {
    let pending = match db::load_pending_jobs(pool).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Could not read pending jobs for reconciliation: {}", e);
            return;
        }
    };
    if pending.is_empty() {
        return;
    }

    let paths: Vec<(String, String)> = pending
        .iter()
        .map(|j| (j.id.clone(), j.input_path.clone()))
        .collect();
    let missing = match tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .filter(|(_, path)| !std::path::Path::new(path).exists())
            .map(|(id, _)| id)
            .collect::<Vec<String>>()
    })
    .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("Pending-job reconciliation task failed: {}", e);
            return;
        }
    };

    if missing.is_empty() {
        return;
    }
    match db::fail_jobs_with_missing_source(pool, &missing).await {
        Ok(n) if n > 0 => tracing::warn!(
            "Startup reconciliation: {} pending job(s) failed, source file no longer exists",
            n
        ),
        Ok(_) => {}
        Err(e) => tracing::error!("Could not fail pending jobs with a missing source: {}", e),
    }
}

/// The bundled SPA: next to the exe for an installed build, under the working
/// directory for `cargo run`.
fn resolve_web_ui_dir(exe_dir: &std::path::Path) -> std::path::PathBuf {
    let installed = exe_dir.join("web-ui").join("dist");
    if installed.join("index.html").exists() {
        return installed;
    }
    if let Ok(cwd) = std::env::current_dir() {
        let local = cwd.join("web-ui").join("dist");
        if local.join("index.html").exists() {
            return local;
        }
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_returns_immediately_when_already_triggered() {
        let token = ShutdownToken::new();
        token.trigger();
        // Would hang if the token were not latching.
        tokio::time::timeout(std::time::Duration::from_secs(5), token.wait())
            .await
            .expect("wait resolved");
    }

    #[tokio::test]
    async fn wait_resolves_when_triggered_from_another_thread() {
        let token = ShutdownToken::new();
        let t = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            t.trigger();
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), token.wait())
            .await
            .expect("wait resolved");
        assert!(token.is_triggered());
    }

    #[tokio::test]
    async fn trigger_is_idempotent_and_every_clone_observes_it() {
        let token = ShutdownToken::new();
        let other = token.clone();
        token.trigger();
        token.trigger();
        assert!(other.is_triggered());
        tokio::time::timeout(std::time::Duration::from_secs(5), other.wait())
            .await
            .expect("wait resolved");
    }
}
