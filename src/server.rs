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
}

pub async fn run_server(
    port: u16,
    bind_address: &str,
    jobs: JobQueue,
    config: AppConfig,
    toolchain_status: ToolchainStatus,
    service_handle: ServiceHandle,
    web_ui_dir: std::path::PathBuf,
    pool: Arc<SqlitePool>,
) -> Result<(), String> {
    let state = ServerState {
        jobs: jobs.clone(),
        config: Arc::new(Mutex::new(config)),
        toolchain_status: Arc::new(toolchain_status),
        service_handle,
        web_ui_dir: Arc::new(web_ui_dir),
        pool,
        started_at: std::time::Instant::now(),
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
        .route("/events", get(sse_events))
        .route("/stats", get(get_stats))
        .route("/watchfolder", get(get_watchfolder))
        .route("/service/status", get(get_service_status))
        .route("/service/start", post(post_start_service))
        .route("/service/stop", post(post_stop_service))
        .route("/download/start", post(post_download_ffmpeg))
        .route("/download/status", get(get_download_status))
        .route("/logs", get(get_logs))
        .route("/diagnostics", get(get_diagnostics))
        .route("/service/install", post(post_install_service))
        .route("/service/uninstall", post(post_uninstall_service))
        .route("/assets", get(list_assets))
        .route("/assets/{uuid}", get(get_asset))
        .route("/assets/{uuid}/trim", put(put_trim))
        .route("/assets/{uuid}/rating", put(put_rating))
        .route("/assets/{uuid}/tp", put(put_tp))
        .route("/assets/{uuid}/rename", put(put_rename))
        .route("/assets/{uuid}/move", put(put_move))
        .route("/assets/{uuid}/subclip", post(post_subclip))
        .route("/assets/{uuid}/purge", delete(delete_purge_asset))
        .route("/assets/batch", post(post_batch))
        .route(
            "/folders/colors",
            get(get_folder_colors).put(put_folder_color),
        );

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
        .route("/events", get(sse_events))
        .route("/metrics", get(get_metrics_v2))
        .route("/diagnostics", get(get_diagnostics));

    let app = Router::new()
        .nest("/api/v2", api_v2)
        .nest("/api", api)
        .fallback(serve_spa)
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("{}:{}", bind_address, port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    tracing::info!("PlayoutTranscode web UI listening on http://{}", addr);

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Server error: {}", e))
}

async fn serve_spa(uri: Uri, State(state): State<ServerState>) -> Response {
    let path = uri.path().trim_start_matches('/');
    let file_path = if path.is_empty() {
        state.web_ui_dir.join("index.html")
    } else {
        state.web_ui_dir.join(path)
    };

    match tokio::fs::read(&file_path).await {
        Ok(content) => {
            let fname = file_path.to_string_lossy();
            let ct = if fname.ends_with(".js") || fname.ends_with(".mjs") {
                "application/javascript; charset=utf-8"
            } else if fname.ends_with(".css") {
                "text/css; charset=utf-8"
            } else if fname.ends_with(".html") {
                "text/html; charset=utf-8"
            } else if fname.ends_with(".svg") {
                "image/svg+xml"
            } else if fname.ends_with(".png") {
                "image/png"
            } else if fname.ends_with(".ico") {
                "image/x-icon"
            } else if fname.ends_with(".woff2") {
                "font/woff2"
            } else {
                "application/octet-stream"
            };
            (StatusCode::OK, [(header::CONTENT_TYPE, ct)], content).into_response()
        }
        Err(_) => {
            let index_path = state.web_ui_dir.join("index.html");
            if let Ok(content) = tokio::fs::read(&index_path).await {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content,
                )
                    .into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "text/plain")],
                    format!(
                        "SPA not found at {}. Run: cd web-ui && npm install && npm run build",
                        state.web_ui_dir.display()
                    )
                    .as_bytes()
                    .to_vec(),
                )
                    .into_response()
            }
        }
    }
}

async fn health(State(state): State<ServerState>) -> Json<serde_json::Value> {
    let uptime_ms = state.started_at.elapsed().as_millis() as u64;
    Json(serde_json::json!({
        "status": "ok",
        "service": "PlayoutTranscode",
        "version": env!("CARGO_PKG_VERSION"),
        "toolchain_ready": state.toolchain_status.ffmpeg_found && state.toolchain_status.ffprobe_found,
        "service_running": state.service_handle.is_running(),
        "uptime_ms": uptime_ms,
    }))
}

async fn health_v2(State(state): State<ServerState>) -> impl IntoResponse {
    let uptime_secs = state.started_at.elapsed().as_secs();
    Json(serde_json::json!({
        "status": "ok",
        "service": "PlayoutTranscode",
        "api_version": "2.0.0",
        "version": env!("CARGO_PKG_VERSION"),
        "toolchain_ready": state.toolchain_status.ffmpeg_found && state.toolchain_status.ffprobe_found,
        "service_running": state.service_handle.is_running(),
        "uptime_secs": uptime_secs,
    }))
}

async fn get_profiles_v2() -> impl IntoResponse {
    Json(crate::profiles::get_standard_broadcast_profiles())
}

async fn get_job_v2(State(state): State<ServerState>, Path(id): Path<String>) -> impl IntoResponse {
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
    let (_, tool_status) = crate::bootstrap::audit_toolchain();
    let config = state.config.lock().clone();
    let uptime_secs = state.started_at.elapsed().as_secs();
    let all_jobs = state.jobs.all();

    let db_ok = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
        .fetch_one(&*state.pool)
        .await
        .unwrap_or_else(|e| format!("error: {}", e));

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
    #[serde(default)]
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

async fn put_config(
    State(state): State<ServerState>,
    Json(body): Json<ConfigUpdate>,
) -> impl IntoResponse {
    {
        let mut config = state.config.lock();
        if let Some(v) = body.version {
            config.version = v;
        }
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
        if let Some(ap) = body.audio_policy {
            config.audio_policy = Some(ap);
            config.version = 2;
        }
        if let Some(vp) = body.validation_policy {
            config.validation_policy = Some(vp);
            config.version = 2;
        }
        if let Some(sp) = body.storage_policy {
            config.storage_policy = Some(sp);
            config.version = 2;
        }
        if let Some(rp) = body.retry_policy_v2 {
            config.retry_policy_v2 = Some(rp);
            config.version = 2;
        }
        if let Some(tp) = body.toolchain_policy {
            config.toolchain_policy = Some(tp);
            config.version = 2;
        }

        config.initialized = true;

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let config_path = exe_dir.join("config.toml");
        if let Err(e) = config.save_to(&config_path) {
            tracing::error!("Failed to save config: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to save config: {}", e)})),
            )
                .into_response();
        }
    }

    let config = state.config.lock().clone();
    if let Err(e) = config.validate() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("Config validation: {}", e)})),
        )
            .into_response();
    }

    Json(serde_json::json!({"success": true})).into_response()
}

async fn get_toolchain_status(State(_state): State<ServerState>) -> Json<ToolchainStatus> {
    let (_, status) = crate::bootstrap::audit_toolchain();
    Json(status)
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

    let tools = match crate::bootstrap::ensure_toolchain() {
        Ok(t) => t,
        Err(e) => {
            return Json(
                serde_json::json!({ "success": false, "error": format!("FFmpeg toolchain: {}", e) }),
            )
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
    Path(id): Path<String>,
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
    let path = std::path::PathBuf::from(&path_str);
    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::json!({"error": format!("source file no longer exists: {}", path_str)}),
            ),
        )
            .into_response();
    }
    match state.service_handle.submit_retry(path) {
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
    Path(id): Path<String>,
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
    let mut submitted = 0usize;
    let mut missing = 0usize;
    let mut errors = 0usize;
    for job in &failed {
        let path = std::path::PathBuf::from(&job.input_path);
        if !path.exists() {
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
}

async fn post_install_service(State(_state): State<ServerState>) -> Json<serde_json::Value> {
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("PlayoutTranscode.exe"));
    let exe_path = exe.to_string_lossy().replace('\'', "''");
    let config_path = exe
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("config.toml")
        .to_string_lossy()
        .replace('\'', "''");

    let ps_script = format!(
        "$a = @('create','PlayoutTranscode','binPath= \"{}\" run --config \"{}\"','start= auto','DisplayName= PlayoutTranscode Media Service'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait",
        exe_path, config_path
    );

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_script])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let desc_script = "$a = @('description','PlayoutTranscode','Automated broadcast media transcoding service'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", desc_script])
                .output();
            let start_script = "$a = @('start','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", start_script])
                .output();
            Json(serde_json::json!({ "success": true, "message": "Installed as Windows Service" }))
        }
        Ok(o) => Json(
            serde_json::json!({ "success": false, "error": String::from_utf8_lossy(&o.stderr) }),
        ),
        Err(e) => Json(
            serde_json::json!({ "success": false, "error": format!("powershell error: {}", e) }),
        ),
    }
}

async fn post_uninstall_service(State(_state): State<ServerState>) -> Json<serde_json::Value> {
    let stop_script = "$a = @('stop','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", stop_script])
        .output();

    let del_script = "$a = @('delete','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
    let o = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", del_script])
        .output();
    match o {
        Ok(o) if o.status.success() => {
            Json(serde_json::json!({ "success": true, "message": "Uninstalled" }))
        }
        Ok(o) => Json(
            serde_json::json!({ "success": false, "error": String::from_utf8_lossy(&o.stderr) }),
        ),
        Err(e) => Json(
            serde_json::json!({ "success": false, "error": format!("powershell error: {}", e) }),
        ),
    }
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
    Path(uuid): Path<String>,
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
    Path(uuid): Path<String>,
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
    Path(uuid): Path<String>,
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
    Path(uuid): Path<String>,
    Json(body): Json<TpRequest>,
) -> impl IntoResponse {
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
    Path(uuid): Path<String>,
    Json(body): Json<SubclipRequest>,
) -> impl IntoResponse {
    if body.display_name.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "display_name must not be empty"})),
        )
            .into_response();
    }
    if body.display_name.len() > db::MAX_DISPLAY_NAME_LEN {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("display_name must not exceed {} characters", db::MAX_DISPLAY_NAME_LEN)})),
        ).into_response();
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

async fn delete_purge_asset(
    State(state): State<ServerState>,
    Path(uuid): Path<String>,
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
    match db::purge_asset_with_mode(&state.pool, &uuid, mode).await {
        Ok(outcome) => {
            if outcome.rows_deleted == 0 {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "asset not found"})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "success": true,
                        "purged_records": outcome.rows_deleted,
                        "file_removed": outcome.file_removed,
                        "sidecar_removed": outcome.sidecar_removed,
                    })),
                )
                    .into_response()
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

async fn put_rename(
    State(state): State<ServerState>,
    Path(uuid): Path<String>,
    Json(body): Json<RenameRequest>,
) -> impl IntoResponse {
    if body.display_name.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "display_name must not be empty"})),
        )
            .into_response();
    }
    if body.display_name.len() > db::MAX_DISPLAY_NAME_LEN {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("display_name must not exceed {} characters", db::MAX_DISPLAY_NAME_LEN)})),
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
    Path(uuid): Path<String>,
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
