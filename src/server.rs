use crate::bootstrap::ToolchainStatus;
use crate::config::AppConfig;
use crate::db::{self, AssetResponse};
use crate::jobs::{JobQueue, JobState};
use crate::service_handle::ServiceHandle;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct ServerState {
    pub jobs: JobQueue,
    pub config: Arc<Mutex<AppConfig>>,
    pub toolchain_status: Arc<ToolchainStatus>,
    pub service_handle: ServiceHandle,
    pub web_ui_dir: Arc<std::path::PathBuf>,
    pub pool: Arc<SqlitePool>,
    pub started_at: std::time::Instant,
    /// Expensive, rarely-changing values recomputed on a timer rather than per
    /// request (F-06, F-07).
    toolchain_cache: Arc<Cached<ToolchainStatus>>,
    db_check_cache: Arc<Cached<String>>,
}

/// Everything `build_router` and `run_server` need, so the argument list stays
/// one value instead of nine and the test harness can construct it directly.
pub struct ServerDeps {
    pub jobs: JobQueue,
    pub config: AppConfig,
    pub toolchain_status: ToolchainStatus,
    pub service_handle: ServiceHandle,
    pub web_ui_dir: std::path::PathBuf,
    pub pool: Arc<SqlitePool>,
}

pub async fn run_server(port: u16, bind_address: &str, deps: ServerDeps) -> Result<(), String> {
    run_server_with_shutdown(port, bind_address, deps, std::future::pending()).await
}

/// `run_server`, but draining when `shutdown` resolves as well as on Ctrl-C.
///
/// The Service Control Manager delivers `Stop` through a callback on its own
/// thread, not as a console signal, so the Windows service path (T2-1) needs a
/// second way to ask the server to drain.
pub async fn run_server_with_shutdown(
    port: u16,
    bind_address: &str,
    deps: ServerDeps,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let addr = format!("{}:{}", bind_address, port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    tracing::info!("PlayoutTranscode web UI listening on http://{}", addr);

    let app = build_router(port, bind_address, deps);
    // Ctrl-C used to drop the process with in-flight DB writes and the SQLite
    // pool mid-write (F-30). Stop accepting, let open requests finish.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .map_err(|e| format!("Server error: {}", e))
}

async fn shutdown_signal(external: impl std::future::Future<Output = ()> + Send + 'static) {
    tokio::select! {
        r = tokio::signal::ctrl_c() => match r {
            Ok(()) => tracing::info!("Shutdown signal received; draining HTTP requests"),
            Err(e) => tracing::error!("Failed to install Ctrl-C handler: {}", e),
        },
        _ = external => tracing::info!("Stop requested; draining HTTP requests"),
    }
}

/// Requests may not run longer than this, except SSE which streams forever.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum requests in flight. The service shares a host with playout, so an
/// unbounded queue of expensive reads is a real availability risk (F-10).
const MAX_CONCURRENT_REQUESTS: usize = 64;

/// Explicit JSON body cap, rather than relying on axum's implicit 2 MiB.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Build the whole application router.
///
/// Split out of `run_server` so tests can drive the real handlers, real
/// middleware and real state instead of a hand-built stub router (F-31). `port`
/// is the port the service is reachable on — it is what the CORS allow-list and
/// the `Host` guard compare against, so a test serving on an ephemeral port
/// must pass that same port here.
pub fn build_router(port: u16, bind_address: &str, deps: ServerDeps) -> Router {
    let ServerDeps {
        jobs,
        config,
        toolchain_status,
        service_handle,
        web_ui_dir,
        pool,
    } = deps;
    let state_toolchain = Arc::new(toolchain_status);
    let state = ServerState {
        jobs: jobs.clone(),
        config: Arc::new(Mutex::new(config)),
        toolchain_status: state_toolchain.clone(),
        service_handle,
        web_ui_dir: Arc::new(web_ui_dir),
        pool,
        started_at: std::time::Instant::now(),
        toolchain_cache: Arc::new(Cached::seeded(
            TOOLCHAIN_CACHE_TTL,
            (*state_toolchain).clone(),
        )),
        db_check_cache: Arc::new(Cached::new(DB_CHECK_TTL)),
    };

    let api = Router::new()
        .route("/health", get(health))
        .route("/jobs", get(list_jobs))
        .route("/jobs/active", get(list_active_jobs))
        .route("/jobs/completed", get(list_completed_jobs))
        .route("/jobs/failed", get(list_failed_jobs))
        .route("/jobs/pending", get(list_pending_jobs))
        .route("/jobs/{id}/retry", post(post_retry_job))
        .route("/jobs/{id}/cancel", post(post_cancel_job))
        .route("/jobs/retry-failed", post(post_retry_all_failed))
        .route("/config", get(get_config).put(put_config))
        .route("/toolchain", get(get_toolchain_status))
        .route("/stats", get(get_stats))
        .route("/watchfolder", get(get_watchfolder))
        .route("/service/status", get(get_service_status))
        .route("/service/start", post(post_start_service))
        .route("/service/stop", post(post_stop_service))
        .route("/download/start", post(post_download_ffmpeg))
        .route("/download/status", get(get_download_status))
        .route("/logs", get(get_logs))
        .route("/diagnostics", get(get_diagnostics))
        // Removed in T0-5; 410 for one release (see removed_service_endpoint).
        .route("/service/install", post(removed_service_endpoint))
        .route("/service/uninstall", post(removed_service_endpoint))
        .route("/assets", get(list_assets))
        .route("/assets/{uuid}", get(get_asset))
        .route("/assets/{uuid}/trim", put(put_trim))
        .route("/assets/{uuid}/rating", put(put_rating))
        .route("/assets/{uuid}/tp", put(put_tp))
        .route("/assets/{uuid}/rename", put(put_rename))
        .route("/assets/{uuid}/move", put(put_move))
        .route("/assets/{uuid}/subclip", post(post_subclip))
        .route("/assets/{uuid}/purge", delete(delete_purge_asset))
        .route("/assets/{uuid}/regenerate-sidecar", post(post_regenerate_sidecar))
        .route("/assets/{uuid}/trash", post(post_trash_asset).put(post_trash_asset))
        .route("/assets/{uuid}/restore", post(post_restore_asset).put(post_restore_asset))
        .route("/assets/batch", post(post_batch))
        .route("/folders/trash", post(post_trash_folder).put(post_trash_folder))
        .route("/folders/restore", post(post_restore_folder).put(post_restore_folder))
        .route("/folders/purge", delete(delete_purge_folder))
        .route("/recycle-bin", get(get_recycle_bin))
        .route("/recycle-bin/purge", delete(delete_empty_recycle_bin))
        .route("/recycle-bin/auto-purge", post(post_auto_purge))
        .route(
            "/folders/colors",
            get(get_folder_colors).put(put_folder_color),
        )
        .route("/db/overview", get(get_db_overview_handler))
        .route("/db/assets", get(get_db_assets_handler))
        .route("/db/assets/{uuid}", get(get_db_asset_detail_handler))
        .route("/db/assets/{uuid}/regenerate-sidecar", post(post_regenerate_sidecar))
        .route("/db/jobs", get(get_db_jobs_handler))
        .route("/db/jobs/{id}", get(get_db_job_detail_handler))
        .route("/db/folders", get(get_db_folders_handler))
        .route("/db/schema", get(get_db_schema_handler));

    let api_v2 = Router::new()
        .route("/health", get(health_v2))
        .route("/toolchain", get(get_toolchain_status))
        .route("/config", get(get_config).put(put_config))
        .route("/profiles", get(get_profiles_v2))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job_v2))
        .route("/jobs/{id}/cancel", post(post_cancel_job))
        .route("/jobs/{id}/retry", post(post_retry_job))
        .route("/assets", get(list_assets))
        .route("/assets/{uuid}", get(get_asset))
        .route("/assets/{uuid}/trash", post(post_trash_asset))
        .route("/assets/{uuid}/restore", post(post_restore_asset))
        .route("/assets/{uuid}/purge", delete(delete_purge_asset))
        .route("/assets/{uuid}/regenerate-sidecar", post(post_regenerate_sidecar))
        .route("/folders/trash", post(post_trash_folder))
        .route("/folders/restore", post(post_restore_folder))
        .route("/folders/purge", delete(delete_purge_folder))
        .route("/recycle-bin", get(get_recycle_bin))
        .route("/recycle-bin/purge", delete(delete_empty_recycle_bin))
        .route("/recycle-bin/auto-purge", post(post_auto_purge))
        .route("/metrics", get(get_metrics_v2))
        .route("/diagnostics", get(get_diagnostics))
        .route("/db/overview", get(get_db_overview_handler))
        .route("/db/assets", get(get_db_assets_handler))
        .route("/db/assets/{uuid}", get(get_db_asset_detail_handler))
        .route("/db/assets/{uuid}/regenerate-sidecar", post(post_regenerate_sidecar))
        .route("/db/jobs", get(get_db_jobs_handler))
        .route("/db/jobs/{id}", get(get_db_job_detail_handler))
        .route("/db/folders", get(get_db_folders_handler))
        .route("/db/schema", get(get_db_schema_handler));

    let (allowed_origins, api_token) = {
        let cfg = state.config.lock();
        (
            allowed_origin_list(port, &cfg.server.allowed_origins),
            Arc::new(cfg.server.api_token.clone()),
        )
    };
    let cors = build_cors(&allowed_origins);
    let loopback_only = crate::config::is_loopback_bind(bind_address);

    if api_token.is_empty() {
        tracing::info!("API token not set; loopback-only mode");
    } else {
        tracing::info!("API token required on /api/** (health endpoints exempt)");
    }

    // `/api/events` is a long-lived SSE stream, so it is mounted here rather
    // than inside the nested routers, to sit outside the request timeout — a 30 s cap would cut every client's stream.
    let events = Router::new()
        .route("/api/events", get(sse_events))
        .route("/api/v2/events", get(sse_events));

    let timed = Router::new()
        .nest("/api/v2", api_v2)
        .nest("/api", api)
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            REQUEST_TIMEOUT,
        ));

    // Layers run outermost-last, so a request is seen by: concurrency limit,
    // tracing, Host guard, CORS, token, confirmation, then the route.
    Router::new()
        .merge(events)
        .merge(timed)
        .fallback(serve_spa)
        .layer(axum::middleware::from_fn(require_confirmation))
        .layer(axum::middleware::from_fn(move |req, next| {
            require_token(api_token.clone(), req, next)
        }))
        .layer(cors)
        .layer(axum::middleware::from_fn(move |req, next| {
            host_guard(loopback_only, port, req, next)
        }))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower::limit::ConcurrencyLimitLayer::new(
            MAX_CONCURRENT_REQUESTS,
        ))
        .with_state(state)
}

/// `true` when `s` is a canonical, hyphenated UUID.
///
/// PlayOut validates ids client-side, but the service had no server-side
/// guarantee at all: a `{uuid}` path segment was any string (F-08, PlayOut
/// handoff §3.1). Every `{uuid}`/`{id}` route now rejects anything else with
/// 422 before touching the database.
pub fn is_canonical_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if c != b'-' {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Reject a non-canonical id with a 422 that says which field was wrong.
fn reject_bad_id(kind: &'static str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "error": format!("invalid {} id", kind) })),
    )
        .into_response()
}

/// Upper bound on a `tp` compliance string.
pub const MAX_TP_LEN: usize = 512;

/// `true` when `tp` is an acceptable compliance marker.
///
/// PlayOut's `ComplianceModule.vue` writes this and the service stored any
/// string of any length. This is the provisional grammar from the remediation
/// plan; `docs/audit-2026-09-15/PLAYOUT-CLIENT-CHANGES.md` §3 asks the PlayOut
/// team whether `tp` is really an enumeration, in which case this becomes an
/// allow-list.
pub fn is_valid_tp(tp: &str) -> bool {
    if tp.len() > MAX_TP_LEN {
        return false;
    }
    tp.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                ' ' | '_' | '-' | '|' | ':' | '[' | ']' | '{' | '}' | '"' | ',' | '.'
            )
    })
}

/// Named colours the UI offers, alongside `#rrggbb`.
const NAMED_FOLDER_COLORS: &[&str] = &[
    "default", "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "grey",
    "gray",
];

/// `true` when `color` is safe to interpolate into a CSS `style` binding.
///
/// `DbViewer.vue` renders this value straight into a style attribute, so an
/// arbitrary string was a CSS-injection sink (F-08).
pub fn is_valid_folder_color(color: &str) -> bool {
    if color.is_empty() {
        return true; // clears the colour
    }
    if let Some(hex) = color.strip_prefix('#') {
        return hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    NAMED_FOLDER_COLORS.contains(&color.to_ascii_lowercase().as_str())
}

/// `true` when a user-supplied display name is safe to store.
///
/// Length is checked by the caller against `db::MAX_DISPLAY_NAME_LEN`; this
/// rejects control characters, which would corrupt log lines, the sidecar JSON
/// and PlayOut's own rendering.
pub fn is_valid_display_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= db::MAX_DISPLAY_NAME_LEN
        && name == name.trim()
        && !name.chars().any(|c| c.is_control())
}

/// Validate a caller-supplied retry `input_path`.
///
/// Blocking (`canonicalize`) — call it from `spawn_blocking`. A UNC path is
/// rejected outright: `exists()` on `\\host\share` makes the service open an
/// outbound SMB connection and leak an NTLM handshake to an attacker-chosen
/// host (F-08).
pub fn validate_retry_input_path(raw: &str, watch_folder: &str) -> Result<std::path::PathBuf, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("input_path must not be empty");
    }
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return Err("UNC paths are not accepted");
    }
    let p = std::path::Path::new(trimmed);
    if !p.is_absolute() {
        return Err("input_path must be absolute");
    }
    let watch = std::path::Path::new(watch_folder.trim());
    let (Ok(canon), Ok(canon_watch)) = (p.canonicalize(), watch.canonicalize()) else {
        return Err("input_path could not be resolved");
    };
    if !canon.starts_with(&canon_watch) {
        return Err("input_path must be inside the watch folder");
    }
    Ok(canon)
}

/// Path extractor that accepts only a canonical UUID.
///
/// Used for every `{uuid}` and `{id}` route (job ids are v4 UUIDs too), so an
/// id can never reach a query as an arbitrary string.
pub struct AssetId(pub String);

impl<S> axum::extract::FromRequestParts<S> for AssetId
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Path(raw) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(|_| reject_bad_id("asset"))?;
        if is_canonical_uuid(&raw) {
            Ok(AssetId(raw))
        } else {
            tracing::warn!("rejected non-canonical id on {}", parts.uri.path());
            Err(reject_bad_id("asset"))
        }
    }
}

/// How long a cached `ToolchainStatus` is served before being recomputed.
const TOOLCHAIN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a cached database health result is served.
const DB_CHECK_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// A value recomputed at most once per TTL.
pub struct Cached<T> {
    inner: tokio::sync::Mutex<Option<(std::time::Instant, T)>>,
    ttl: std::time::Duration,
}

impl<T: Clone> Cached<T> {
    pub fn new(ttl: std::time::Duration) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
            ttl,
        }
    }

    /// Start warm, with a value computed during startup.
    pub fn seeded(ttl: std::time::Duration, value: T) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(Some((std::time::Instant::now(), value))),
            ttl,
        }
    }

    /// Return whatever is cached without ever recomputing, even if stale.
    ///
    /// For `/api/health`, which PlayOut polls every 5 s and treats as the
    /// liveness signal: it must stay O(1) and side-effect free (handoff §3.5),
    /// so it never triggers a refresh.
    pub async fn peek(&self) -> Option<T> {
        self.inner.lock().await.as_ref().map(|(_, v)| v.clone())
    }

    /// Return the cached value, recomputing it via `refresh` when stale.
    ///
    /// `refresh` runs under the lock, so a burst of concurrent requests
    /// produces one recomputation, not one per request.
    pub async fn get_or_refresh<F, Fut>(&self, refresh: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let mut slot = self.inner.lock().await;
        if let Some((at, value)) = slot.as_ref() {
            if at.elapsed() < self.ttl {
                return value.clone();
            }
        }
        let value = refresh().await;
        *slot = Some((std::time::Instant::now(), value.clone()));
        value
    }

    /// Drop the cached value so the next read recomputes.
    pub async fn invalidate(&self) {
        *self.inner.lock().await = None;
    }
}

/// Recompute the toolchain status off the async runtime.
///
/// `audit_toolchain` spawns `ffmpeg -version` and `ffprobe -version` and hashes
/// both binaries, so it is doubly unsuitable for an async handler (F-07) and
/// far too expensive to run per request: the bundled UI polls `/api/toolchain`
/// every 2 s, which was two process spawns per second per open tab (F-06).
async fn refresh_toolchain_status() -> ToolchainStatus {
    match tokio::task::spawn_blocking(|| crate::bootstrap::audit_toolchain().1).await {
        Ok(status) => status,
        Err(e) => {
            tracing::error!("toolchain audit task failed: {}", e);
            ToolchainStatus {
                ffmpeg_found: false,
                ffprobe_found: false,
                ffmpeg_version: None,
                ffprobe_version: None,
                bundled: false,
                bin_dir: String::new(),
                ffmpeg_sha256: None,
                ffprobe_sha256: None,
                ffmpeg_path: None,
            }
        }
    }
}

impl ServerState {
    /// Toolchain status, recomputed at most once per minute.
    pub async fn toolchain(&self) -> ToolchainStatus {
        self.toolchain_cache
            .get_or_refresh(refresh_toolchain_status)
            .await
    }

    /// Whether the toolchain is usable, from the cache only.
    ///
    /// Never recomputes: `/api/health` must stay cheap. The cache is seeded at
    /// startup and refreshed by `/api/toolchain` (which the UI polls) and
    /// after a successful download, so this is fresh in practice.
    pub async fn toolchain_ready(&self) -> bool {
        match self.toolchain_cache.peek().await {
            Some(s) => s.ffmpeg_found && s.ffprobe_found,
            None => self.toolchain_status.ffmpeg_found && self.toolchain_status.ffprobe_found,
        }
    }

    /// Database health, recomputed at most once per ten minutes.
    ///
    /// `PRAGMA integrity_check` walks the entire database; `quick_check` does
    /// the structural checks only, which is what a diagnostics endpoint needs
    /// and is orders of magnitude cheaper on a large registry (F-06).
    pub async fn db_health(&self) -> String {
        let pool = self.pool.clone();
        self.db_check_cache
            .get_or_refresh(|| async move {
                sqlx::query_scalar::<_, String>("PRAGMA quick_check(1)")
                    .fetch_one(&*pool)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!("database quick_check failed: {}", e);
                        "error".to_string()
                    })
            })
            .await
    }
}

/// Routes that destroy data or stop ingest, and so require an explicit
/// confirmation header.
///
/// PlayOut already prompts the operator natively before each of these; the
/// header makes that prompt a protocol requirement rather than a client-side
/// convention, so a stray or replayed request cannot empty the library
/// (PlayOut handoff §3.7).
fn is_destructive(method: &axum::http::Method, path: &str) -> bool {
    // Strip the API prefix so v1 and v2 are handled by one table.
    let p = path
        .strip_prefix("/api/v2")
        .or_else(|| path.strip_prefix("/api"))
        .unwrap_or(path);

    match *method {
        axum::http::Method::DELETE => {
            p == "/folders/purge"
                || p == "/recycle-bin/purge"
                || (p.starts_with("/assets/") && p.ends_with("/purge"))
        }
        axum::http::Method::POST => {
            matches!(
                p,
                "/recycle-bin/auto-purge"
                    | "/folders/trash"
                    | "/jobs/retry-failed"
                    | "/service/stop"
            )
        }
        axum::http::Method::PUT => p == "/config" || p == "/folders/trash",
        _ => false,
    }
}

/// Header value that arms a destructive operation.
const CONFIRM_HEADER: &str = "x-confirm-destructive";

async fn require_confirmation(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    if !is_destructive(req.method(), &path) {
        return next.run(req).await;
    }

    let confirmed = req
        .headers()
        .get(CONFIRM_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("yes"))
        .unwrap_or(false);
    if !confirmed {
        return (
            StatusCode::PRECONDITION_REQUIRED,
            [(header::CONTENT_TYPE, "application/json")],
            br#"{"error":"confirmation_required"}"#.to_vec(),
        )
            .into_response();
    }

    let method = req.method().clone();
    let remote = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let response = next.run(req).await;

    // One structured line per destructive operation, on a dedicated target so
    // T2-3 can route it to its own sink and the UI log bridge can surface it.
    tracing::warn!(
        target: "audit",
        op = %method,
        path = %path,
        remote_addr = %remote,
        status = response.status().as_u16(),
        "destructive operation"
    );

    response
}

/// Paths that stay reachable without a token.
///
/// PlayOut polls health every 5 s and treats it as the liveness signal; the web
/// UI needs it to show the status light before the operator has typed the
/// token. Both are cheap and side-effect free, so exempting them costs nothing.
fn is_auth_exempt(path: &str) -> bool {
    matches!(path, "/api/health" | "/api/v2/health")
}

/// Extract a presented token from `X-Api-Token`, `Authorization: Bearer …`, or
/// a `token=` query parameter.
///
/// The query parameter exists only because `EventSource` cannot set headers, so
/// the SSE stream has no other way to authenticate.
fn presented_token(headers: &header::HeaderMap, query: Option<&str>) -> Option<String> {
    if let Some(v) = headers.get("x-api-token").and_then(|v| v.to_str().ok()) {
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    for pair in query.unwrap_or("").split('&') {
        if let Some(v) = pair.strip_prefix("token=") {
            if !v.is_empty() {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

/// Minimal percent-decoding for the `token=` query parameter. The token
/// alphabet is URL-safe base64, so this only has to cope with a client that
/// encoded it anyway.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Require `server.api_token` on every `/api/**` request except the health
/// endpoints.
///
/// Without this, any LAN host (and, before T0-2, any web page the operator had
/// open) could drive every mutating route with no credentials at all — config
/// takeover, library purge, service stop (F-02).
async fn require_token(
    expected: Arc<String>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if expected.is_empty() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if !path.starts_with("/api/") || is_auth_exempt(path) {
        return next.run(req).await;
    }

    let provided = presented_token(req.headers(), req.uri().query());
    let ok = provided
        .as_deref()
        .map(|t| crate::config::tokens_match(&expected, t))
        .unwrap_or(false);
    if !ok {
        // Never log the presented value.
        tracing::warn!("rejected unauthenticated request to {}", path);
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            br#"{"error":"unauthorized"}"#.to_vec(),
        )
            .into_response();
    }
    next.run(req).await
}

/// Browser origins permitted by CORS.
///
/// The bundled SPA is same-origin and needs no CORS at all; PlayOut talks from
/// a Rust `reqwest` client and sends no `Origin`. This list exists only for
/// developers running the Vue dev server, so it stays as small as possible
/// (F-02: `CorsLayer::permissive()` let any web page the operator opened drive
/// every mutating route).
fn allowed_origin_list(port: u16, extra: &[String]) -> Vec<String> {
    let mut out = vec![
        format!("http://127.0.0.1:{}", port),
        format!("http://localhost:{}", port),
        format!("http://[::1]:{}", port),
    ];
    for o in extra {
        let o = o.trim().trim_end_matches('/').to_string();
        if !o.is_empty() && !out.contains(&o) {
            out.push(o);
        }
    }
    out
}

fn build_cors(origins: &[String]) -> CorsLayer {
    let parsed: Vec<header::HeaderValue> = origins
        .iter()
        .filter_map(|o| match o.parse::<header::HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!("ignoring unparseable allowed origin '{}'", o);
                None
            }
        })
        .collect();
    CorsLayer::new()
        .allow_origin(parsed)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-api-token"),
            header::HeaderName::from_static("x-confirm-destructive"),
        ])
}

/// True when `host` (a `Host` header value) names this machine on `port`.
fn is_local_host_header(host: &str, port: u16) -> bool {
    let host = host.trim();
    // Split off the port, taking IPv6 literals (`[::1]:4353`) into account.
    let (name, port_part) = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, tail)) => (inner, tail.strip_prefix(':')),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (host, None),
        }
    };
    if let Some(p) = port_part {
        if p.parse::<u16>() != Ok(port) {
            return false;
        }
    }
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    name.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Reject DNS-rebinding against a loopback-bound service.
///
/// An attacker-controlled name that resolves to 127.0.0.1 would otherwise let a
/// web page reach the API as a same-origin request, bypassing CORS entirely.
async fn host_guard(
    loopback_only: bool,
    port: u16,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if loopback_only {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !host.is_empty() && !is_local_host_header(host, port) {
            tracing::warn!("rejected request with unexpected Host header: {}", host);
            return (
                StatusCode::MISDIRECTED_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                br#"{"error":"misdirected_request"}"#.to_vec(),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Resolve a request path against the SPA root without ever escaping it.
///
/// Axum does not normalise dot segments, and `PathBuf::join` with an absolute
/// or drive-qualified component *replaces* the base on Windows. Both make the
/// naive `root.join(uri.path())` an arbitrary file read (F-01). We therefore
/// rebuild the path from scratch, accepting only plain, non-dot segments.
fn safe_join(root: &std::path::Path, request_path: &str) -> Option<std::path::PathBuf> {
    let mut out = root.to_path_buf();
    let mut segments = 0usize;
    for raw in request_path.split(['/', '\\']) {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            return None;
        }
        // Reject anything that is not a single plain file/dir name: drive
        // letters (`C:`), UNC fragments, NTFS alternate data streams, and any
        // component the OS would interpret as a root or prefix.
        if raw.contains(':') || raw.contains('\0') {
            return None;
        }
        let mut comps = std::path::Path::new(raw).components();
        match (comps.next(), comps.next()) {
            (Some(std::path::Component::Normal(c)), None) => out.push(c),
            _ => return None,
        }
        segments += 1;
        if segments > 32 {
            return None;
        }
    }
    Some(out)
}

fn content_type_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("map") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn serve_index(web_ui_dir: &std::path::Path) -> Response {
    match tokio::fs::read(web_ui_dir.join("index.html")).await {
        Ok(content) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            content,
        )
            .into_response(),
        Err(_) => {
            tracing::warn!(
                "SPA index.html missing under {}; run `cd web-ui && npm install && npm run build`",
                web_ui_dir.display()
            );
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                b"web UI not built".to_vec(),
            )
                .into_response()
        }
    }
}

async fn serve_spa(uri: Uri, State(state): State<ServerState>) -> Response {
    let Some(file_path) = safe_join(state.web_ui_dir.as_path(), uri.path()) else {
        tracing::warn!("rejected SPA path traversal attempt: {}", uri.path());
        return serve_index(state.web_ui_dir.as_path()).await;
    };

    if file_path == *state.web_ui_dir.as_path() {
        return serve_index(state.web_ui_dir.as_path()).await;
    }

    match tokio::fs::read(&file_path).await {
        Ok(content) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type_for(&file_path))],
            content,
        )
            .into_response(),
        // Unknown path: hand the SPA router its index, as before.
        Err(_) => serve_index(state.web_ui_dir.as_path()).await,
    }
}

async fn health(State(state): State<ServerState>) -> Json<serde_json::Value> {
    let uptime_ms = state.started_at.elapsed().as_millis() as u64;
    let toolchain_ready = state.toolchain_ready().await;
    Json(serde_json::json!({
        "status": "ok",
        "service": "PlayoutTranscode",
        "version": env!("CARGO_PKG_VERSION"),
        "toolchain_ready": toolchain_ready,
        "service_running": state.service_handle.is_running(),
        "uptime_ms": uptime_ms,
    }))
}

async fn health_v2(State(state): State<ServerState>) -> impl IntoResponse {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let toolchain_ready = state.toolchain_ready().await;
    Json(serde_json::json!({
        "status": "ok",
        "service": "PlayoutTranscode",
        "api_version": "2.0.0",
        "version": env!("CARGO_PKG_VERSION"),
        "toolchain_ready": toolchain_ready,
        "service_running": state.service_handle.is_running(),
        "uptime_secs": uptime_secs,
    }))
}

async fn get_profiles_v2() -> impl IntoResponse {
    Json(crate::profiles::get_standard_broadcast_profiles())
}

async fn get_job_v2(State(state): State<ServerState>, AssetId(id): AssetId) -> impl IntoResponse {
    if let Some(job) = state.jobs.get(&id) {
        Json(job).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Job not found"})),
        )
            .into_response()
    }
}

async fn get_metrics_v2(State(state): State<ServerState>) -> impl IntoResponse {
    let all = state.jobs.all();
    let pending = all.iter().filter(|j| j.state == JobState::Pending).count();
    let active = all
        .iter()
        .filter(|j| j.state == JobState::Processing)
        .count();
    let completed = all
        .iter()
        .filter(|j| j.state == JobState::Completed)
        .count();
    let failed = all.iter().filter(|j| j.state == JobState::Failed).count();
    let uptime_secs = state.started_at.elapsed().as_secs();

    Json(serde_json::json!({
        "jobs": {
            "pending": pending,
            "active": active,
            "completed": completed,
            "failed": failed,
            "total": all.len(),
        },
        "system": {
            "uptime_secs": uptime_secs,
            "active_pids": state.service_handle.active_pids_count(),
            "service_running": state.service_handle.is_running(),
        }
    }))
}

async fn get_diagnostics(State(state): State<ServerState>) -> impl IntoResponse {
    let tool_status = state.toolchain().await;
    let config = state.config.lock().clone();
    let uptime_secs = state.started_at.elapsed().as_secs();
    let all_jobs = state.jobs.all();
    let db_ok = state.db_health().await;

    Json(serde_json::json!({
        "service": {
            "name": "PlayoutTranscode",
            "version": env!("CARGO_PKG_VERSION"),
            "api_version": "2.0.0",
            "running": state.service_handle.is_running(),
            "uptime_secs": uptime_secs,
            "active_pids": state.service_handle.active_pids_count(),
        },
        "toolchain": tool_status,
        "database": {
            "integrity": db_ok,
        },
        "system": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "logical_cores": crate::config::available_logical_cores(),
            // Where config, the registry, logs and the toolchain actually live
            // (T2-2). Support cannot ask for the right files without it.
            "data_dir": crate::paths::data_dir().to_string_lossy(),
        },
        "metrics": {
            "pending_jobs": all_jobs.iter().filter(|j| j.state == JobState::Pending).count(),
            "active_jobs": all_jobs.iter().filter(|j| j.state == JobState::Processing).count(),
            "completed_jobs": all_jobs.iter().filter(|j| j.state == JobState::Completed).count(),
            "failed_jobs": all_jobs.iter().filter(|j| j.state == JobState::Failed).count(),
            "total_jobs": all_jobs.len(),
        },
        "config_summary": {
            "watch_folder": config.paths.watch_folder,
            "target_folder": config.paths.target_folder,
            "max_concurrency": config.ingestion.max_concurrency,
            "preset": config.encoding.preset,
            "audio_mode": config.audio_policy.map(|p| format!("{:?}", p.mode)).unwrap_or_else(|| "legacy".into()),
        }
    }))
}

async fn list_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.all_recent())
}

async fn list_active_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.active())
}

async fn list_completed_jobs(
    State(state): State<ServerState>,
) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.completed())
}

async fn list_failed_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.failed())
}

async fn list_pending_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.pending())
}

async fn get_config(State(state): State<ServerState>) -> Json<serde_json::Value> {
    let config = state.config.lock();
    let max_concurrency = config.ingestion.max_concurrency;
    let per_encode_threads = config
        .encoding
        .effective_threads_per_encode(max_concurrency);
    let total_threads = config.encoding.effective_total_threads(max_concurrency);
    let available_cores = crate::config::available_logical_cores();

    let effective_audio = config.effective_audio_policy();
    let effective_validation = config.effective_validation_policy();
    let effective_storage = config.effective_storage_policy();
    let effective_retry = config.effective_retry_policy();
    let effective_toolchain = config.effective_toolchain_policy();

    Json(serde_json::json!({
        "version": config.version,
        "paths": {
            "watch_folder": config.paths.watch_folder,
            "target_folder": config.paths.target_folder,
        },
        "server": {
            "web_port": config.server.web_port,
            "bind_address": config.server.bind_address,
            "allowed_origins": config.server.allowed_origins,
            // Never echo the token itself.
            "api_token_set": !config.server.api_token.is_empty(),
        },
        "encoding": {
            "preset": config.encoding.preset,
            "ffmpeg_threads": config.encoding.ffmpeg_threads,
            "cpu_cores": config.encoding.cpu_cores,
            "audio_codec": config.encoding.audio_codec,
            "audio_bitrate": config.encoding.audio_bitrate,
            "tune": config.encoding.tune,
            "probesize": config.encoding.probesize,
            "analyzeduration": config.encoding.analyzeduration,
            // Read-only derived values for the config UI:
            "effective_threads_per_encode": per_encode_threads,
            "effective_total_threads": total_threads,
        },
        "profiles": {
            "a": {
                "enabled": config.profile_a.enabled,
                "crf": config.profile_a.crf,
                "maxrate": config.profile_a.maxrate,
                "bufsize": config.profile_a.bufsize,
            },
            "b": {
                "enabled": config.profile_b.enabled,
                "crf": config.profile_b.crf,
                "maxrate": config.profile_b.maxrate,
                "bufsize": config.profile_b.bufsize,
            },
            "c": {
                "enabled": config.profile_c.enabled,
                "crf": config.profile_c.crf,
                "maxrate": config.profile_c.maxrate,
                "bufsize": config.profile_c.bufsize,
            },
        },
        "ingestion": {
            "settle_secs": config.ingestion.settle_secs,
            "poll_secs": config.ingestion.poll_secs,
            "max_concurrency": config.ingestion.max_concurrency,
            "stable_polls_min": config.ingestion.stable_polls_min,
            "retry_policy": config.ingestion.retry_policy,
            "auto_retry_on_start": config.ingestion.auto_retry_on_start,
            "max_attempts": config.ingestion.max_attempts,
            "retry_delay_ms": config.ingestion.retry_delay_ms,
            "clean_source_after_success": config.ingestion.clean_source_after_success,
        },
        "logging": {
            "level": config.logging.level,
        },
        "system": {
            "available_logical_cores": available_cores,
        },
        "initialized": config.initialized,
        "audio_policy": effective_audio,
        "validation_policy": effective_validation,
        "storage_policy": effective_storage,
        "retry_policy_v2": effective_retry,
        "toolchain_policy": effective_toolchain,
    }))
}

#[derive(Deserialize)]
struct ConfigUpdate {
    /// Accepted for wire compatibility but deliberately ignored: the config
    /// version is derived from which sections are present, never asserted by
    /// the caller.
    #[serde(default)]
    #[allow(dead_code)]
    version: Option<u32>,
    #[serde(default)]
    paths: Option<PathsConfigUpdate>,
    #[serde(default)]
    encoding: Option<EncodingConfigUpdate>,
    #[serde(default)]
    profile_a: Option<ProfileConfigUpdate>,
    #[serde(default)]
    profile_b: Option<ProfileConfigUpdate>,
    #[serde(default)]
    profile_c: Option<ProfileConfigUpdate>,
    #[serde(default)]
    ingestion: Option<IngestionConfigUpdate>,
    #[serde(default)]
    audio_policy: Option<crate::config::AudioPolicy>,
    #[serde(default)]
    validation_policy: Option<crate::config::ValidationPolicy>,
    #[serde(default)]
    storage_policy: Option<crate::config::StoragePolicy>,
    #[serde(default)]
    retry_policy_v2: Option<crate::config::RetryPolicyV2>,
    #[serde(default)]
    toolchain_policy: Option<crate::config::ToolchainPolicy>,
}

#[derive(Deserialize)]
struct PathsConfigUpdate {
    #[serde(default)]
    watch_folder: Option<String>,
    #[serde(default)]
    target_folder: Option<String>,
}

#[derive(Deserialize)]
struct EncodingConfigUpdate {
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    ffmpeg_threads: Option<usize>,
    #[serde(default)]
    cpu_cores: Option<usize>,
    #[serde(default)]
    audio_codec: Option<String>,
    #[serde(default)]
    audio_bitrate: Option<String>,
    #[serde(default)]
    tune: Option<String>,
    #[serde(default)]
    probesize: Option<String>,
    #[serde(default)]
    analyzeduration: Option<String>,
}

#[derive(Deserialize)]
struct ProfileConfigUpdate {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    crf: Option<u8>,
    #[serde(default)]
    maxrate: Option<String>,
    #[serde(default)]
    bufsize: Option<String>,
}

#[derive(Deserialize)]
struct IngestionConfigUpdate {
    #[serde(default)]
    settle_secs: Option<u64>,
    #[serde(default)]
    poll_secs: Option<u64>,
    #[serde(default)]
    max_concurrency: Option<usize>,
    #[serde(default)]
    stable_polls_min: Option<u32>,
    #[serde(default)]
    retry_policy: Option<String>,
    #[serde(default)]
    auto_retry_on_start: Option<bool>,
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    retry_delay_ms: Option<u64>,
    #[serde(default)]
    clean_source_after_success: Option<bool>,
}

/// Apply a `PUT /api/config` patch to a copy of the running config and
/// validate the result.
///
/// Extracted from the handler so it is pure and testable: the handler must
/// never mutate `state.config` or touch the on-disk file unless this returns
/// `Ok`. The old code did the opposite — it mutated the shared config, wrote
/// `config.toml`, and only then validated, so an invalid patch was persisted
/// and silently disabled auto-start at the next boot (F-03).
fn apply_config_patch(current: &AppConfig, body: ConfigUpdate) -> Result<AppConfig, String> {
    let mut config = current.clone();

    // `version` is derived, never taken from the request body.
    if let Some(p) = body.paths {
        if let Some(w) = p.watch_folder {
            config.paths.watch_folder = w;
        }
        if let Some(t) = p.target_folder {
            config.paths.target_folder = t;
        }
    }
    if let Some(e) = body.encoding {
        if let Some(v) = e.preset {
            config.encoding.preset = v;
        }
        if let Some(v) = e.ffmpeg_threads {
            config.encoding.ffmpeg_threads = v;
        }
        if let Some(v) = e.cpu_cores {
            config.encoding.cpu_cores = v;
        }
        if let Some(v) = e.audio_codec {
            config.encoding.audio_codec = v;
        }
        if let Some(v) = e.audio_bitrate {
            config.encoding.audio_bitrate = v;
        }
        if let Some(v) = e.tune {
            config.encoding.tune = v;
        }
        if let Some(v) = e.probesize {
            config.encoding.probesize = v;
        }
        if let Some(v) = e.analyzeduration {
            config.encoding.analyzeduration = v;
        }
    }
    if let Some(p) = body.profile_a {
        if let Some(v) = p.enabled {
            config.profile_a.enabled = v;
        }
        if let Some(v) = p.crf {
            config.profile_a.crf = v;
        }
        if let Some(v) = p.maxrate {
            config.profile_a.maxrate = v;
        }
        if let Some(v) = p.bufsize {
            config.profile_a.bufsize = v;
        }
    }
    if let Some(p) = body.profile_b {
        if let Some(v) = p.enabled {
            config.profile_b.enabled = v;
        }
        if let Some(v) = p.crf {
            config.profile_b.crf = v;
        }
        if let Some(v) = p.maxrate {
            config.profile_b.maxrate = v;
        }
        if let Some(v) = p.bufsize {
            config.profile_b.bufsize = v;
        }
    }
    if let Some(p) = body.profile_c {
        if let Some(v) = p.enabled {
            config.profile_c.enabled = v;
        }
        if let Some(v) = p.crf {
            config.profile_c.crf = v;
        }
        if let Some(v) = p.maxrate {
            config.profile_c.maxrate = v;
        }
        if let Some(v) = p.bufsize {
            config.profile_c.bufsize = v;
        }
    }
    if let Some(i) = body.ingestion {
        if let Some(v) = i.settle_secs {
            config.ingestion.settle_secs = v;
        }
        if let Some(v) = i.poll_secs {
            config.ingestion.poll_secs = v;
        }
        if let Some(v) = i.max_concurrency {
            config.ingestion.max_concurrency = v;
        }
        if let Some(v) = i.stable_polls_min {
            config.ingestion.stable_polls_min = v;
        }
        if let Some(v) = i.retry_policy {
            config.ingestion.retry_policy = v;
        }
        if let Some(v) = i.auto_retry_on_start {
            config.ingestion.auto_retry_on_start = v;
        }
        if let Some(v) = i.max_attempts {
            config.ingestion.max_attempts = v;
        }
        if let Some(v) = i.retry_delay_ms {
            config.ingestion.retry_delay_ms = v;
        }
        if let Some(v) = i.clean_source_after_success {
            config.ingestion.clean_source_after_success = v;
        }
    }

    let mut saw_v2 = false;
    if let Some(ap) = body.audio_policy {
        config.audio_policy = Some(ap);
        saw_v2 = true;
    }
    if let Some(vp) = body.validation_policy {
        config.validation_policy = Some(vp);
        saw_v2 = true;
    }
    if let Some(sp) = body.storage_policy {
        config.storage_policy = Some(sp);
        saw_v2 = true;
    }
    if let Some(rp) = body.retry_policy_v2 {
        config.retry_policy_v2 = Some(rp);
        saw_v2 = true;
    }
    if let Some(tp) = body.toolchain_policy {
        config.toolchain_policy = Some(tp);
        saw_v2 = true;
    }
    if saw_v2 {
        config.version = 2;
    }

    config.initialized = true;
    config.validate()?;
    Ok(config)
}

async fn put_config(
    State(state): State<ServerState>,
    Json(body): Json<ConfigUpdate>,
) -> impl IntoResponse {
    let current = state.config.lock().clone();
    let patched = match apply_config_patch(&current, body) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": format!("Config validation: {}", e)})),
            )
                .into_response();
        }
    };

    let config_path = crate::paths::config_path();
    if let Err(e) = patched.save_to(&config_path) {
        tracing::error!("Failed to save config to {}: {}", config_path.display(), e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "config_save_failed"})),
        )
            .into_response();
    }

    *state.config.lock() = patched;
    Json(serde_json::json!({"success": true})).into_response()
}

async fn get_toolchain_status(State(state): State<ServerState>) -> Json<ToolchainStatus> {
    Json(state.toolchain().await)
}

#[derive(Deserialize)]
struct EventEnvelope {
    event: String,
    data: serde_json::Value,
}

async fn sse_events(
    State(state): State<ServerState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.jobs.event_sender().subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg: Result<String, _>| {
        let msg = msg.ok()?;
        let envelope: EventEnvelope = serde_json::from_str(&msg).ok()?;
        let event = Event::default()
            .event(envelope.event)
            .data(envelope.data.to_string());
        Some(Ok(event))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Serialize)]
struct JobStats {
    pending: usize,
    active: usize,
    completed: usize,
    failed: usize,
    total: usize,
}

async fn get_stats(State(state): State<ServerState>) -> Json<JobStats> {
    let all = state.jobs.all();
    Json(JobStats {
        pending: all.iter().filter(|j| j.state == JobState::Pending).count(),
        active: all
            .iter()
            .filter(|j| j.state == JobState::Processing)
            .count(),
        completed: all
            .iter()
            .filter(|j| j.state == JobState::Completed)
            .count(),
        failed: all.iter().filter(|j| j.state == JobState::Failed).count(),
        total: all.len(),
    })
}

#[derive(Serialize)]
struct WatchfolderInfo {
    watch_folder: String,
    target_folder: String,
    settle_secs: u64,
    poll_secs: u64,
    stable_polls_min: u32,
    retry_policy: String,
    max_concurrency: usize,
}

async fn get_watchfolder(State(state): State<ServerState>) -> Json<WatchfolderInfo> {
    let config = state.config.lock();
    Json(WatchfolderInfo {
        watch_folder: config.paths.watch_folder.clone(),
        target_folder: config.paths.target_folder.clone(),
        settle_secs: config.ingestion.settle_secs,
        poll_secs: config.ingestion.poll_secs,
        stable_polls_min: config.ingestion.stable_polls_min,
        retry_policy: config.ingestion.retry_policy.clone(),
        max_concurrency: config.ingestion.max_concurrency,
    })
}

async fn get_service_status(State(state): State<ServerState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "running": state.service_handle.is_running(),
    }))
}

async fn post_start_service(State(state): State<ServerState>) -> Json<serde_json::Value> {
    if state.service_handle.is_running() {
        return Json(serde_json::json!({ "success": false, "error": "Service already running" }));
    }

    let config = state.config.lock().clone();
    if config.paths.watch_folder.trim().is_empty() || config.paths.target_folder.trim().is_empty() {
        return Json(
            serde_json::json!({ "success": false, "error": "Watch and target folders must be configured first" }),
        );
    }

    // `ensure_toolchain` spawns processes and hashes binaries (F-07).
    let tools = match tokio::task::spawn_blocking(crate::bootstrap::ensure_toolchain).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            return Json(
                serde_json::json!({ "success": false, "error": format!("FFmpeg toolchain: {}", e) }),
            )
        }
        Err(e) => {
            tracing::error!("toolchain check task failed: {}", e);
            return Json(serde_json::json!({ "success": false, "error": "internal_error" }));
        }
    };

    match crate::service_handle::start_processing_loop(
        &state.service_handle,
        &config,
        &state.jobs,
        &tools,
        state.pool.clone(),
    ) {
        Ok(()) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

async fn post_stop_service(State(state): State<ServerState>) -> Json<serde_json::Value> {
    crate::service_handle::stop_processing(&state.service_handle);
    Json(serde_json::json!({ "success": true }))
}

/// Starts the FFmpeg download worker.
///
/// This writes executables into `<exe_dir>/bin` that the service later runs, so
/// it must never be reachable from off-box. It is protected today by the
/// loopback `Host` guard plus the loopback-only bind rule (T0-2); T1-1 adds the
/// API token on top, and T1-2 pins the download itself.
async fn post_download_ffmpeg(State(state): State<ServerState>) -> Json<serde_json::Value> {
    let started = crate::service_handle::trigger_download(&state.service_handle);
    Json(serde_json::json!({ "success": started }))
}

async fn get_download_status(State(state): State<ServerState>) -> Json<serde_json::Value> {
    crate::service_handle::poll_download_status(&state.service_handle);
    let status = state
        .service_handle
        .download_status
        .lock()
        .clone()
        .unwrap_or_else(|| "idle".into());
    Json(serde_json::json!({ "status": status }))
}

async fn get_logs(State(state): State<ServerState>) -> Json<Vec<String>> {
    Json(state.service_handle.get_logs())
}

#[derive(Deserialize)]
struct RetryJobBody {
    /// Optional override for retrying a job whose source is no longer in the watch folder.
    /// If omitted, the job's stored `input_path` is used.
    input_path: Option<String>,
}

async fn post_retry_job(
    State(state): State<ServerState>,
    AssetId(id): AssetId,
    body: Option<Json<RetryJobBody>>,
) -> impl IntoResponse {
    let jobs = state.jobs.all_recent();
    let Some(job) = jobs.into_iter().find(|j| j.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "job not found"})),
        )
            .into_response();
    };
    let path_str: String = body
        .and_then(|b| b.input_path.clone())
        .unwrap_or(job.input_path.clone());
    if path_str.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "no input_path on job"})),
        )
            .into_response();
    }
    // `canonicalize`/`exists` are blocking, and on a UNC path `exists()` opens
    // an outbound SMB connection that leaks an NTLM handshake to whatever host
    // the caller named (F-08). Validate off the async runtime, and only accept
    // a path that really resolves inside the watch folder.
    let watch_folder = state.config.lock().paths.watch_folder.clone();
    let validated = match tokio::task::spawn_blocking(move || {
        validate_retry_input_path(&path_str, &watch_folder)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(reason)) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": reason })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("retry path validation task failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response();
        }
    };
    match state.service_handle.submit_retry(validated) {
        Ok(_) => {
            let _ = state.jobs.transition(
                &id,
                crate::jobs::JobPhase::Queued,
                Some("Re-queued (manual retry)".into()),
                |j| {
                    j.error = None;
                    j.error_category = None;
                    j.stderr_log = None;
                    j.finished_at = None;
                    j.attempt = j.attempt.saturating_add(1);
                },
            );
            Json(serde_json::json!({"success": true})).into_response()
        }
        Err(e) => (StatusCode::CONFLICT, Json(serde_json::json!({"error": e}))).into_response(),
    }
}

async fn post_cancel_job(
    State(state): State<ServerState>,
    AssetId(id): AssetId,
) -> impl IntoResponse {
    match state.jobs.request_cancel(&id) {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn post_retry_all_failed(State(state): State<ServerState>) -> impl IntoResponse {
    let failed = state.jobs.failed();
    // One `exists()` per failed job, on a possibly slow network share, would
    // block a Tokio worker for as long as the whole sweep takes (F-07).
    let paths: Vec<std::path::PathBuf> =
        failed.iter().map(|j| j.input_path.clone().into()).collect();
    let present: Vec<bool> = match tokio::task::spawn_blocking(move || {
        paths.iter().map(|p| p.exists()).collect::<Vec<bool>>()
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("bulk retry stat task failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal_error"})),
            )
                .into_response();
        }
    };

    let mut submitted = 0usize;
    let mut missing = 0usize;
    let mut errors = 0usize;
    for (i, job) in failed.iter().enumerate() {
        let path = std::path::PathBuf::from(&job.input_path);
        if !present.get(i).copied().unwrap_or(false) {
            missing += 1;
            continue;
        }
        match state.service_handle.submit_retry(path) {
            Ok(_) => {
                let _ = state.jobs.transition(
                    &job.id,
                    crate::jobs::JobPhase::Queued,
                    Some("Re-queued (bulk retry)".into()),
                    |j| {
                        j.error = None;
                        j.error_category = None;
                        j.stderr_log = None;
                        j.finished_at = None;
                        j.attempt = j.attempt.saturating_add(1);
                    },
                );
                submitted += 1;
            }
            Err(_) => {
                errors += 1;
            }
        }
    }
    Json(serde_json::json!({
        "submitted": submitted,
        "source_missing": missing,
        "errors": errors,
    }))
    .into_response()
}

/// `POST /api/service/install` and `/uninstall` used to spawn
/// `powershell ... Start-Process sc.exe -Verb RunAs`, popping a UAC prompt on
/// the console session and, if approved, registering a LocalSystem service
/// whose binPath pointed at whatever directory the exe happened to live in.
/// Unauthenticated and reachable cross-site (F-02), that was a
/// social-engineering privilege-escalation path (F-04).
///
/// Service registration belongs to the installer. These stubs stay for one
/// release so an old cached UI gets a clear message instead of a bare 404.
async fn removed_service_endpoint() -> Response {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "success": false,
            "error": "removed; use installer",
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct TrimRequest {
    trim_in_ms: i64,
    trim_out_ms: i64,
}

#[derive(Deserialize)]
struct RatingRequest {
    rating: String,
}

#[derive(Deserialize)]
struct TpRequest {
    tp: String,
}

#[derive(Deserialize)]
struct RenameRequest {
    display_name: String,
}

#[derive(Deserialize)]
struct MoveRequest {
    virtual_folder: String,
}

#[derive(Deserialize)]
struct SubclipRequest {
    display_name: String,
    trim_in_ms: i64,
    trim_out_ms: i64,
}

const MAX_BATCH_UUIDS: usize = 500;

async fn list_assets(
    State(state): State<ServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let status_filter = params.get("status").map(|s| s.as_str());
    match db::find_all(&state.pool, status_filter).await {
        Ok(assets) => {
            let response: Vec<AssetResponse> =
                assets.into_iter().map(AssetResponse::from).collect();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!("DB error on list_assets: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_asset(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
) -> impl IntoResponse {
    match db::find_by_uuid(&state.pool, &uuid).await {
        Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on get_asset: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn put_trim(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<TrimRequest>,
) -> impl IntoResponse {
    if body.trim_in_ms < 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "trim_in_ms must be non-negative"})),
        )
            .into_response();
    }

    let asset = match db::find_by_uuid(&state.pool, &uuid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("DB error on put_trim fetch: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response();
        }
    };

    let duration_ms = asset.duration_ms;
    if duration_ms <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "asset has no resolved duration; cannot set trim"})),
        )
            .into_response();
    }

    let effective_out = if body.trim_out_ms <= 0 {
        duration_ms
    } else {
        body.trim_out_ms
    };

    if effective_out <= body.trim_in_ms {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "trim_out_ms must be greater than trim_in_ms"})),
        )
            .into_response();
    }

    if effective_out > duration_ms {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("trim_out_ms ({}) exceeds duration_ms ({})", effective_out, duration_ms)})),
        ).into_response();
    }

    match db::set_trim(&state.pool, &uuid, body.trim_in_ms, effective_out).await {
        Ok(true) => match db::find_by_uuid(&state.pool, &uuid).await {
            Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response(),
            Err(e) => {
                tracing::error!("DB error on put_trim fetch: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "database error"})),
                )
                    .into_response()
            }
        },
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on put_trim: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn put_rating(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<RatingRequest>,
) -> impl IntoResponse {
    if !db::is_valid_rating(&body.rating) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid rating; must be one of K, 8, 12, 16, 18"})),
        )
            .into_response();
    }
    match db::set_rating(&state.pool, &uuid, &body.rating).await {
        Ok(true) => match db::find_by_uuid(&state.pool, &uuid).await {
            Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response(),
            Err(e) => {
                tracing::error!("DB error on put_rating fetch: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "database error"})),
                )
                    .into_response()
            }
        },
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on put_rating: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn put_tp(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<TpRequest>,
) -> impl IntoResponse {
    if !is_valid_tp(&body.tp) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid tp"})),
        )
            .into_response();
    }
    match db::set_tp(&state.pool, &uuid, &body.tp).await {
        Ok(true) => match db::find_by_uuid(&state.pool, &uuid).await {
            Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response(),
            Err(e) => {
                tracing::error!("DB error on put_tp fetch: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "database error"})),
                )
                    .into_response()
            }
        },
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on put_tp: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_subclip(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<SubclipRequest>,
) -> impl IntoResponse {
    if !is_valid_display_name(&body.display_name) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!(
                "display_name must be 1-{} characters, trimmed, with no control characters",
                db::MAX_DISPLAY_NAME_LEN
            )})),
        )
            .into_response();
    }
    if body.trim_in_ms < 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "trim_in_ms must be non-negative"})),
        )
            .into_response();
    }

    let parent = match db::find_by_uuid(&state.pool, &uuid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "parent asset not found"})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("DB error on post_subclip parent fetch: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response();
        }
    };

    let duration_ms = parent.duration_ms;
    if duration_ms <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "parent asset has no resolved duration"})),
        )
            .into_response();
    }

    let effective_out = if body.trim_out_ms <= 0 {
        duration_ms
    } else {
        body.trim_out_ms
    };

    if effective_out <= body.trim_in_ms {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "trim_out_ms must be greater than trim_in_ms"})),
        )
            .into_response();
    }
    if effective_out > duration_ms {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("trim_out_ms ({}) exceeds parent duration_ms ({})", effective_out, duration_ms)})),
        ).into_response();
    }

    let (sub_mezzanine_ok, sub_warnings) =
        if parent.mezzanine_ok && !parent.keyframe_offsets_json.is_empty() {
            let offsets: Vec<i64> =
                serde_json::from_str(&parent.keyframe_offsets_json).unwrap_or_default();
            let fps = if parent.fps_den > 0 {
                parent.fps_num as f64 / parent.fps_den as f64
            } else {
                parent.fps
            };
            let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 40.0 };
            let tolerance = frame_ms * 0.5;
            let aligned = offsets
                .iter()
                .any(|&kf| (kf - body.trim_in_ms).abs() as f64 <= tolerance);
            if aligned {
                (parent.mezzanine_ok, Vec::new())
            } else {
                (parent.mezzanine_ok, vec!["trim_in_not_keyframe_aligned".to_string()])
            }
        } else {
            (parent.mezzanine_ok, Vec::new())
        };

    let warnings_json = serde_json::to_string(&sub_warnings).unwrap_or_else(|_| "[]".to_string());

    let new_uuid = uuid::Uuid::new_v4().to_string();
    match db::create_subclip(
        &state.pool,
        &new_uuid,
        &uuid,
        &body.display_name,
        body.trim_in_ms,
        effective_out,
        sub_mezzanine_ok,
        &warnings_json,
    )
    .await
    {
        Ok(Some(asset)) => (StatusCode::CREATED, Json(AssetResponse::from(asset))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "parent asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on post_subclip: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TrashFolderRequest {
    pub folder_path: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct RestoreAssetRequest {
    #[serde(default)]
    pub target_folder: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RestoreFolderRequest {
    pub folder_path: String,
    #[serde(default)]
    pub fallback_to_root: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PurgeFolderRequest {
    pub folder_path: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct AutoPurgeRequest {
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub max_age_days: Option<u32>,
}

async fn get_recycle_bin(State(state): State<ServerState>) -> impl IntoResponse {
    match db::list_recycle_bin(&state.pool).await {
        Ok(assets) => {
            let responses: Vec<AssetResponse> = assets.into_iter().map(AssetResponse::from).collect();
            (StatusCode::OK, Json(responses)).into_response()
        }
        Err(e) => {
            tracing::error!("DB error on get_recycle_bin: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_trash_asset(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
) -> impl IntoResponse {
    match db::trash_asset(&state.pool, &uuid).await {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"success": true, "uuid": uuid, "trashed": true})),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found or already trashed"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on post_trash_asset: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_trash_folder(
    State(state): State<ServerState>,
    Json(body): Json<TrashFolderRequest>,
) -> impl IntoResponse {
    if !db::is_valid_virtual_folder(&body.folder_path) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid folder_path"})),
        )
            .into_response();
    }
    match db::trash_folder(&state.pool, &body.folder_path).await {
        Ok(count) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "folder_path": body.folder_path,
                "trashed_count": count
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on post_trash_folder: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_restore_asset(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    body: Option<Json<RestoreAssetRequest>>,
) -> impl IntoResponse {
    let target = body.and_then(|b| b.target_folder.clone());
    // An invalid target used to be silently downgraded to "/", which moved the
    // asset somewhere the caller never asked for (F-08).
    if let Some(t) = target.as_deref() {
        if !db::is_valid_virtual_folder(t) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "invalid target_folder"})),
            )
                .into_response();
        }
    }
    match db::restore_asset(&state.pool, &uuid, target.as_deref()).await {
        Ok(Some(asset)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "asset": AssetResponse::from(asset)
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found in recycle bin"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on post_restore_asset: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_restore_folder(
    State(state): State<ServerState>,
    Json(body): Json<RestoreFolderRequest>,
) -> impl IntoResponse {
    if !db::is_valid_virtual_folder(&body.folder_path) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid folder_path"})),
        )
            .into_response();
    }
    let fallback = body.fallback_to_root.unwrap_or(false);
    match db::restore_folder(&state.pool, &body.folder_path, fallback).await {
        Ok(count) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "folder_path": body.folder_path,
                "restored_count": count,
                "fallback_to_root": fallback
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on post_restore_folder: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn delete_purge_asset(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
) -> impl IntoResponse {
    let mode = if state
        .config
        .lock()
        .effective_storage_policy()
        .preserve_subclips_on_purge
    {
        db::PurgeMode::PreserveReferencedMezzanine
    } else {
        db::PurgeMode::DeleteUnreferencedMezzanine
    };
    let cfg = state.config.lock().clone();
    let target_dir = if !cfg.paths.target_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.target_folder))
    } else {
        None
    };
    let watch_dir = if !cfg.paths.watch_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.watch_folder))
    } else {
        None
    };

    match db::purge_single_asset_with_context(&state.pool, &uuid, mode, target_dir, watch_dir).await {
        Ok(result) => {
            if result.rows_deleted == 0 {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "asset not found", "result": result})),
                )
                    .into_response()
            } else {
                (StatusCode::OK, Json(result)).into_response()
            }
        }
        Err(e) => {
            tracing::error!("DB error during asset purge: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_regenerate_sidecar(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
) -> impl IntoResponse {
    match db::find_by_uuid(&state.pool, &uuid).await {
        Ok(Some(asset)) => {
            if asset.current_path.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Asset has no current_path"})),
                )
                    .into_response();
            }
            // `exists()` and the sidecar write both block; PlayOut writes
            // error bodies verbatim into its diagnostics log, so internal
            // paths and OS error strings stay in `tracing` (F-07, F-09).
            let asset_for_task = asset.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let media_path = std::path::Path::new(&asset_for_task.current_path);
                if !media_path.exists() {
                    return Err(None);
                }
                crate::identity::build_sidecar_from_db_asset(&asset_for_task).map_err(Some)
            })
            .await;

            match outcome {
                Ok(Ok(path)) => {
                    tracing::info!("Regenerated sidecar for asset '{}': {}", uuid, path.display());
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "ok": true,
                            "uuid": uuid,
                        })),
                    )
                        .into_response()
                }
                Ok(Err(None)) => {
                    tracing::warn!(
                        "Sidecar regen for '{}': mezzanine missing at {}",
                        uuid,
                        asset.current_path
                    );
                    (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({"error": "mezzanine_missing"})),
                    )
                        .into_response()
                }
                Ok(Err(Some(e))) => {
                    tracing::error!("Failed to rebuild sidecar for '{}': {}", uuid, e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": "sidecar_write_failed"})),
                    )
                        .into_response()
                }
                Err(e) => {
                    tracing::error!("Sidecar regen task failed for '{}': {}", uuid, e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": "internal_error"})),
                    )
                        .into_response()
                }
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error looking up asset for sidecar regen: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
                .into_response()
        }
    }
}

async fn delete_purge_folder(
    State(state): State<ServerState>,
    Json(body): Json<PurgeFolderRequest>,
) -> impl IntoResponse {
    if !db::is_valid_virtual_folder(&body.folder_path) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid folder_path"})),
        )
            .into_response();
    }
    let mode = if state
        .config
        .lock()
        .effective_storage_policy()
        .preserve_subclips_on_purge
    {
        db::PurgeMode::PreserveReferencedMezzanine
    } else {
        db::PurgeMode::DeleteUnreferencedMezzanine
    };
    let cfg = state.config.lock().clone();
    let target_dir = if !cfg.paths.target_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.target_folder))
    } else {
        None
    };
    let watch_dir = if !cfg.paths.watch_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.watch_folder))
    } else {
        None
    };

    match db::purge_folder_with_context(&state.pool, &body.folder_path, mode, target_dir, watch_dir).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => {
            tracing::error!("DB error during folder purge: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn delete_empty_recycle_bin(State(state): State<ServerState>) -> impl IntoResponse {
    let mode = if state
        .config
        .lock()
        .effective_storage_policy()
        .preserve_subclips_on_purge
    {
        db::PurgeMode::PreserveReferencedMezzanine
    } else {
        db::PurgeMode::DeleteUnreferencedMezzanine
    };
    let cfg = state.config.lock().clone();
    let target_dir = if !cfg.paths.target_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.target_folder))
    } else {
        None
    };
    let watch_dir = if !cfg.paths.watch_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.watch_folder))
    } else {
        None
    };

    match db::purge_recycle_bin_with_context(&state.pool, mode, target_dir, watch_dir).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => {
            tracing::error!("DB error during empty recycle bin: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_auto_purge(
    State(state): State<ServerState>,
    Json(body): Json<AutoPurgeRequest>,
) -> impl IntoResponse {
    let days = if let Some(ref pol) = body.policy {
        match pol.to_ascii_lowercase().as_str() {
            "disabled" | "none" => 0,
            "1week" | "7days" | "7d" => 7,
            "2weeks" | "14days" | "14d" => 14,
            "3weeks" | "21days" | "21d" => 21,
            "1month" | "30days" | "30d" => 30,
            _ => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({"error": "invalid policy, must be one of: disabled, 1week, 2weeks, 3weeks, 1month"})),
                )
                    .into_response();
            }
        }
    } else if let Some(d) = body.max_age_days {
        if d != 0 && d != 7 && d != 14 && d != 21 && d != 30 {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "max_age_days must be 0, 7, 14, 21, or 30"})),
            )
                .into_response();
        }
        d
    } else {
        0
    };

    let mode = if state
        .config
        .lock()
        .effective_storage_policy()
        .preserve_subclips_on_purge
    {
        db::PurgeMode::PreserveReferencedMezzanine
    } else {
        db::PurgeMode::DeleteUnreferencedMezzanine
    };
    let cfg = state.config.lock().clone();
    let target_dir = if !cfg.paths.target_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.target_folder))
    } else {
        None
    };
    let watch_dir = if !cfg.paths.watch_folder.is_empty() {
        Some(std::path::Path::new(&cfg.paths.watch_folder))
    } else {
        None
    };

    match db::auto_purge_expired_with_context(&state.pool, days, mode, target_dir, watch_dir).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => {
            tracing::error!("DB error during auto purge: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn put_rename(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<RenameRequest>,
) -> impl IntoResponse {
    if !is_valid_display_name(&body.display_name) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!(
                "display_name must be 1-{} characters, trimmed, with no control characters",
                db::MAX_DISPLAY_NAME_LEN
            )})),
        )
            .into_response();
    }
    match db::set_display_name(&state.pool, &uuid, &body.display_name).await {
        Ok(true) => match db::find_by_uuid(&state.pool, &uuid).await {
            Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response(),
            Err(e) => {
                tracing::error!("DB error on put_rename fetch: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "database error"})),
                )
                    .into_response()
            }
        },
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on put_rename: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn put_move(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
    Json(body): Json<MoveRequest>,
) -> impl IntoResponse {
    if !db::is_valid_virtual_folder(&body.virtual_folder) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid virtual_folder; must start with '/', must not contain '..', and must not end with '/' unless root"})),
        )
            .into_response();
    }
    match db::set_virtual_folder(&state.pool, &uuid, &body.virtual_folder).await {
        Ok(true) => match db::find_by_uuid(&state.pool, &uuid).await {
            Ok(Some(asset)) => (StatusCode::OK, Json(AssetResponse::from(asset))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "asset not found"})),
            )
                .into_response(),
            Err(e) => {
                tracing::error!("DB error on put_move fetch: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "database error"})),
                )
                    .into_response()
            }
        },
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on put_move: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn post_batch(
    State(state): State<ServerState>,
    Json(body): Json<Vec<String>>,
) -> impl IntoResponse {
    if body.len() > MAX_BATCH_UUIDS {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("max {} UUIDs per batch request", MAX_BATCH_UUIDS)})),
        )
            .into_response();
    }

    let mut seen = HashSet::with_capacity(body.len());
    for uuid in &body {
        if !is_canonical_uuid(uuid) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "invalid asset id in batch request"})),
            )
                .into_response();
        }
        if !seen.insert(uuid) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "duplicate UUIDs in batch request"})),
            )
                .into_response();
        }
    }

    match db::find_batch(&state.pool, &body).await {
        Ok(assets) => {
            let map: serde_json::Map<String, serde_json::Value> = assets
                .into_iter()
                .map(|a| {
                    let uuid = a.uuid.clone();
                    let val = serde_json::to_value(AssetResponse::from(a)).unwrap_or_default();
                    (uuid, val)
                })
                .collect();
            (StatusCode::OK, Json(serde_json::Value::Object(map))).into_response()
        }
        Err(e) => {
            tracing::error!("DB error on post_batch: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct SetFolderColorRequest {
    virtual_folder: String,
    color: String,
}

async fn get_folder_colors(State(state): State<ServerState>) -> impl IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    match db::get_all_folder_colors(&state.pool).await {
        Ok(colors) => (StatusCode::OK, headers, Json(colors)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_folder_colors: {}", e);
            (StatusCode::OK, headers, Json(serde_json::json!([]))).into_response()
        }
    }
}

async fn put_folder_color(
    State(state): State<ServerState>,
    Json(body): Json<SetFolderColorRequest>,
) -> impl IntoResponse {
    if !db::is_valid_virtual_folder(&body.virtual_folder) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid virtual_folder"})),
        )
            .into_response();
    }
    if !is_valid_folder_color(&body.color) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "invalid color; expected #rrggbb or a named colour"})),
        )
            .into_response();
    }
    match db::set_folder_color(&state.pool, &body.virtual_folder, &body.color).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::error!("DB error on put_folder_color: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

// ── DB Viewer API Handlers ───────────────────────────────────────────────────

async fn get_db_overview_handler(State(state): State<ServerState>) -> impl IntoResponse {
    match db::get_db_overview(&state.pool).await {
        Ok(overview) => (StatusCode::OK, Json(overview)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_overview: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_assets_handler(
    State(state): State<ServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let filter = params.get("filter").map(|s| s.as_str());
    let search = params.get("search").map(|s| s.as_str());
    let limit = params.get("limit").and_then(|s| s.parse::<i64>().ok());
    let offset = params.get("offset").and_then(|s| s.parse::<i64>().ok());

    match db::query_db_assets(&state.pool, filter, search, limit, offset).await {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_assets: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_asset_detail_handler(
    State(state): State<ServerState>,
    AssetId(uuid): AssetId,
) -> impl IntoResponse {
    match db::get_db_asset_detail(&state.pool, &uuid).await {
        Ok(Some(detail)) => (StatusCode::OK, Json(detail)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_asset_detail: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_jobs_handler(
    State(state): State<ServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let state_filter = params.get("state").map(|s| s.as_str());
    let search = params.get("search").map(|s| s.as_str());
    let limit = params.get("limit").and_then(|s| s.parse::<i64>().ok());
    let offset = params.get("offset").and_then(|s| s.parse::<i64>().ok());

    match db::query_db_jobs(&state.pool, state_filter, search, limit, offset).await {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_jobs: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_job_detail_handler(
    State(state): State<ServerState>,
    AssetId(id): AssetId,
) -> impl IntoResponse {
    match db::get_db_job_detail(&state.pool, &id).await {
        Ok(Some(detail)) => (StatusCode::OK, Json(detail)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "job not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_job_detail: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_folders_handler(State(state): State<ServerState>) -> impl IntoResponse {
    match db::get_db_folders(&state.pool).await {
        Ok(folders) => (StatusCode::OK, Json(folders)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_folders: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

async fn get_db_schema_handler(State(state): State<ServerState>) -> impl IntoResponse {
    match db::get_db_schema(&state.pool).await {
        Ok(schema) => (StatusCode::OK, Json(schema)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_db_schema: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn root() -> &'static Path {
        Path::new(if cfg!(windows) {
            r"C:\app\web-ui\dist"
        } else {
            "/app/web-ui/dist"
        })
    }

    #[test]
    fn safe_join_allows_plain_assets() {
        let p = safe_join(root(), "/assets/index-abc123.js").expect("allowed");
        assert!(p.starts_with(root()));
        assert!(p.ends_with("index-abc123.js"));
    }

    #[test]
    fn safe_join_rejects_dot_dot_traversal() {
        assert!(safe_join(root(), "/../config.toml").is_none());
        assert!(safe_join(root(), "/assets/../../media_assets.db").is_none());
        assert!(safe_join(root(), "/..%2fconfig.toml").is_some()); // not decoded => literal name
        assert!(safe_join(root(), "/a/b/../../../etc/passwd").is_none());
    }

    #[test]
    fn safe_join_rejects_absolute_and_drive_paths() {
        assert!(safe_join(root(), "/C:/Windows/win.ini").is_none());
        assert!(safe_join(root(), "/c:\\windows\\win.ini").is_none());
        assert!(safe_join(root(), "/index.html:stream").is_none());
    }

    #[test]
    fn safe_join_normalises_root_and_dot_segments() {
        assert_eq!(safe_join(root(), "/").unwrap(), root().to_path_buf());
        assert_eq!(safe_join(root(), "").unwrap(), root().to_path_buf());
        assert_eq!(
            safe_join(root(), "/./assets/./app.css").unwrap(),
            root().join("assets").join("app.css")
        );
    }

    #[test]
    fn safe_join_rejects_absurd_depth() {
        let deep = "/a".repeat(64);
        assert!(safe_join(root(), &deep).is_none());
    }

    #[test]
    fn destructive_route_table() {
        use axum::http::Method;

        let destructive = [
            (Method::DELETE, "/api/folders/purge"),
            (Method::DELETE, "/api/recycle-bin/purge"),
            (Method::DELETE, "/api/assets/3f2504e0-4f89-41d3-9a0c-0305e82c3301/purge"),
            (Method::POST, "/api/recycle-bin/auto-purge"),
            (Method::POST, "/api/folders/trash"),
            (Method::POST, "/api/jobs/retry-failed"),
            (Method::POST, "/api/service/stop"),
            (Method::PUT, "/api/config"),
            (Method::PUT, "/api/folders/trash"),
            // v2 resolves through the same table.
            (Method::DELETE, "/api/v2/folders/purge"),
            (Method::PUT, "/api/v2/config"),
        ];
        for (m, p) in destructive {
            assert!(is_destructive(&m, p), "{} {} should be destructive", m, p);
        }

        let safe = [
            (Method::GET, "/api/config"),
            (Method::GET, "/api/health"),
            (Method::GET, "/api/recycle-bin"),
            (Method::POST, "/api/service/start"),
            (Method::POST, "/api/folders/restore"),
            (Method::POST, "/api/assets/3f2504e0-4f89-41d3-9a0c-0305e82c3301/trash"),
            (Method::POST, "/api/assets/3f2504e0-4f89-41d3-9a0c-0305e82c3301/restore"),
            (Method::PUT, "/api/assets/3f2504e0-4f89-41d3-9a0c-0305e82c3301/rename"),
            (Method::DELETE, "/api/assets/3f2504e0-4f89-41d3-9a0c-0305e82c3301"),
            // A path that merely mentions purge is not a purge route.
            (Method::GET, "/api/db/assets?search=purge"),
            (Method::POST, "/purge"),
        ];
        for (m, p) in safe {
            assert!(!is_destructive(&m, p), "{} {} should not be destructive", m, p);
        }
    }

    #[test]
    fn canonical_uuid_grammar() {
        assert!(is_canonical_uuid("3f2504e0-4f89-41d3-9a0c-0305e82c3301"));
        assert!(is_canonical_uuid("3F2504E0-4F89-41D3-9A0C-0305E82C3301"));

        for bad in [
            "",
            "3f2504e0-4f89-41d3-9a0c-0305e82c330",
            "3f2504e0-4f89-41d3-9a0c-0305e82c33011",
            "3f2504e04f8941d39a0c0305e82c3301",
            "3f2504e0-4f89-41d3-9a0c-0305e82c330g",
            "3f2504e0_4f89_41d3_9a0c_0305e82c3301",
            "../../config.toml",
            "' OR 1=1 --",
        ] {
            assert!(!is_canonical_uuid(bad), "{:?} must be rejected", bad);
        }
    }

    #[test]
    fn tp_grammar() {
        for ok in ["", "TP", "SHOW", "TP|18:00", "A,B.C [x] {y} \"z\"", "x"] {
            assert!(is_valid_tp(ok), "{:?} should be valid", ok);
        }
        for bad in [
            "<script>",
            "drop\u{0}table",
            "new\nline",
            "tab\there",
            "emoji \u{1F600}",
        ] {
            assert!(!is_valid_tp(bad), "{:?} should be invalid", bad);
        }
        assert!(is_valid_tp(&"x".repeat(MAX_TP_LEN)));
        assert!(!is_valid_tp(&"x".repeat(MAX_TP_LEN + 1)));
    }

    #[test]
    fn folder_color_grammar() {
        for ok in ["", "#a1b2c3", "#FFFFFF", "blue", "GREY"] {
            assert!(is_valid_folder_color(ok), "{:?} should be valid", ok);
        }
        for bad in [
            "#12345",
            "#1234567",
            "#gggggg",
            "rgb(1,2,3)",
            "red; background: url(http://evil.example/x)",
            "expression(alert(1))",
            "chartreuse",
        ] {
            assert!(!is_valid_folder_color(bad), "{:?} should be invalid", bad);
        }
    }

    #[test]
    fn display_name_grammar() {
        assert!(is_valid_display_name("Promo 2026 - Final"));
        assert!(is_valid_display_name("Ειδήσεις 20:00"));
        assert!(is_valid_display_name(&"x".repeat(db::MAX_DISPLAY_NAME_LEN)));

        for bad in [
            "",
            " leading",
            "trailing ",
            "line\nbreak",
            "bell\u{7}",
        ] {
            assert!(!is_valid_display_name(bad), "{:?} should be invalid", bad);
        }
        assert!(!is_valid_display_name(&"x".repeat(db::MAX_DISPLAY_NAME_LEN + 1)));
    }

    #[test]
    fn retry_input_path_rules() {
        let dir = std::env::temp_dir().join(format!("pt-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let watch = dir.join("watch");
        std::fs::create_dir_all(&watch).unwrap();
        let inside = watch.join("clip.mp4");
        std::fs::write(&inside, b"x").unwrap();
        let outside = dir.join("elsewhere.mp4");
        std::fs::write(&outside, b"x").unwrap();

        let watch_str = watch.to_string_lossy().to_string();

        assert!(validate_retry_input_path(&inside.to_string_lossy(), &watch_str).is_ok());

        // A UNC path must be refused before anything touches the filesystem:
        // `exists()` on one leaks an NTLM handshake to the named host.
        assert_eq!(
            validate_retry_input_path("\\\\evil.example\\share\\x.mp4", &watch_str),
            Err("UNC paths are not accepted")
        );
        assert_eq!(
            validate_retry_input_path("//evil.example/share/x.mp4", &watch_str),
            Err("UNC paths are not accepted")
        );
        assert_eq!(
            validate_retry_input_path("relative/clip.mp4", &watch_str),
            Err("input_path must be absolute")
        );
        assert_eq!(
            validate_retry_input_path("", &watch_str),
            Err("input_path must not be empty")
        );
        assert_eq!(
            validate_retry_input_path(&outside.to_string_lossy(), &watch_str),
            Err("input_path must be inside the watch folder")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn patch_test_config(tag: &str) -> (AppConfig, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pt-patch-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let watch = dir.join("watch");
        std::fs::create_dir_all(&watch).unwrap();
        let mut cfg = AppConfig::default();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = dir.join("target").to_string_lossy().to_string();
        assert!(cfg.validate().is_ok(), "fixture must be valid");
        (cfg, dir)
    }

    fn patch_from(json: serde_json::Value) -> ConfigUpdate {
        serde_json::from_value(json).expect("patch body")
    }

    #[test]
    fn config_patch_rejects_target_inside_watch() {
        let (cfg, dir) = patch_test_config("overlap");
        let nested = std::path::Path::new(&cfg.paths.watch_folder).join("out");
        let err = apply_config_patch(
            &cfg,
            patch_from(serde_json::json!({
                "paths": { "target_folder": nested.to_string_lossy() }
            })),
        )
        .expect_err("target inside watch must be rejected");
        assert!(err.contains("overlap"), "unexpected message: {}", err);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_patch_rejects_bad_ffmpeg_strings() {
        let (cfg, dir) = patch_test_config("ffstr");
        assert!(apply_config_patch(
            &cfg,
            patch_from(serde_json::json!({ "encoding": { "tune": "nope" } }))
        )
        .is_err());
        assert!(apply_config_patch(
            &cfg,
            patch_from(serde_json::json!({ "profile_a": { "maxrate": "15M -f null -" } }))
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_patch_ignores_caller_supplied_version() {
        let (cfg, dir) = patch_test_config("version");
        let before = cfg.version;
        let patched = apply_config_patch(&cfg, patch_from(serde_json::json!({ "version": 99 })))
            .expect("empty patch is valid");
        assert_eq!(patched.version, before, "version must be derived, not taken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_patch_applies_valid_changes() {
        let (cfg, dir) = patch_test_config("apply");
        let patched = apply_config_patch(
            &cfg,
            patch_from(serde_json::json!({
                "encoding": { "preset": "veryfast", "tune": "film" },
                "ingestion": { "max_concurrency": 3 }
            })),
        )
        .expect("valid patch");
        assert_eq!(patched.encoding.preset, "veryfast");
        assert_eq!(patched.encoding.tune, "film");
        assert_eq!(patched.ingestion.max_concurrency, 3);
        assert!(patched.initialized);
        // The source config is untouched.
        assert_eq!(cfg.ingestion.max_concurrency, AppConfig::default().ingestion.max_concurrency);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn removed_service_endpoints_return_410() {
        let resp = removed_service_endpoint().await;
        assert_eq!(resp.status(), StatusCode::GONE);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["success"], serde_json::json!(false));
        assert_eq!(json["error"], serde_json::json!("removed; use installer"));
    }

    #[test]
    fn allowed_origins_cover_loopback_and_extras() {
        let list = allowed_origin_list(4353, &["http://localhost:5173/".to_string()]);
        assert!(list.contains(&"http://127.0.0.1:4353".to_string()));
        assert!(list.contains(&"http://localhost:4353".to_string()));
        assert!(list.contains(&"http://[::1]:4353".to_string()));
        assert!(list.contains(&"http://localhost:5173".to_string()));
        assert!(!list.iter().any(|o| o.contains("evil")));
    }

    #[test]
    fn host_header_guard_accepts_only_loopback_names() {
        assert!(is_local_host_header("127.0.0.1:4353", 4353));
        assert!(is_local_host_header("localhost:4353", 4353));
        assert!(is_local_host_header("LOCALHOST", 4353));
        assert!(is_local_host_header("[::1]:4353", 4353));
        assert!(is_local_host_header("127.0.0.1", 4353));

        assert!(!is_local_host_header("evil.example", 4353));
        assert!(!is_local_host_header("evil.localtest.me:4353", 4353));
        assert!(!is_local_host_header("127.0.0.1:9999", 4353));
        assert!(!is_local_host_header("192.168.1.10:4353", 4353));
        assert!(!is_local_host_header("[::1:4353", 4353));
    }

    #[test]
    fn content_type_is_derived_from_extension() {
        assert_eq!(
            content_type_for(Path::new("x/app.JS")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("x/app.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type_for(Path::new("x/logo.svg")), "image/svg+xml");
        assert_eq!(
            content_type_for(Path::new("x/blob")),
            "application/octet-stream"
        );
    }
}
