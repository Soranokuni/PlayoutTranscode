use serde::Serialize;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MediaAsset {
    pub uuid: String,
    pub fingerprint: i64,
    pub current_path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub rating: String,
    pub tp: String,
    pub status: String,
    pub display_name: String,
    pub virtual_folder: String,
    pub mezzanine_ok: bool,
    pub fps: f64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: String,
    pub keyframe_offsets_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetResponse {
    pub uuid: String,
    pub playoutvue_id: String,
    pub current_path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub rating: String,
    pub tp: String,
    pub status: String,
    pub display_name: String,
    pub virtual_folder: String,
    pub mezzanine_ok: bool,
    pub fps: f64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: Vec<String>,
    pub keyframe_offsets: Vec<i64>,
}

impl From<MediaAsset> for AssetResponse {
    fn from(a: MediaAsset) -> Self {
        let warnings: Vec<String> = serde_json::from_str(&a.warnings).unwrap_or_default();
        let keyframe_offsets: Vec<i64> =
            serde_json::from_str(&a.keyframe_offsets_json).unwrap_or_default();
        Self {
            uuid: a.uuid.clone(),
            playoutvue_id: a.uuid,
            current_path: a.current_path,
            duration_ms: a.duration_ms,
            trim_in_ms: a.trim_in_ms,
            trim_out_ms: a.trim_out_ms,
            rating: a.rating,
            tp: a.tp,
            status: a.status,
            display_name: a.display_name,
            virtual_folder: a.virtual_folder,
            mezzanine_ok: a.mezzanine_ok,
            fps: a.fps,
            fps_num: a.fps_num,
            fps_den: a.fps_den,
            total_frames: a.total_frames,
            gop_frames: a.gop_frames,
            keyframe_safe_start_ms: a.keyframe_safe_start_ms,
            warnings,
            keyframe_offsets,
        }
    }
}

const SELECT_COLS: &str = "uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json";

/// Find all assets with a given status. Used for startup recovery scans.
pub async fn find_all_with_status(
    pool: &SqlitePool,
    status: &str,
) -> Result<Vec<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE status = ?1 ORDER BY uuid",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(status)
        .fetch_all(pool)
        .await
}

/// Outcome of a startup-recovery sweep over in-flight / failed asset rows.
#[derive(Debug, Default, Serialize)]
pub struct RecoveryOutcome {
    pub purged_for_retry: usize,
    pub purged_dead: usize,
    pub kept_dead: usize,
}

/// Reclaim `error`/`processing` rows whose `current_path` (= source path on those states)
/// still lives inside the watch folder, so the watcher will re-queue them. The remaining
/// dead rows are kept (their source file is no longer reachable). Returns counts for logging.
pub async fn recover_failed_assets(
    pool: &SqlitePool,
    watch_folder: &Path,
    auto_retry: bool,
) -> Result<RecoveryOutcome, sqlx::Error> {
    let mut out = RecoveryOutcome::default();
    if !auto_retry {
        return Ok(out);
    }
    let canonical_watch = watch_folder
        .canonicalize()
        .unwrap_or_else(|_| watch_folder.to_path_buf());
    for status in ["error", "processing"] {
        let rows = find_all_with_status(pool, status).await?;
        for a in rows {
            let src_path = std::path::Path::new(&a.current_path);
            let still_in_watch = src_path
                .canonicalize()
                .ok()
                .map(|c| c.starts_with(&canonical_watch))
                .unwrap_or(false);
            let exists = src_path.exists();
            if still_in_watch {
                purge_row_by_uuid(pool, &a.uuid).await?;
                out.purged_for_retry += 1;
            } else if !exists {
                purge_row_by_uuid(pool, &a.uuid).await?;
                out.purged_dead += 1;
            } else {
                out.kept_dead += 1;
            }
        }
    }
    Ok(out)
}

pub async fn init_pool(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let db_dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let _ = std::fs::create_dir_all(db_dir);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await?;

    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS media_assets (
            uuid         TEXT PRIMARY KEY,
            fingerprint  INTEGER NOT NULL,
            current_path TEXT NOT NULL,
            duration_ms  INTEGER NOT NULL DEFAULT 0,
            trim_in_ms   INTEGER NOT NULL DEFAULT 0,
            trim_out_ms  INTEGER NOT NULL DEFAULT 0,
            rating       TEXT NOT NULL DEFAULT 'K',
            tp           TEXT NOT NULL DEFAULT 'None',
            status       TEXT NOT NULL DEFAULT 'processing',
            display_name TEXT NOT NULL DEFAULT '',
            virtual_folder TEXT NOT NULL DEFAULT '/',
            mezzanine_ok BOOLEAN NOT NULL DEFAULT 0,
            fps REAL NOT NULL DEFAULT 0.0,
            fps_num INTEGER NOT NULL DEFAULT 0,
            fps_den INTEGER NOT NULL DEFAULT 0,
            total_frames INTEGER NOT NULL DEFAULT 0,
            gop_frames INTEGER NOT NULL DEFAULT 0,
            keyframe_safe_start_ms INTEGER NOT NULL DEFAULT 0,
            warnings TEXT NOT NULL DEFAULT '[]',
            keyframe_offsets_json TEXT NOT NULL DEFAULT '[]'
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS virtual_folder_colors (
            virtual_folder TEXT PRIMARY KEY,
            color          TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transcode_jobs (
            id TEXT PRIMARY KEY NOT NULL,
            input_path TEXT NOT NULL,
            output_path TEXT,
            profile TEXT NOT NULL,
            uuid TEXT,
            state TEXT NOT NULL,
            phase TEXT NOT NULL,
            progress REAL NOT NULL DEFAULT 0.0,
            current_stage TEXT NOT NULL,
            duration_secs REAL NOT NULL DEFAULT 0.0,
            error TEXT,
            error_category TEXT,
            stderr_log_json TEXT,
            attempt INTEGER NOT NULL DEFAULT 1,
            max_attempts INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            started_at TEXT,
            finished_at TEXT,
            fingerprint INTEGER,
            request_hash TEXT,
            worker_id TEXT,
            leased_until TEXT,
            heartbeat_at TEXT,
            cancel_requested BOOLEAN NOT NULL DEFAULT 0,
            source_frame_count INTEGER NOT NULL DEFAULT 0,
            current_frame INTEGER NOT NULL DEFAULT 0,
            encode_fps REAL NOT NULL DEFAULT 0.0,
            encode_bitrate TEXT NOT NULL DEFAULT '',
            encode_speed TEXT NOT NULL DEFAULT '',
            current_time_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await?;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_transcode_jobs_state_phase ON transcode_jobs(state, phase)",
    )
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_transcode_jobs_request_hash ON transcode_jobs(request_hash)",
    )
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_transcode_jobs_fingerprint ON transcode_jobs(fingerprint)",
    )
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_transcode_jobs_leased_until ON transcode_jobs(leased_until)",
    )
    .execute(&pool)
    .await;

    for (col, default) in [
        ("display_name", "''"),
        ("rating", "'K'"),
        ("tp", "'None'"),
        ("virtual_folder", "'/'"),
        ("mezzanine_ok", "0"),
        ("fps", "0.0"),
        ("total_frames", "0"),
        ("gop_frames", "0"),
        ("keyframe_safe_start_ms", "0"),
        ("warnings", "'[]'"),
        ("keyframe_offsets_json", "'[]'"),
        ("fps_num", "0"),
        ("fps_den", "0"),
    ] {
        let sql = format!(
            "ALTER TABLE media_assets ADD COLUMN {} {} NOT NULL DEFAULT {}",
            col,
            if col == "fps" {
                "REAL"
            } else if col == "mezzanine_ok" {
                "BOOLEAN"
            } else {
                "INTEGER"
            },
            default
        );
        if let Err(e) = sqlx::query(&sql).execute(&pool).await {
            tracing::debug!("{} column may already exist: {}", col, e);
        }
    }

    {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT uuid, current_path FROM media_assets WHERE display_name = ''")
                .fetch_all(&pool)
                .await?;

        if !rows.is_empty() {
            tracing::info!(
                "Populating display_name for {} existing assets from current_path stems",
                rows.len()
            );
            for (uuid, path) in &rows {
                let stem = std::path::Path::new(path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| uuid.clone());
                let display_name: String = stem.chars().take(255).collect();
                let _ = sqlx::query("UPDATE media_assets SET display_name = ?1 WHERE uuid = ?2")
                    .bind(&display_name)
                    .bind(uuid)
                    .execute(&pool)
                    .await;
            }
        }
    }

    let result =
        sqlx::query("UPDATE media_assets SET status = 'error' WHERE status = 'processing'")
            .execute(&pool)
            .await?;
    if result.rows_affected() > 0 {
        tracing::warn!(
            "Recovered {} orphaned asset row(s) left in 'processing' state (marked 'error'); recovery sweep will purge eligible ones",
            result.rows_affected()
        );
    }

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_media_assets_fingerprint ON media_assets(fingerprint)",
    )
    .execute(&pool)
    .await?;

    tracing::info!(
        "Database initialized at {} (WAL mode, media_assets ready)",
        db_path.display()
    );

    Ok(pool)
}

pub async fn insert_processing(
    pool: &SqlitePool,
    uuid: &str,
    fingerprint: i64,
    path: &str,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_assets (uuid, fingerprint, current_path, display_name, status) VALUES (?1, ?2, ?3, ?4, 'processing')",
    )
    .bind(uuid)
    .bind(fingerprint)
    .bind(path)
    .bind(display_name)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_ready(
    pool: &SqlitePool,
    uuid: &str,
    output_path: &str,
    duration_ms: i64,
    mezzanine_ok: bool,
    fps: f64,
    fps_num: i64,
    fps_den: i64,
    total_frames: i64,
    gop_frames: i64,
    keyframe_safe_start_ms: i64,
    warnings: &[String],
    keyframe_offsets_json: &str,
) -> Result<(), sqlx::Error> {
    let warnings_json = serde_json::to_string(warnings).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "UPDATE media_assets SET
            current_path = ?1,
            duration_ms = ?2,
            trim_in_ms = 0,
            trim_out_ms = ?2,
            status = 'ready',
            mezzanine_ok = ?3,
            fps = ?4,
            fps_num = ?5,
            fps_den = ?6,
            total_frames = ?7,
            gop_frames = ?8,
            keyframe_safe_start_ms = ?9,
            warnings = ?10,
            keyframe_offsets_json = ?11
         WHERE uuid = ?12",
    )
    .bind(output_path)
    .bind(duration_ms)
    .bind(mezzanine_ok)
    .bind(fps)
    .bind(fps_num)
    .bind(fps_den)
    .bind(total_frames)
    .bind(gop_frames)
    .bind(keyframe_safe_start_ms)
    .bind(warnings_json)
    .bind(keyframe_offsets_json)
    .bind(uuid)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_error(pool: &SqlitePool, uuid: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE media_assets SET status = 'error' WHERE uuid = ?1")
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn find_by_uuid(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!("SELECT {} FROM media_assets WHERE uuid = ?1", SELECT_COLS);
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(uuid)
        .fetch_optional(pool)
        .await
}

pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE fingerprint = ?1",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(fingerprint)
        .fetch_optional(pool)
        .await
}

pub async fn count_rows_by_path(pool: &SqlitePool, current_path: &str) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM media_assets WHERE current_path = ?1")
            .bind(current_path)
            .fetch_one(pool)
            .await?;
    Ok(count)
}

pub async fn set_trim(
    pool: &SqlitePool,
    uuid: &str,
    trim_in_ms: i64,
    trim_out_ms: i64,
) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("UPDATE media_assets SET trim_in_ms = ?1, trim_out_ms = ?2 WHERE uuid = ?3")
            .bind(trim_in_ms)
            .bind(trim_out_ms)
            .bind(uuid)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_rating(pool: &SqlitePool, uuid: &str, rating: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET rating = ?1 WHERE uuid = ?2")
        .bind(rating)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_tp(pool: &SqlitePool, uuid: &str, tp: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET tp = ?1 WHERE uuid = ?2")
        .bind(tp)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn create_subclip(
    pool: &SqlitePool,
    new_uuid: &str,
    parent_uuid: &str,
    display_name: &str,
    trim_in_ms: i64,
    trim_out_ms: i64,
    mezzanine_ok: bool,
    warnings: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let parent = find_by_uuid(pool, parent_uuid).await?;
    if let Some(p) = parent {
        sqlx::query(
            "INSERT INTO media_assets (uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"
        )
        .bind(new_uuid)
        .bind(p.fingerprint)
        .bind(&p.current_path)
        .bind(p.duration_ms)
        .bind(trim_in_ms)
        .bind(trim_out_ms)
        .bind(&p.rating)
        .bind(&p.tp)
        .bind(&p.status)
        .bind(display_name)
        .bind(&p.virtual_folder)
        .bind(mezzanine_ok)
        .bind(p.fps)
        .bind(p.fps_num)
        .bind(p.fps_den)
        .bind(p.total_frames)
        .bind(p.gop_frames)
        .bind(p.keyframe_safe_start_ms)
        .bind(warnings)
        .bind(&p.keyframe_offsets_json)
        .execute(pool)
        .await?;

        find_by_uuid(pool, new_uuid).await
    } else {
        Ok(None)
    }
}

pub async fn purge_row_by_uuid(pool: &SqlitePool, uuid: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM media_assets WHERE uuid = ?1")
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn purge_rows_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM media_assets WHERE fingerprint = ?1")
        .bind(fingerprint)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PurgeMode {
    #[default]
    PreserveReferencedMezzanine,
    DeleteUnreferencedMezzanine,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub rows_deleted: u64,
    pub file_removed: bool,
    pub sidecar_removed: bool,
}

pub async fn purge_asset_with_mode(
    pool: &SqlitePool,
    uuid: &str,
    mode: PurgeMode,
) -> Result<PurgeOutcome, sqlx::Error> {
    let asset = find_by_uuid(pool, uuid).await?;
    let Some(a) = asset else {
        return Ok(PurgeOutcome {
            rows_deleted: 0,
            file_removed: false,
            sidecar_removed: false,
        });
    };

    let path = a.current_path.clone();
    purge_row_by_uuid(pool, uuid).await?;

    let remaining_refs = count_rows_by_path(pool, &path).await?;
    let should_remove_file = match mode {
        PurgeMode::PreserveReferencedMezzanine => remaining_refs == 0 && !path.is_empty(),
        PurgeMode::DeleteUnreferencedMezzanine => remaining_refs == 0 && !path.is_empty(),
    };

    let mut file_removed = false;
    let mut sidecar_removed = false;

    if should_remove_file {
        let media_path = Path::new(&path);
        if !crate::watcher::is_temp_file_name(media_path) {
            match tokio::fs::remove_file(media_path).await {
                Ok(_) => file_removed = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => file_removed = false,
                Err(e) => {
                    tracing::warn!("Failed to remove physical file at {}: {}", path, e);
                }
            }

            let sidecar_path = crate::identity::sidecar_path_for(media_path);
            match tokio::fs::remove_file(&sidecar_path).await {
                Ok(_) => sidecar_removed = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => sidecar_removed = false,
                Err(e) => {
                    tracing::warn!(
                        "Failed to remove sidecar file at {}: {}",
                        sidecar_path.display(),
                        e
                    );
                }
            }
        }
    }

    Ok(PurgeOutcome {
        rows_deleted: 1,
        file_removed,
        sidecar_removed,
    })
}

#[allow(dead_code)]
pub async fn purge_asset_completely(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<PurgeOutcome, sqlx::Error> {
    purge_asset_with_mode(pool, uuid, PurgeMode::PreserveReferencedMezzanine).await
}

pub const VALID_RATINGS: &[&str] = &["K", "8", "12", "16", "18"];

pub fn is_valid_rating(rating: &str) -> bool {
    let trimmed = rating.trim_end_matches('+').to_ascii_uppercase();
    VALID_RATINGS.contains(&trimmed.as_str()) || trimmed == "NONE" || trimmed.is_empty()
}

pub const MAX_DISPLAY_NAME_LEN: usize = 255;

pub async fn set_display_name(
    pool: &SqlitePool,
    uuid: &str,
    display_name: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET display_name = ?1 WHERE uuid = ?2")
        .bind(display_name)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_virtual_folder(
    pool: &SqlitePool,
    uuid: &str,
    virtual_folder: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET virtual_folder = ?1 WHERE uuid = ?2")
        .bind(virtual_folder)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub fn is_valid_virtual_folder(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if !path.starts_with('/') {
        return false;
    }
    if path.contains("..") {
        return false;
    }
    if path != "/" && path.ends_with('/') {
        return false;
    }
    true
}

pub async fn find_all(
    pool: &SqlitePool,
    status_filter: Option<&str>,
) -> Result<Vec<MediaAsset>, sqlx::Error> {
    let sql = format!("SELECT {} FROM media_assets ORDER BY uuid", SELECT_COLS);
    if let Some(status) = status_filter {
        let filtered = format!(
            "SELECT {} FROM media_assets WHERE status = ?1 ORDER BY uuid",
            SELECT_COLS
        );
        sqlx::query_as::<_, MediaAsset>(&filtered)
            .bind(status)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, MediaAsset>(&sql).fetch_all(pool).await
    }
}

pub async fn find_batch(
    pool: &SqlitePool,
    uuids: &[String],
) -> Result<Vec<MediaAsset>, sqlx::Error> {
    if uuids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = uuids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {} FROM media_assets WHERE uuid IN ({})",
        SELECT_COLS, placeholders
    );
    let mut query = sqlx::query_as::<_, MediaAsset>(&sql);
    for uuid in uuids {
        query = query.bind(uuid);
    }
    query.fetch_all(pool).await
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FolderColor {
    pub virtual_folder: String,
    pub color: String,
}

pub async fn get_all_folder_colors(pool: &SqlitePool) -> Result<Vec<FolderColor>, sqlx::Error> {
    sqlx::query_as::<_, FolderColor>("SELECT virtual_folder, color FROM virtual_folder_colors")
        .fetch_all(pool)
        .await
}

pub async fn set_folder_color(
    pool: &SqlitePool,
    virtual_folder: &str,
    color: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO virtual_folder_colors (virtual_folder, color) VALUES (?1, ?2)
         ON CONFLICT(virtual_folder) DO UPDATE SET color = excluded.color",
    )
    .bind(virtual_folder)
    .bind(color)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DurableJobRow {
    pub id: String,
    pub input_path: String,
    pub output_path: Option<String>,
    pub profile: String,
    pub uuid: Option<String>,
    pub state: String,
    pub phase: String,
    pub progress: f64,
    pub current_stage: String,
    pub duration_secs: f64,
    pub error: Option<String>,
    pub error_category: Option<String>,
    pub stderr_log_json: Option<String>,
    pub attempt: i64,
    pub max_attempts: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub fingerprint: Option<i64>,
    pub request_hash: Option<String>,
    pub worker_id: Option<String>,
    pub leased_until: Option<String>,
    pub heartbeat_at: Option<String>,
    pub cancel_requested: bool,
    pub source_frame_count: i64,
    pub current_frame: i64,
    pub encode_fps: f64,
    pub encode_bitrate: String,
    pub encode_speed: String,
    pub current_time_ms: i64,
    pub duration_ms: i64,
}

impl DurableJobRow {
    pub fn into_job_record(self) -> crate::jobs::JobRecord {
        let state = self.state.parse().unwrap_or(crate::jobs::JobState::Pending);
        let phase = self.phase.parse().unwrap_or(crate::jobs::JobPhase::Queued);
        let stderr_log = self
            .stderr_log_json
            .and_then(|j| serde_json::from_str(&j).ok());

        crate::jobs::JobRecord {
            id: self.id,
            input_path: self.input_path,
            output_path: self.output_path,
            profile: self.profile,
            uuid: self.uuid,
            state,
            phase,
            progress: self.progress as f32,
            current_stage: self.current_stage,
            duration_secs: self.duration_secs,
            error: self.error,
            error_category: self.error_category,
            stderr_log,
            attempt: self.attempt as u32,
            max_attempts: self.max_attempts as u32,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
            fingerprint: self.fingerprint,
            request_hash: self.request_hash,
            worker_id: self.worker_id,
            leased_until: self.leased_until,
            heartbeat_at: self.heartbeat_at,
            cancel_requested: self.cancel_requested,
            source_frame_count: self.source_frame_count,
            current_frame: self.current_frame,
            encode_fps: self.encode_fps,
            encode_bitrate: self.encode_bitrate,
            encode_speed: self.encode_speed,
            current_time_ms: self.current_time_ms,
            duration_ms: self.duration_ms,
        }
    }
}

pub async fn insert_durable_job(
    pool: &SqlitePool,
    job: &crate::jobs::JobRecord,
) -> Result<(), sqlx::Error> {
    let stderr_json = job
        .stderr_log
        .as_ref()
        .map(|s| serde_json::to_string(s).unwrap_or_default());
    sqlx::query(
        "INSERT INTO transcode_jobs (
            id, input_path, output_path, profile, uuid, state, phase, progress, current_stage,
            duration_secs, error, error_category, stderr_log_json, attempt, max_attempts,
            created_at, started_at, finished_at, fingerprint, request_hash, worker_id,
            leased_until, heartbeat_at, cancel_requested, source_frame_count, current_frame,
            encode_fps, encode_bitrate, encode_speed, current_time_ms, duration_ms
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
            ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21,
            ?22, ?23, ?24, ?25, ?26,
            ?27, ?28, ?29, ?30, ?31
        ) ON CONFLICT(id) DO UPDATE SET
            input_path = excluded.input_path,
            output_path = excluded.output_path,
            profile = excluded.profile,
            uuid = excluded.uuid,
            state = excluded.state,
            phase = excluded.phase,
            progress = excluded.progress,
            current_stage = excluded.current_stage,
            duration_secs = excluded.duration_secs,
            error = excluded.error,
            error_category = excluded.error_category,
            stderr_log_json = excluded.stderr_log_json,
            attempt = excluded.attempt,
            max_attempts = excluded.max_attempts,
            started_at = excluded.started_at,
            finished_at = excluded.finished_at,
            fingerprint = excluded.fingerprint,
            request_hash = excluded.request_hash,
            worker_id = excluded.worker_id,
            leased_until = excluded.leased_until,
            heartbeat_at = excluded.heartbeat_at,
            cancel_requested = excluded.cancel_requested,
            source_frame_count = excluded.source_frame_count,
            current_frame = excluded.current_frame,
            encode_fps = excluded.encode_fps,
            encode_bitrate = excluded.encode_bitrate,
            encode_speed = excluded.encode_speed,
            current_time_ms = excluded.current_time_ms,
            duration_ms = excluded.duration_ms",
    )
    .bind(&job.id)
    .bind(&job.input_path)
    .bind(&job.output_path)
    .bind(&job.profile)
    .bind(&job.uuid)
    .bind(job.state.as_str())
    .bind(job.phase.as_str())
    .bind(job.progress as f64)
    .bind(&job.current_stage)
    .bind(job.duration_secs)
    .bind(&job.error)
    .bind(&job.error_category)
    .bind(&stderr_json)
    .bind(job.attempt as i64)
    .bind(job.max_attempts as i64)
    .bind(&job.created_at)
    .bind(&job.started_at)
    .bind(&job.finished_at)
    .bind(job.fingerprint)
    .bind(&job.request_hash)
    .bind(&job.worker_id)
    .bind(&job.leased_until)
    .bind(&job.heartbeat_at)
    .bind(job.cancel_requested)
    .bind(job.source_frame_count)
    .bind(job.current_frame)
    .bind(job.encode_fps)
    .bind(&job.encode_bitrate)
    .bind(&job.encode_speed)
    .bind(job.current_time_ms)
    .bind(job.duration_ms)
    .execute(pool)
    .await?;

    Ok(())
}

#[allow(dead_code)]
pub async fn claim_next_job(
    pool: &SqlitePool,
    worker_id: &str,
    lease_secs: i64,
) -> Result<Option<crate::jobs::JobRecord>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let row: Option<DurableJobRow> = sqlx::query_as(
        "SELECT * FROM transcode_jobs 
         WHERE (state = 'Pending' AND phase = 'queued') 
            OR (state = 'Processing' AND leased_until IS NOT NULL AND leased_until < datetime('now'))
         ORDER BY created_at ASC 
         LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(r) = row {
        let now = chrono::Utc::now();
        let leased_until = (now + chrono::Duration::seconds(lease_secs)).to_rfc3339();
        let heartbeat_at = now.to_rfc3339();
        let started_at = r.started_at.clone().unwrap_or_else(|| now.to_rfc3339());

        sqlx::query(
            "UPDATE transcode_jobs SET
                state = 'Processing',
                phase = 'probing',
                current_stage = 'Claimed by worker',
                worker_id = ?1,
                leased_until = ?2,
                heartbeat_at = ?3,
                started_at = ?4
             WHERE id = ?5",
        )
        .bind(worker_id)
        .bind(&leased_until)
        .bind(&heartbeat_at)
        .bind(&started_at)
        .bind(&r.id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let mut job = r.into_job_record();
        job.state = crate::jobs::JobState::Processing;
        job.phase = crate::jobs::JobPhase::Probing;
        job.current_stage = "Claimed by worker".to_string();
        job.worker_id = Some(worker_id.to_string());
        job.leased_until = Some(leased_until);
        job.heartbeat_at = Some(heartbeat_at);
        job.started_at = Some(started_at);

        Ok(Some(job))
    } else {
        tx.rollback().await?;
        Ok(None)
    }
}

#[allow(dead_code)]
pub async fn heartbeat_job(
    pool: &SqlitePool,
    job_id: &str,
    worker_id: &str,
    extend_secs: i64,
) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now();
    let leased_until = (now + chrono::Duration::seconds(extend_secs)).to_rfc3339();
    let heartbeat_at = now.to_rfc3339();

    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT cancel_requested FROM transcode_jobs WHERE id = ?1 AND worker_id = ?2",
    )
    .bind(job_id)
    .bind(worker_id)
    .fetch_optional(pool)
    .await?;

    if let Some((cancel_req,)) = row {
        if !cancel_req {
            let _ = sqlx::query(
                "UPDATE transcode_jobs SET leased_until = ?1, heartbeat_at = ?2 WHERE id = ?3 AND worker_id = ?4",
            )
            .bind(&leased_until)
            .bind(&heartbeat_at)
            .bind(job_id)
            .bind(worker_id)
            .execute(pool)
            .await;
        }
        Ok(cancel_req)
    } else {
        Ok(false)
    }
}

#[allow(dead_code)]
pub async fn request_job_cancellation(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE transcode_jobs SET cancel_requested = 1, phase = CASE WHEN phase = 'queued' THEN 'cancelled' ELSE 'cancel_requested' END, state = CASE WHEN phase = 'queued' THEN 'Cancelled' ELSE state END WHERE id = ?1 AND state IN ('Pending', 'Processing')",
    )
    .bind(job_id)
    .execute(pool)
    .await?;

    Ok(res.rows_affected() > 0)
}

#[derive(Debug, Clone, Default)]
pub struct JobRecoveryReport {
    pub requeued: usize,
    pub failed_exhausted: usize,
}

pub async fn recover_stale_jobs(pool: &SqlitePool) -> Result<JobRecoveryReport, sqlx::Error> {
    let mut report = JobRecoveryReport::default();
    let now = chrono::Utc::now().to_rfc3339();

    // Re-queue in-flight jobs that have attempts remaining
    let requeue_res = sqlx::query(
        "UPDATE transcode_jobs SET
            state = 'Pending',
            phase = 'queued',
            current_stage = 'Re-queued (stale crash recovery)',
            worker_id = NULL,
            leased_until = NULL,
            heartbeat_at = NULL,
            attempt = attempt + 1
         WHERE state = 'Processing' AND attempt < max_attempts",
    )
    .execute(pool)
    .await?;
    report.requeued = requeue_res.rows_affected() as usize;

    // Fail in-flight jobs that have exhausted attempts
    let fail_res = sqlx::query(
        "UPDATE transcode_jobs SET
            state = 'Failed',
            phase = 'failed',
            current_stage = 'Failed',
            error = 'Worker crashed or lease expired (attempts exhausted)',
            error_category = 'lease_expired',
            worker_id = NULL,
            leased_until = NULL,
            finished_at = ?1
         WHERE state = 'Processing' AND attempt >= max_attempts",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    report.failed_exhausted = fail_res.rows_affected() as usize;

    if report.requeued > 0 || report.failed_exhausted > 0 {
        tracing::warn!(
            "Durable queue startup recovery: {} job(s) re-queued, {} job(s) marked failed (exhausted)",
            report.requeued,
            report.failed_exhausted
        );
    }

    Ok(report)
}

pub async fn load_all_durable_jobs(
    pool: &SqlitePool,
) -> Result<Vec<crate::jobs::JobRecord>, sqlx::Error> {
    let rows: Vec<DurableJobRow> =
        sqlx::query_as("SELECT * FROM transcode_jobs ORDER BY created_at ASC")
            .fetch_all(pool)
            .await?;

    Ok(rows.into_iter().map(|r| r.into_job_record()).collect())
}

#[allow(dead_code)]
pub async fn find_active_by_request_hash(
    pool: &SqlitePool,
    req_hash: &str,
) -> Result<Option<crate::jobs::JobRecord>, sqlx::Error> {
    let row: Option<DurableJobRow> = sqlx::query_as(
        "SELECT * FROM transcode_jobs WHERE request_hash = ?1 AND state IN ('Pending', 'Processing') LIMIT 1",
    )
    .bind(req_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| r.into_job_record()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_rating() {
        assert!(is_valid_rating("K"));
        assert!(is_valid_rating("12"));
        assert!(is_valid_rating("18+"));
        assert!(is_valid_rating(""));
        assert!(!is_valid_rating("21"));
    }

    #[test]
    fn test_is_valid_virtual_folder() {
        assert!(is_valid_virtual_folder("/"));
        assert!(is_valid_virtual_folder("/news"));
        assert!(is_valid_virtual_folder("/a/b"));
        assert!(!is_valid_virtual_folder(""));
        assert!(!is_valid_virtual_folder("news"));
        assert!(!is_valid_virtual_folder("/../etc"));
        assert!(!is_valid_virtual_folder("/news/"));
    }

    async fn setup_test_pool() -> (SqlitePool, std::path::PathBuf) {
        let temp_dir = std::env::temp_dir().join(format!("pt_test_purge_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let db_path = temp_dir.join("test.db");
        let pool = init_pool(&db_path).await.expect("init_pool failed");
        (pool, temp_dir)
    }

    #[tokio::test]
    async fn test_purge_parent_with_no_subclips() {
        let (pool, temp_dir) = setup_test_pool().await;
        let video_path = temp_dir.join("video1.mp4");
        let sidecar_path = crate::identity::sidecar_path_for(&video_path);

        std::fs::File::create(&video_path).unwrap();
        std::fs::File::create(&sidecar_path).unwrap();

        let uuid = "parent-1";
        insert_processing(&pool, uuid, 12345, &video_path.to_string_lossy(), "video1")
            .await
            .unwrap();
        mark_ready(
            &pool,
            uuid,
            &video_path.to_string_lossy(),
            10000,
            true,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &[],
            "[]",
        )
        .await
        .unwrap();

        assert!(video_path.exists());
        assert!(sidecar_path.exists());

        let outcome = purge_asset_with_mode(&pool, uuid, PurgeMode::PreserveReferencedMezzanine)
            .await
            .unwrap();
        assert_eq!(outcome.rows_deleted, 1);
        assert!(outcome.file_removed);
        assert!(outcome.sidecar_removed);

        assert!(!video_path.exists());
        assert!(!sidecar_path.exists());
        assert!(find_by_uuid(&pool, uuid).await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_purge_parent_while_virtual_subclip_still_references_it() {
        let (pool, temp_dir) = setup_test_pool().await;
        let video_path = temp_dir.join("shared_mezzanine.mp4");
        let sidecar_path = crate::identity::sidecar_path_for(&video_path);

        std::fs::File::create(&video_path).unwrap();
        std::fs::File::create(&sidecar_path).unwrap();

        let parent_uuid = "parent-shared";
        let subclip_uuid = "subclip-shared";

        insert_processing(
            &pool,
            parent_uuid,
            67890,
            &video_path.to_string_lossy(),
            "shared",
        )
        .await
        .unwrap();
        mark_ready(
            &pool,
            parent_uuid,
            &video_path.to_string_lossy(),
            20000,
            true,
            25.0,
            25,
            1,
            500,
            50,
            0,
            &[],
            "[]",
        )
        .await
        .unwrap();

        create_subclip(
            &pool,
            subclip_uuid,
            parent_uuid,
            "subclip_1",
            5000,
            15000,
            true,
            "[]",
        )
        .await
        .unwrap();

        assert_eq!(
            count_rows_by_path(&pool, &video_path.to_string_lossy())
                .await
                .unwrap(),
            2
        );

        // Purge parent only
        let outcome =
            purge_asset_with_mode(&pool, parent_uuid, PurgeMode::PreserveReferencedMezzanine)
                .await
                .unwrap();
        assert_eq!(outcome.rows_deleted, 1);
        assert!(
            !outcome.file_removed,
            "Mezzanine file MUST be preserved while subclip references it"
        );
        assert!(
            !outcome.sidecar_removed,
            "Sidecar MUST be preserved while subclip references it"
        );

        assert!(video_path.exists());
        assert!(sidecar_path.exists());
        assert!(find_by_uuid(&pool, parent_uuid).await.unwrap().is_none());

        let subclip = find_by_uuid(&pool, subclip_uuid)
            .await
            .unwrap()
            .expect("Subclip must still exist in DB");
        assert_eq!(subclip.current_path, video_path.to_string_lossy());

        // Now purge subclip (final reference)
        let outcome2 =
            purge_asset_with_mode(&pool, subclip_uuid, PurgeMode::PreserveReferencedMezzanine)
                .await
                .unwrap();
        assert_eq!(outcome2.rows_deleted, 1);
        assert!(
            outcome2.file_removed,
            "Mezzanine file MUST be removed once final reference is purged"
        );
        assert!(
            outcome2.sidecar_removed,
            "Sidecar MUST be removed once final reference is purged"
        );

        assert!(!video_path.exists());
        assert!(!sidecar_path.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_purge_one_of_multiple_subclips() {
        let (pool, temp_dir) = setup_test_pool().await;
        let video_path = temp_dir.join("multi_subclips.mp4");
        std::fs::File::create(&video_path).unwrap();

        let parent_uuid = "parent-multi";
        let sub1 = "subclip-1";
        let sub2 = "subclip-2";

        insert_processing(
            &pool,
            parent_uuid,
            11111,
            &video_path.to_string_lossy(),
            "multi",
        )
        .await
        .unwrap();
        mark_ready(
            &pool,
            parent_uuid,
            &video_path.to_string_lossy(),
            30000,
            true,
            25.0,
            25,
            1,
            750,
            50,
            0,
            &[],
            "[]",
        )
        .await
        .unwrap();

        create_subclip(&pool, sub1, parent_uuid, "sub1", 1000, 5000, true, "[]")
            .await
            .unwrap();
        create_subclip(&pool, sub2, parent_uuid, "sub2", 6000, 10000, true, "[]")
            .await
            .unwrap();

        assert_eq!(
            count_rows_by_path(&pool, &video_path.to_string_lossy())
                .await
                .unwrap(),
            3
        );

        let out = purge_asset_with_mode(&pool, sub1, PurgeMode::PreserveReferencedMezzanine)
            .await
            .unwrap();
        assert_eq!(out.rows_deleted, 1);
        assert!(!out.file_removed);
        assert!(video_path.exists());
        assert_eq!(
            count_rows_by_path(&pool, &video_path.to_string_lossy())
                .await
                .unwrap(),
            2
        );

        let _ = std::fs::remove_file(&video_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_purge_never_deletes_staging_files() {
        let (pool, temp_dir) = setup_test_pool().await;
        let staging_path = temp_dir.join(".tmp_uuid1_video.mp4");
        std::fs::File::create(&staging_path).unwrap();

        let uuid = "failed-staging-asset";
        insert_processing(&pool, uuid, 99999, &staging_path.to_string_lossy(), "video")
            .await
            .unwrap();

        let out = purge_asset_with_mode(&pool, uuid, PurgeMode::PreserveReferencedMezzanine)
            .await
            .unwrap();
        assert_eq!(out.rows_deleted, 1);
        assert!(
            !out.file_removed,
            "Staging file must not be deleted through asset purge"
        );

        let _ = std::fs::remove_file(&staging_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_durable_job_insert_and_load() {
        let (pool, temp_dir) = setup_test_pool().await;
        let mut job = crate::jobs::JobRecord::new("D:/media/clip.mp4", "ProfileA");
        job.request_hash = Some("reqhash12345".into());
        job.fingerprint = Some(424242);
        job.transition_to(crate::jobs::JobPhase::Probing, None)
            .unwrap();

        insert_durable_job(&pool, &job).await.unwrap();

        let loaded = load_all_durable_jobs(&pool).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job.id);
        assert_eq!(loaded[0].phase, crate::jobs::JobPhase::Probing);
        assert_eq!(loaded[0].state, crate::jobs::JobState::Processing);
        assert_eq!(loaded[0].request_hash.as_deref(), Some("reqhash12345"));
        assert_eq!(loaded[0].fingerprint, Some(424242));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_atomic_claim_and_lease() {
        let (pool, temp_dir) = setup_test_pool().await;
        let job = crate::jobs::JobRecord::new("D:/media/clip.mp4", "ProfileA");
        insert_durable_job(&pool, &job).await.unwrap();

        // Worker 1 claims job
        let claimed = claim_next_job(&pool, "worker-1", 60).await.unwrap();
        assert!(claimed.is_some());
        let claimed_job = claimed.unwrap();
        assert_eq!(claimed_job.id, job.id);
        assert_eq!(claimed_job.worker_id.as_deref(), Some("worker-1"));
        assert_eq!(claimed_job.phase, crate::jobs::JobPhase::Probing);
        assert_eq!(claimed_job.state, crate::jobs::JobState::Processing);

        // Worker 2 attempts to claim while lease is active -> None
        let second_claim = claim_next_job(&pool, "worker-2", 60).await.unwrap();
        assert!(second_claim.is_none());

        // Worker 1 heartbeats
        let cancel_req = heartbeat_job(&pool, &job.id, "worker-1", 60).await.unwrap();
        assert!(!cancel_req);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_stale_job_crash_recovery() {
        let (pool, temp_dir) = setup_test_pool().await;

        // Job 1: In-flight with attempt 1 of 2 -> should re-queue
        let mut job1 = crate::jobs::JobRecord::new("D:/media/in1.mp4", "ProfileA");
        job1.state = crate::jobs::JobState::Processing;
        job1.phase = crate::jobs::JobPhase::Encoding;
        job1.attempt = 1;
        job1.max_attempts = 2;
        insert_durable_job(&pool, &job1).await.unwrap();

        // Job 2: In-flight with attempt 2 of 2 -> should fail
        let mut job2 = crate::jobs::JobRecord::new("D:/media/in2.mp4", "ProfileA");
        job2.state = crate::jobs::JobState::Processing;
        job2.phase = crate::jobs::JobPhase::Encoding;
        job2.attempt = 2;
        job2.max_attempts = 2;
        insert_durable_job(&pool, &job2).await.unwrap();

        let report = recover_stale_jobs(&pool).await.unwrap();
        assert_eq!(report.requeued, 1);
        assert_eq!(report.failed_exhausted, 1);

        let all = load_all_durable_jobs(&pool).await.unwrap();
        let j1 = all.iter().find(|j| j.id == job1.id).unwrap();
        assert_eq!(j1.state, crate::jobs::JobState::Pending);
        assert_eq!(j1.phase, crate::jobs::JobPhase::Queued);
        assert_eq!(j1.attempt, 2);

        let j2 = all.iter().find(|j| j.id == job2.id).unwrap();
        assert_eq!(j2.state, crate::jobs::JobState::Failed);
        assert_eq!(j2.phase, crate::jobs::JobPhase::Failed);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_job_cancellation_and_request_hash_dedup() {
        let (pool, temp_dir) = setup_test_pool().await;
        let mut job = crate::jobs::JobRecord::new("D:/media/clip.mp4", "ProfileA");
        job.request_hash = Some("hash-abc-123".into());
        insert_durable_job(&pool, &job).await.unwrap();

        let found = find_active_by_request_hash(&pool, "hash-abc-123")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, job.id);

        let cancel_res = request_job_cancellation(&pool, &job.id).await.unwrap();
        assert!(cancel_res);

        let all = load_all_durable_jobs(&pool).await.unwrap();
        let j = all.iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(j.phase, crate::jobs::JobPhase::Cancelled);
        assert_eq!(j.state, crate::jobs::JobState::Cancelled);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
