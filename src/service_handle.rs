use crate::bootstrap::{self, ToolPaths};
use crate::config::{self, AppConfig};
use crate::db;
use crate::jobs::JobQueue;
use parking_lot::Mutex;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum ServiceCmd {
    Stop,
}

#[derive(Clone)]
pub struct ServiceHandle {
    pub running: Arc<Mutex<bool>>,
    pub cmd_tx: Arc<Mutex<Option<mpsc::Sender<ServiceCmd>>>>,
    /// Optional channel for the API to inject manual retries into the processing loop.
    pub retry_tx: Arc<StdMutex<Option<mpsc::Sender<std::path::PathBuf>>>>,
    pub download_status: Arc<Mutex<Option<String>>>,
    pub log_lines: Arc<Mutex<Vec<String>>>,
    pub active_pids: Arc<StdMutex<Vec<u32>>>,
}

impl ServiceHandle {
    pub fn new() -> Self {
        Self {
            running: Arc::new(Mutex::new(false)),
            cmd_tx: Arc::new(Mutex::new(None)),
            retry_tx: Arc::new(StdMutex::new(None)),
            download_status: Arc::new(Mutex::new(None)),
            log_lines: Arc::new(Mutex::new(Vec::new())),
            active_pids: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    /// Submit a manual retry for an input file. Fails if the service is not running.
    pub fn submit_retry(&self, path: std::path::PathBuf) -> Result<(), String> {
        if !self.is_running() {
            return Err("Service is not running".into());
        }
        let guard = self
            .retry_tx
            .lock()
            .map_err(|e| format!("retry channel lock: {}", e))?;
        match guard.as_ref() {
            Some(tx) => tx
                .try_send(path)
                .map_err(|e| format!("retry queue full or closed: {}", e)),
            None => Err("retry channel not established".into()),
        }
    }

    pub fn add_log(&self, level: &str, msg: &str) {
        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
        let mut logs = self.log_lines.lock();
        logs.push(format!("{} [{}] {}", ts, level.to_uppercase(), msg));
        while logs.len() > 500 {
            logs.remove(0);
        }
    }

    pub fn get_logs(&self) -> Vec<String> {
        self.log_lines.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        *self.running.lock()
    }

    pub fn kill_active_ffmpeg(&self) {
        let pids: Vec<u32> = {
            match self.active_pids.lock() {
                Ok(mut list) => {
                    let snapshot = list.clone();
                    list.clear();
                    snapshot
                }
                Err(e) => {
                    tracing::error!("active_pids lock poisoned: {}", e);
                    return;
                }
            }
        };

        for pid in pids {
            kill_process_tree(pid);
        }
    }
}

fn kill_process_tree(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let pid_str = pid.to_string();
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid_str, "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

pub fn start_processing_loop(
    handle: &ServiceHandle,
    config: &AppConfig,
    job_queue: &JobQueue,
    tools: &ToolPaths,
    pool: Arc<SqlitePool>,
) -> Result<(), String> {
    if *handle.running.lock() {
        return Err("Service already running".into());
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<ServiceCmd>(1);
    *handle.cmd_tx.lock() = Some(cmd_tx);
    *handle.running.lock() = true;

    let running = handle.running.clone();
    let watch = PathBuf::from(&config.paths.watch_folder);
    let target = PathBuf::from(&config.paths.target_folder);
    let tools = tools.clone();
    let jobs = job_queue.clone();
    let cfg = config.clone();
    let active_pids = handle.active_pids.clone();
    // Clone for the worker thread; the closure below captures this clone by move.
    let handle_for_thread = handle.clone();
    // The retry_tx cleanup slot is also held by this handle. Once handle_for_thread moves
    // into the closure, only the closure can clear it on stop.
    let cleanup_retry_tx = handle_for_thread.retry_tx.clone();

    handle.add_log("info", "Transcoding service started");

    let per_encode_threads = config
        .encoding
        .effective_threads_per_encode(config.ingestion.max_concurrency);
    let total_threads = config
        .encoding
        .effective_total_threads(config.ingestion.max_concurrency);
    handle.add_log(
        "info",
        &format!(
            "CPU budget: {} cores / max_concurrency={} -> {} threads/encode ({} total)",
            if config.encoding.cpu_cores > 0 {
                config.encoding.cpu_cores.to_string()
            } else {
                "auto".to_string()
            },
            config.ingestion.max_concurrency,
            per_encode_threads,
            total_threads,
        ),
    );

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let _ = std::fs::create_dir_all(&target);

            // Recovery sweep: purge DB rows whose source is still in the watch folder so the
            // watcher re-queues them; also purge rows whose source is gone. This is the
            // "auto-purge + retry on start" behaviour.
            match db::recover_failed_assets(&pool, &watch, cfg.ingestion.auto_retry_on_start).await {
                Ok(o) => {
                    if o.purged_for_retry > 0 || o.purged_dead > 0 || o.kept_dead > 0 {
                        handle_for_thread.add_log(
                            "info",
                            &format!(
                                "Recovery: purged {} retryable / {} dead, kept {} dead rows for inspection",
                                o.purged_for_retry, o.purged_dead, o.kept_dead
                            ),
                        );
                    }
                }
                Err(e) => {
                    handle_for_thread.add_log("error", &format!("Recovery sweep DB error: {}", e));
                }
            }

            let (file_tx, mut file_rx) = mpsc::channel::<PathBuf>(256);
            let (retry_tx, mut retry_rx) = mpsc::channel::<PathBuf>(256);
            if let Ok(mut slot) = handle_for_thread.retry_tx.lock() {
                *slot = Some(retry_tx);
            }
            let sem = Arc::new(tokio::sync::Semaphore::new(cfg.ingestion.max_concurrency));

            let excl_ext = cfg.ingestion.exclude_extensions.clone();
            let incl_ext = cfg.ingestion.include_extensions.clone();
            let settle_secs = cfg.ingestion.settle_secs;
            let poll_secs = cfg.ingestion.poll_secs;
            let stable_polls_min = cfg.ingestion.stable_polls_min;
            let w_root = watch.clone();

            tokio::spawn(async move {
                crate::watcher::watch_loop(
                    w_root, settle_secs, poll_secs, stable_polls_min,
                    incl_ext, excl_ext, file_tx,
                )
                .await;
            });

            let mut cmd_rx = cmd_rx;
            loop {
                tokio::select! {
                    Some(path) = file_rx.recv() => {
                        process_one(&tools, &cfg, &jobs, &target, &sem, &pool, &active_pids, path).await;
                    }
                    Some(retry_path) = retry_rx.recv() => {
                        handle_for_thread.add_log(
                            "info",
                            &format!("Manual retry submitted for {}", retry_path.display()),
                        );
                        process_one(&tools, &cfg, &jobs, &target, &sem, &pool, &active_pids, retry_path).await;
                    }
                    cmd = cmd_rx.recv() => {
                        if let Some(ServiceCmd::Stop) = cmd {
                            break;
                        }
                    }
                }
            }
        });

        // Service stopped: clear retry channel so API retries fail fast until the service restarts.
        if let Ok(mut slot) = cleanup_retry_tx.lock() {
            *slot = None;
        }
        *running.lock() = false;
    });

    Ok(())
}

/// Drains one input through the configured concurrency semaphore and into the processor. Used
/// for both watcher-discovered files and manual retries.
async fn process_one(
    tools: &bootstrap::ToolPaths,
    cfg: &config::AppConfig,
    jobs: &JobQueue,
    target: &std::path::Path,
    sem: &Arc<tokio::sync::Semaphore>,
    pool: &SqlitePool,
    active_pids: &Arc<StdMutex<Vec<u32>>>,
    path: std::path::PathBuf,
) {
    let t = tools.clone();
    let c = cfg.clone();
    let jq = jobs.clone();
    let tg = target.to_path_buf();
    let s = sem.clone();
    let p = pool.clone();
    let apids = active_pids.clone();
    let _permit = s.acquire_owned().await;
    tokio::task::spawn_blocking(move || {
        crate::processor::process_file_sync(&jq, &t, &tg, &path, &c, &p, apids);
    });
}

pub fn stop_processing(handle: &ServiceHandle) {
    if let Some(ref tx) = *handle.cmd_tx.lock() {
        let _ = tx.try_send(ServiceCmd::Stop);
    }
    if let Ok(mut slot) = handle.retry_tx.lock() {
        *slot = None;
    }
    handle.kill_active_ffmpeg();
    handle.add_log("info", "Service stop requested");
}

pub fn trigger_download(handle: &ServiceHandle) -> bool {
    let mut status = handle.download_status.lock();
    if status.is_some() {
        return false;
    }
    *status = Some("downloading".to_string());
    drop(status);
    handle.add_log("info", "Starting FFmpeg download (full build)...");

    let h = handle.clone();
    std::thread::spawn(move || match crate::bootstrap::download_ffmpeg() {
        Ok(_) => {
            *h.download_status.lock() = Some("ok".to_string());
        }
        Err(e) => {
            *h.download_status.lock() = Some(format!("error: {}", e));
        }
    });
    true
}

pub fn poll_download_status(handle: &ServiceHandle) -> Option<String> {
    let mut s = handle.download_status.lock();
    if let Some(ref msg) = *s {
        if msg == "ok" {
            *s = None;
            drop(s);
            let (_, status) = crate::bootstrap::audit_toolchain();
            let ver = status.ffmpeg_version.unwrap_or_default();
            handle.add_log("info", &format!("FFmpeg {} downloaded and ready", ver));
            return Some("ok".into());
        } else if msg.starts_with("error:") {
            let err = msg.clone();
            *s = None;
            drop(s);
            handle.add_log("error", &err);
            return Some(err);
        }
    }
    s.clone()
}
