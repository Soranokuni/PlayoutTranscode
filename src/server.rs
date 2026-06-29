use crate::bootstrap::ToolchainStatus;
use crate::config::AppConfig;
use crate::db::{self, AssetResponse};
use crate::jobs::{JobQueue, JobState};
use crate::service_handle::ServiceHandle;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post, put, delete},
    Json, Router,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

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
        .route("/config", get(get_config))
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
        .route("/folders/colors", get(get_folder_colors).put(put_folder_color));

    let app = Router::new()
        .nest("/api", api)
        .fallback(serve_spa)
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
                (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], content).into_response()
            } else {
                (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], format!("SPA not found at {}. Run: cd web-ui && npm install && npm run build", state.web_ui_dir.display()).as_bytes().to_vec()).into_response()
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

async fn list_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.all_recent())
}

async fn list_active_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
    Json(state.jobs.active())
}

async fn list_completed_jobs(State(state): State<ServerState>) -> Json<Vec<crate::jobs::JobRecord>> {
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
    Json(serde_json::json!({
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
            "audio_codec": config.encoding.audio_codec,
            "audio_bitrate": config.encoding.audio_bitrate,
            "tune": config.encoding.tune,
            "probesize": config.encoding.probesize,
            "analyzeduration": config.encoding.analyzeduration,
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
            "clean_source_after_success": config.ingestion.clean_source_after_success,
        },
        "logging": {
            "level": config.logging.level,
        },
    }))
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

async fn sse_events(State(state): State<ServerState>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
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
        active: all.iter().filter(|j| j.state == JobState::Processing).count(),
        completed: all.iter().filter(|j| j.state == JobState::Completed).count(),
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
        return Json(serde_json::json!({ "success": false, "error": "Watch and target folders must be configured first" }));
    }

    let tools = match crate::bootstrap::ensure_toolchain() {
        Ok(t) => t,
        Err(e) => return Json(serde_json::json!({ "success": false, "error": format!("FFmpeg toolchain: {}", e) })),
    };

    match crate::service_handle::start_processing_loop(&state.service_handle, &config, &state.jobs, &tools, state.pool.clone()) {
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
    let status = state.service_handle.download_status.lock().clone().unwrap_or_else(|| "idle".into());
    Json(serde_json::json!({ "status": status }))
}

async fn get_logs(State(state): State<ServerState>) -> Json<Vec<String>> {
    Json(state.service_handle.get_logs())
}

async fn post_install_service(State(_state): State<ServerState>) -> Json<serde_json::Value> {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("PlayoutTranscode.exe"));
    let exe_path = exe.to_string_lossy().replace('\'', "''");
    let config_path = exe.parent()
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
            let _ = std::process::Command::new("powershell").args(["-NoProfile", "-Command", desc_script]).output();
            let start_script = "$a = @('start','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
            let _ = std::process::Command::new("powershell").args(["-NoProfile", "-Command", start_script]).output();
            Json(serde_json::json!({ "success": true, "message": "Installed as Windows Service" }))
        }
        Ok(o) => Json(serde_json::json!({ "success": false, "error": String::from_utf8_lossy(&o.stderr) })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": format!("powershell error: {}", e) })),
    }
}

async fn post_uninstall_service(State(_state): State<ServerState>) -> Json<serde_json::Value> {
    let stop_script = "$a = @('stop','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
    let _ = std::process::Command::new("powershell").args(["-NoProfile", "-Command", stop_script]).output();

    let del_script = "$a = @('delete','PlayoutTranscode'); Start-Process sc.exe -ArgumentList $a -Verb RunAs -Wait";
    let o = std::process::Command::new("powershell").args(["-NoProfile", "-Command", del_script]).output();
    match o {
        Ok(o) if o.status.success() => Json(serde_json::json!({ "success": true, "message": "Uninstalled" })),
        Ok(o) => Json(serde_json::json!({ "success": false, "error": String::from_utf8_lossy(&o.stderr) })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": format!("powershell error: {}", e) })),
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
            let response: Vec<AssetResponse> = assets.into_iter().map(AssetResponse::from).collect();
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
    match db::set_trim(&state.pool, &uuid, body.trim_in_ms, body.trim_out_ms).await {
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
        )
            .into_response();
    }
    
    let new_uuid = uuid::Uuid::new_v4().to_string();
    match db::create_subclip(
        &state.pool,
        &new_uuid,
        &uuid,
        &body.display_name,
        body.trim_in_ms,
        body.trim_out_ms,
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
    match db::find_by_uuid(&state.pool, &uuid).await {
        Ok(Some(asset)) => {
            let path_str = asset.current_path.clone();
            if !path_str.is_empty() {
                let path = std::path::Path::new(&path_str);
                if let Err(e) = tokio::fs::remove_file(path).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!("Failed to remove physical file at {}: {}", path_str, e);
                    }
                }
            }
            
            match db::purge_asset_by_path_or_fingerprint(&state.pool, &asset.current_path, asset.fingerprint).await {
                Ok(rows) => {
                    (StatusCode::OK, Json(serde_json::json!({
                        "success": true,
                        "purged_records": rows
                    }))).into_response()
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
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "asset not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("DB error finding asset for purge: {}", e);
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
    match db::get_all_folder_colors(&state.pool).await {
        Ok(colors) => (StatusCode::OK, Json(colors)).into_response(),
        Err(e) => {
            tracing::error!("DB error on get_folder_colors: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database error"})),
            )
                .into_response()
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

