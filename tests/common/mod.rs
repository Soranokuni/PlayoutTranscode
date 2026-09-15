//! Shared harness that boots the **real** Axum server for integration tests.
//!
//! Before this existed, `tests/v1_wire_contract.rs` built stub routers with
//! canned JSON, so not one handler in `server.rs` was exercised end to end
//! (F-31). Everything here drives the production `server::build_router`, the
//! production middleware stack and a real SQLite pool.

#![allow(dead_code)]

use playout_transcode::bootstrap::ToolchainStatus;
use playout_transcode::config::AppConfig;
use playout_transcode::db;
use playout_transcode::jobs::JobQueue;
use playout_transcode::server::{self, ServerDeps};
use playout_transcode::service_handle::ServiceHandle;
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A live server on an ephemeral loopback port, plus the temp tree it uses.
///
/// Dropping it shuts the server task down and removes the temp tree.
pub struct TestServer {
    pub base_url: String,
    pub addr: SocketAddr,
    pub port: u16,
    pub pool: Arc<SqlitePool>,
    pub jobs: JobQueue,
    pub service_handle: ServiceHandle,
    pub root: PathBuf,
    pub watch_dir: PathBuf,
    pub target_dir: PathBuf,
    pub web_ui_dir: PathBuf,
    client: reqwest::Client,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Knobs a test may want to change before the server starts.
pub struct TestServerOptions {
    /// Mutate the config after the harness has filled in the temp paths.
    pub config: Box<dyn FnOnce(&mut AppConfig)>,
    /// Write an `index.html` and a sample asset into the SPA directory.
    pub with_web_ui: bool,
}

impl Default for TestServerOptions {
    fn default() -> Self {
        Self {
            config: Box::new(|_| {}),
            with_web_ui: true,
        }
    }
}

pub async fn spawn_test_server() -> TestServer {
    spawn_test_server_with(TestServerOptions::default()).await
}

pub async fn spawn_test_server_with(opts: TestServerOptions) -> TestServer {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("pt-it-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&root);

    let watch_dir = root.join("watch");
    let target_dir = root.join("target");
    let data_dir = root.join("data");
    let web_ui_dir = root.join("web-ui").join("dist");
    for d in [&watch_dir, &target_dir, &data_dir, &web_ui_dir] {
        std::fs::create_dir_all(d).expect("create temp dir");
    }

    if opts.with_web_ui {
        std::fs::write(web_ui_dir.join("index.html"), "<!doctype html><title>spa</title>")
            .expect("write index.html");
        std::fs::create_dir_all(web_ui_dir.join("assets")).expect("assets dir");
        std::fs::write(web_ui_dir.join("assets").join("app.js"), "export const x = 1;\n")
            .expect("write app.js");
        // A file the SPA must never hand out: it sits next to the dist dir,
        // reachable only by escaping it.
        std::fs::write(root.join("secret.toml"), "token = \"do-not-serve\"\n")
            .expect("write secret");
    }

    let mut config = AppConfig::default();
    config.paths.watch_folder = watch_dir.to_string_lossy().to_string();
    config.paths.target_folder = target_dir.to_string_lossy().to_string();
    config.initialized = true;
    (opts.config)(&mut config);

    let pool = db::init_pool(&data_dir.join("media_assets.db"))
        .await
        .expect("init test pool");
    let pool = Arc::new(pool);

    let (event_tx, _rx) = tokio::sync::broadcast::channel::<String>(256);
    let jobs = JobQueue::new(event_tx, Some(pool.clone()));
    let service_handle = ServiceHandle::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();

    let deps = ServerDeps {
        jobs: jobs.clone(),
        config: config.clone(),
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
        service_handle: service_handle.clone(),
        web_ui_dir: web_ui_dir.clone(),
        pool: pool.clone(),
    };

    // `port` here is the port the service is actually reachable on, which is
    // what the CORS allow-list and the Host guard compare against.
    let app = server::build_router(port, "127.0.0.1", deps);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    TestServer {
        base_url: format!("http://127.0.0.1:{}", port),
        addr,
        port,
        pool,
        jobs,
        service_handle,
        root,
        watch_dir,
        target_dir,
        web_ui_dir,
        // `no_proxy` so a machine-wide HTTP proxy cannot intercept loopback
        // traffic, and redirects are off so we assert on the real status.
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build test client"),
        shutdown: Some(shutdown_tx),
    }
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .send()
            .await
            .expect("GET failed")
    }

    pub async fn get_json(&self, path: &str) -> serde_json::Value {
        let body = self.get(path).await.text().await.expect("body");
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("GET {} returned non-JSON ({}): {}", path, e, body))
    }

    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .expect("POST failed")
    }

    pub async fn put_json(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .put(self.url(path))
            .json(&body)
            .send()
            .await
            .expect("PUT failed")
    }

    pub async fn delete_json(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .delete(self.url(path))
            .json(&body)
            .send()
            .await
            .expect("DELETE failed")
    }

    /// Send a request line verbatim over a raw socket.
    ///
    /// `reqwest` normalises dot segments in the path, so traversal attempts
    /// have to bypass it entirely — this is what `curl --path-as-is` does.
    pub async fn raw_request(&self, request_line: &str, host: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(self.addr)
            .await
            .expect("connect");
        let req = format!(
            "{}\r\nHost: {}\r\nConnection: close\r\n\r\n",
            request_line, host
        );
        stream.write_all(req.as_bytes()).await.expect("write");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read");
        String::from_utf8_lossy(&buf).to_string()
    }

    pub fn file(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

/// First line of a raw HTTP response, e.g. `HTTP/1.1 200 OK`.
pub fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or("")
}

/// Body of a raw HTTP response (everything after the blank line).
pub fn raw_body(response: &str) -> &str {
    match response.find("\r\n\r\n") {
        Some(i) => &response[i + 4..],
        None => "",
    }
}

/// Insert a ready asset directly into the registry, bypassing the processor.
pub async fn insert_ready_asset(
    pool: &SqlitePool,
    uuid: &str,
    fingerprint: i64,
    path: &Path,
    display_name: &str,
) {
    db::insert_processing(
        pool,
        uuid,
        fingerprint,
        &path.to_string_lossy(),
        display_name,
    )
    .await
    .expect("insert_processing");
}
