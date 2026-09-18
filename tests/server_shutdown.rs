//! The external-shutdown path the Windows service uses (T2-1).
//!
//! The SCM delivers `Stop` through a callback on its own thread, not as
//! Ctrl-C, so `run_server` grew a second way to be asked to drain. These tests
//! drive that mechanism directly — `win_service` itself cannot be tested
//! without a real Service Control Manager, which is what
//! `scripts/verify-service.ps1` is for.

use playout_transcode::app::ShutdownToken;
use playout_transcode::bootstrap::ToolchainStatus;
use playout_transcode::config::AppConfig;
use playout_transcode::db;
use playout_transcode::jobs::JobQueue;
use playout_transcode::server::{self, ServerDeps};
use playout_transcode::service_handle::ServiceHandle;
use std::sync::Arc;
use std::time::Duration;

/// A port nothing is listening on right now. Binding and dropping is the only
/// way to ask the OS for one, since `run_server` does its own bind.
async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    l.local_addr().expect("local_addr").port()
}

/// Removes the temp tree when the test ends. Held separately from the deps
/// because `ServerDeps` has to be moved into the server task, and a struct with
/// a `Drop` impl cannot be partially moved out of.
struct TempTree(std::path::PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn fixture(tag: &str) -> (TempTree, ServerDeps) {
    let root = std::env::temp_dir().join(format!("pt-shutdown-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&root);
    let watch = root.join("watch");
    let target = root.join("target");
    let web_ui = root.join("web-ui").join("dist");
    for d in [&watch, &target, &web_ui] {
        std::fs::create_dir_all(d).expect("create temp dir");
    }

    let mut config = AppConfig::default();
    config.paths.watch_folder = watch.to_string_lossy().to_string();
    config.paths.target_folder = target.to_string_lossy().to_string();
    config.initialized = true;

    let pool = Arc::new(
        db::init_pool(&root.join("media_assets.db"))
            .await
            .expect("init pool"),
    );
    let (event_tx, _rx) = tokio::sync::broadcast::channel::<std::sync::Arc<playout_transcode::jobs::SseFrame>>(16);

    let deps = ServerDeps {
        jobs: JobQueue::new(event_tx, Some(pool.clone())),
        config,
        toolchain_status: ToolchainStatus {
            ffmpeg_found: true,
            ffprobe_found: true,
            ffmpeg_version: Some("test".into()),
            ffprobe_version: Some("test".into()),
            bundled: false,
            bin_dir: root.join("bin").to_string_lossy().to_string(),
            ffmpeg_sha256: None,
            ffprobe_sha256: None,
            ffmpeg_path: None,
        },
        service_handle: ServiceHandle::new(),
        web_ui_dir: web_ui,
        pool,
    };

    (TempTree(root), deps)
}

#[tokio::test]
async fn the_server_drains_when_the_shutdown_token_fires() {
    let (_tree, deps) = fixture("drain").await;
    let port = free_port().await;
    let shutdown = ShutdownToken::new();

    let token = shutdown.clone();
    let server = tokio::spawn(async move {
        server::run_server_with_shutdown(port, "127.0.0.1", deps, async move { token.wait().await })
            .await
    });

    // Wait for the bind to land, then confirm it really is serving.
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/api/health", port);
    let mut served = false;
    for _ in 0..50 {
        if let Ok(r) = client.get(&url).send().await {
            if r.status().is_success() {
                served = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        served,
        "the server never answered /api/health on port {port}"
    );

    shutdown.trigger();

    let result = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the server did not drain within 10 s after the stop request")
        .expect("server task panicked");
    assert!(result.is_ok(), "run_server returned an error: {result:?}");

    // The listener is gone: the port can be bound again.
    let rebind = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await;
    assert!(rebind.is_ok(), "the port was still held after shutdown");
}

#[tokio::test]
async fn a_shutdown_requested_before_startup_still_stops_the_server() {
    // The SCM can deliver Stop while the service is still in StartPending. The
    // token latches, so the drain happens rather than the request being lost.
    let (_tree, deps) = fixture("early").await;
    let port = free_port().await;
    let shutdown = ShutdownToken::new();
    shutdown.trigger();

    let token = shutdown.clone();
    let server = tokio::spawn(async move {
        server::run_server_with_shutdown(port, "127.0.0.1", deps, async move { token.wait().await })
            .await
    });

    let result = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the server did not stop despite an already-triggered token")
        .expect("server task panicked");
    assert!(result.is_ok(), "run_server returned an error: {result:?}");
}
