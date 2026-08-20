use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPurgeResult {
    pub operation: String,
    pub rows_deleted: u64,
    pub media_removed: bool,
    pub sidecar_removed: bool,
    pub skipped_referenced_files: Vec<String>,
    pub cleanup_failures: Vec<String>,
    pub warnings: Vec<String>,
}

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
    pub deleted_at: Option<String>,
    pub original_virtual_folder: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_virtual_folder: Option<String>,
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
            deleted_at: a.deleted_at,
            original_virtual_folder: a.original_virtual_folder,
        }
    }
}

const SELECT_COLS: &str = "uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json, deleted_at, original_virtual_folder";

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
            keyframe_offsets_json TEXT NOT NULL DEFAULT '[]',
            deleted_at TEXT DEFAULT NULL,
            original_virtual_folder TEXT DEFAULT NULL
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

    for (col, col_type, default) in [
        ("display_name", "TEXT", "''"),
        ("rating", "TEXT", "'K'"),
        ("tp", "TEXT", "'None'"),
        ("virtual_folder", "TEXT", "'/'"),
        ("mezzanine_ok", "BOOLEAN", "0"),
        ("fps", "REAL", "0.0"),
        ("total_frames", "INTEGER", "0"),
        ("gop_frames", "INTEGER", "0"),
        ("keyframe_safe_start_ms", "INTEGER", "0"),
        ("warnings", "TEXT", "'[]'"),
        ("keyframe_offsets_json", "TEXT", "'[]'"),
        ("fps_num", "INTEGER", "0"),
        ("fps_den", "INTEGER", "0"),
        ("deleted_at", "TEXT", "NULL"),
        ("original_virtual_folder", "TEXT", "NULL"),
    ] {
        let sql = if default == "NULL" {
            format!(
                "ALTER TABLE media_assets ADD COLUMN {} {} DEFAULT NULL",
                col, col_type
            )
        } else {
            format!(
                "ALTER TABLE media_assets ADD COLUMN {} {} NOT NULL DEFAULT {}",
                col, col_type, default
            )
        };
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

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_media_assets_deleted_at ON media_assets(deleted_at)",
    )
    .execute(&pool)
    .await;

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

/// Find active asset by uuid (excludes soft-deleted / trashed assets).
pub async fn find_by_uuid(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE uuid = ?1 AND deleted_at IS NULL",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(uuid)
        .fetch_optional(pool)
        .await
}

/// Find asset by uuid unconditionally (including soft-deleted / trashed assets).
pub async fn find_by_uuid_raw(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!("SELECT {} FROM media_assets WHERE uuid = ?1", SELECT_COLS);
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(uuid)
        .fetch_optional(pool)
        .await
}

/// Find trashed asset by uuid.
#[allow(dead_code)]
pub async fn find_trashed_by_uuid(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE uuid = ?1 AND deleted_at IS NOT NULL",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(uuid)
        .fetch_optional(pool)
        .await
}

/// Find active asset by fingerprint.
pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE fingerprint = ?1 AND deleted_at IS NULL",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(fingerprint)
        .fetch_optional(pool)
        .await
}

/// Count total rows matching path (active + trashed) to protect physical media from premature deletion.
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
        sqlx::query("UPDATE media_assets SET trim_in_ms = ?1, trim_out_ms = ?2 WHERE uuid = ?3 AND deleted_at IS NULL")
            .bind(trim_in_ms)
            .bind(trim_out_ms)
            .bind(uuid)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_rating(pool: &SqlitePool, uuid: &str, rating: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET rating = ?1 WHERE uuid = ?2 AND deleted_at IS NULL")
        .bind(rating)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_tp(pool: &SqlitePool, uuid: &str, tp: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE media_assets SET tp = ?1 WHERE uuid = ?2 AND deleted_at IS NULL")
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

/// Soft delete a single active asset (moves to Recycle Bin).
pub async fn trash_asset(pool: &SqlitePool, uuid: &str) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE media_assets 
         SET deleted_at = ?1, 
             original_virtual_folder = COALESCE(original_virtual_folder, virtual_folder)
         WHERE uuid = ?2 AND deleted_at IS NULL"
    )
    .bind(now)
    .bind(uuid)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Soft delete all active assets under a virtual folder path (and descendants).
pub async fn trash_folder(pool: &SqlitePool, folder_path: &str) -> Result<u64, sqlx::Error> {
    let norm = if folder_path == "/" { "/" } else { folder_path.trim_end_matches('/') };
    let now = chrono::Utc::now().to_rfc3339();
    let result = if norm == "/" {
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = ?1, 
                 original_virtual_folder = COALESCE(original_virtual_folder, virtual_folder)
             WHERE deleted_at IS NULL"
        )
        .bind(now)
        .execute(pool)
        .await?
    } else {
        let prefix = format!("{}/%", norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = ?1, 
                 original_virtual_folder = COALESCE(original_virtual_folder, virtual_folder)
             WHERE (virtual_folder = ?2 OR virtual_folder LIKE ?3) AND deleted_at IS NULL"
        )
        .bind(now)
        .bind(norm)
        .bind(prefix)
        .execute(pool)
        .await?
    };
    Ok(result.rows_affected())
}

/// Restore a trashed asset from Recycle Bin.
pub async fn restore_asset(
    pool: &SqlitePool,
    uuid: &str,
    target_folder: Option<&str>,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let asset = find_by_uuid_raw(pool, uuid).await?;
    let Some(a) = asset else {
        return Ok(None);
    };
    if a.deleted_at.is_none() {
        return Ok(Some(a));
    }

    let effective_folder = if let Some(target) = target_folder {
        if is_valid_virtual_folder(target) {
            target.to_string()
        } else {
            "/".to_string()
        }
    } else {
        a.original_virtual_folder
            .clone()
            .unwrap_or_else(|| "/".to_string())
    };

    sqlx::query(
        "UPDATE media_assets 
         SET deleted_at = NULL, 
             virtual_folder = ?1,
             original_virtual_folder = NULL
         WHERE uuid = ?2"
    )
    .bind(&effective_folder)
    .bind(uuid)
    .execute(pool)
    .await?;

    find_by_uuid(pool, uuid).await
}

/// Restore all trashed assets that originated from a folder path.
pub async fn restore_folder(
    pool: &SqlitePool,
    folder_path: &str,
    fallback_to_root: bool,
) -> Result<u64, sqlx::Error> {
    let norm = if folder_path == "/" { "/" } else { folder_path.trim_end_matches('/') };
    let result = if norm == "/" {
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = NULL, 
                 virtual_folder = COALESCE(original_virtual_folder, '/'),
                 original_virtual_folder = NULL
             WHERE deleted_at IS NOT NULL"
        )
        .execute(pool)
        .await?
    } else if fallback_to_root {
        let prefix = format!("{}/%", norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = NULL, 
                 virtual_folder = '/',
                 original_virtual_folder = NULL
             WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2) AND deleted_at IS NOT NULL"
        )
        .bind(norm)
        .bind(prefix)
        .execute(pool)
        .await?
    } else {
        let prefix = format!("{}/%", norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = NULL, 
                 virtual_folder = COALESCE(original_virtual_folder, '/'),
                 original_virtual_folder = NULL
             WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2) AND deleted_at IS NOT NULL"
        )
        .bind(norm)
        .bind(prefix)
        .execute(pool)
        .await?
    };
    Ok(result.rows_affected())
}

/// List all assets in Recycle Bin (deleted_at IS NOT NULL).
pub async fn list_recycle_bin(pool: &SqlitePool) -> Result<Vec<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC, uuid",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql).fetch_all(pool).await
}

/// Validate path for safe physical deletion.
pub fn validate_purge_path(
    path_str: &str,
    managed_target_dir: Option<&Path>,
    watch_dir: Option<&Path>,
) -> Result<std::path::PathBuf, String> {
    let trimmed = path_str.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".to_string());
    }
    let path = std::path::Path::new(trimmed);
    if path.components().any(|c| c == std::path::Component::ParentDir) {
        return Err("Path contains parent directory traversal (..)".to_string());
    }
    if trimmed == "/" || trimmed == "\\" || (trimmed.len() <= 3 && trimmed.ends_with(":\\")) {
        return Err("Cannot purge root directory".to_string());
    }

    if let Some(watch) = watch_dir {
        if let (Ok(can_path), Ok(can_watch)) = (path.canonicalize(), watch.canonicalize()) {
            if can_path.starts_with(&can_watch) {
                return Err("Refusing to purge source media in watch folder".to_string());
            }
        }
    }

    if let Some(target) = managed_target_dir {
        if let (Ok(can_path), Ok(can_target)) = (path.canonicalize(), target.canonicalize()) {
            if !can_path.starts_with(&can_target) {
                return Err("Path is outside managed target directory root".to_string());
            }
        }
    }

    Ok(path.to_path_buf())
}

/// Purge a single asset with full path validation, reference counting, and physical cleanup.
pub async fn purge_single_asset_with_context(
    pool: &SqlitePool,
    uuid: &str,
    mode: PurgeMode,
    managed_target_dir: Option<&Path>,
    watch_dir: Option<&Path>,
) -> Result<StructuredPurgeResult, sqlx::Error> {
    let asset = find_by_uuid_raw(pool, uuid).await?;
    let Some(a) = asset else {
        return Ok(StructuredPurgeResult {
            operation: "purge_asset".to_string(),
            rows_deleted: 0,
            media_removed: false,
            sidecar_removed: false,
            skipped_referenced_files: Vec::new(),
            cleanup_failures: Vec::new(),
            warnings: vec!["asset_not_found".to_string()],
        });
    };

    let path = a.current_path.clone();
    purge_row_by_uuid(pool, uuid).await?;

    let remaining_refs = count_rows_by_path(pool, &path).await?;
    let mut media_removed = false;
    let mut sidecar_removed = false;
    let mut skipped_referenced_files = Vec::new();
    let mut cleanup_failures = Vec::new();
    let mut warnings = Vec::new();

    let should_remove_file = match mode {
        PurgeMode::PreserveReferencedMezzanine => remaining_refs == 0 && !path.is_empty(),
        PurgeMode::DeleteUnreferencedMezzanine => remaining_refs == 0 && !path.is_empty(),
    };

    if remaining_refs > 0 {
        skipped_referenced_files.push(path.clone());
        warnings.push(format!(
            "Physical media retained because {} other reference(s) still point to it",
            remaining_refs
        ));
    } else if should_remove_file {
        match validate_purge_path(&path, managed_target_dir, watch_dir) {
            Ok(media_path) => {
                if !crate::watcher::is_temp_file_name(&media_path) {
                    match tokio::fs::remove_file(&media_path).await {
                        Ok(_) => media_removed = true,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => media_removed = false,
                        Err(e) => {
                            let msg = format!("Failed to delete media file '{}': {}", path, e);
                            tracing::warn!("{}", msg);
                            cleanup_failures.push(msg);
                        }
                    }

                    let sidecar_path = crate::identity::sidecar_path_for(&media_path);
                    match tokio::fs::remove_file(&sidecar_path).await {
                        Ok(_) => sidecar_removed = true,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => sidecar_removed = false,
                        Err(e) => {
                            let msg = format!(
                                "Failed to delete sidecar file '{}': {}",
                                sidecar_path.display(),
                                e
                            );
                            tracing::warn!("{}", msg);
                            cleanup_failures.push(msg);
                        }
                    }

                    // Also cleanup legacy adjacent sidecar if distinct from sidecar_path
                    let legacy_sidecar = media_path.with_extension("uuid.json");
                    if legacy_sidecar != sidecar_path && legacy_sidecar.exists() {
                        if tokio::fs::remove_file(&legacy_sidecar).await.is_ok() {
                            sidecar_removed = true;
                        }
                    }
                }
            }
            Err(e) => {
                warnings.push(format!("Physical file cleanup skipped: {}", e));
            }
        }
    }

    Ok(StructuredPurgeResult {
        operation: "purge_asset".to_string(),
        rows_deleted: 1,
        media_removed,
        sidecar_removed,
        skipped_referenced_files,
        cleanup_failures,
        warnings,
    })
}

/// Purge all trashed assets under a virtual folder path.
pub async fn purge_folder_with_context(
    pool: &SqlitePool,
    folder_path: &str,
    mode: PurgeMode,
    managed_target_dir: Option<&Path>,
    watch_dir: Option<&Path>,
) -> Result<StructuredPurgeResult, sqlx::Error> {
    let norm = if folder_path == "/" { "/" } else { folder_path.trim_end_matches('/') };
    let assets: Vec<MediaAsset> = if norm == "/" {
        let sql = format!(
            "SELECT {} FROM media_assets WHERE deleted_at IS NOT NULL",
            SELECT_COLS
        );
        sqlx::query_as::<_, MediaAsset>(&sql).fetch_all(pool).await?
    } else {
        let prefix = format!("{}/%", norm);
        let sql = format!(
            "SELECT {} FROM media_assets WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2 OR virtual_folder = ?1 OR virtual_folder LIKE ?2) AND deleted_at IS NOT NULL",
            SELECT_COLS
        );
        sqlx::query_as::<_, MediaAsset>(&sql)
            .bind(norm)
            .bind(prefix)
            .fetch_all(pool)
            .await?
    };

    let mut total_rows = 0;
    let mut any_media = false;
    let mut any_sidecar = false;
    let mut all_skipped = Vec::new();
    let mut all_failures = Vec::new();
    let mut all_warnings = Vec::new();

    for a in assets {
        let res = purge_single_asset_with_context(
            pool,
            &a.uuid,
            mode,
            managed_target_dir,
            watch_dir,
        )
        .await?;
        total_rows += res.rows_deleted;
        any_media = any_media || res.media_removed;
        any_sidecar = any_sidecar || res.sidecar_removed;
        all_skipped.extend(res.skipped_referenced_files);
        all_failures.extend(res.cleanup_failures);
        all_warnings.extend(res.warnings);
    }

    Ok(StructuredPurgeResult {
        operation: "purge_folder".to_string(),
        rows_deleted: total_rows,
        media_removed: any_media,
        sidecar_removed: any_sidecar,
        skipped_referenced_files: all_skipped,
        cleanup_failures: all_failures,
        warnings: all_warnings,
    })
}

/// Purge all items in the Recycle Bin.
pub async fn purge_recycle_bin_with_context(
    pool: &SqlitePool,
    mode: PurgeMode,
    managed_target_dir: Option<&Path>,
    watch_dir: Option<&Path>,
) -> Result<StructuredPurgeResult, sqlx::Error> {
    let trashed = list_recycle_bin(pool).await?;
    let mut total_rows = 0;
    let mut any_media = false;
    let mut any_sidecar = false;
    let mut all_skipped = Vec::new();
    let mut all_failures = Vec::new();
    let mut all_warnings = Vec::new();

    for a in trashed {
        let res = purge_single_asset_with_context(
            pool,
            &a.uuid,
            mode,
            managed_target_dir,
            watch_dir,
        )
        .await?;
        total_rows += res.rows_deleted;
        any_media = any_media || res.media_removed;
        any_sidecar = any_sidecar || res.sidecar_removed;
        all_skipped.extend(res.skipped_referenced_files);
        all_failures.extend(res.cleanup_failures);
        all_warnings.extend(res.warnings);
    }

    Ok(StructuredPurgeResult {
        operation: "purge_recycle_bin".to_string(),
        rows_deleted: total_rows,
        media_removed: any_media,
        sidecar_removed: any_sidecar,
        skipped_referenced_files: all_skipped,
        cleanup_failures: all_failures,
        warnings: all_warnings,
    })
}

/// Purge all trashed items older than max_age_days.
pub async fn auto_purge_expired_with_context(
    pool: &SqlitePool,
    max_age_days: u32,
    mode: PurgeMode,
    managed_target_dir: Option<&Path>,
    watch_dir: Option<&Path>,
) -> Result<StructuredPurgeResult, sqlx::Error> {
    if max_age_days == 0 {
        return Ok(StructuredPurgeResult {
            operation: "auto_purge".to_string(),
            rows_deleted: 0,
            media_removed: false,
            sidecar_removed: false,
            skipped_referenced_files: Vec::new(),
            cleanup_failures: Vec::new(),
            warnings: vec!["auto_purge_disabled".to_string()],
        });
    }

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(max_age_days as i64)).to_rfc3339();
    let sql = format!(
        "SELECT {} FROM media_assets WHERE deleted_at IS NOT NULL AND deleted_at <= ?1 ORDER BY deleted_at ASC",
        SELECT_COLS
    );
    let expired: Vec<MediaAsset> = sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(&cutoff)
        .fetch_all(pool)
        .await?;

    let mut total_rows = 0;
    let mut any_media = false;
    let mut any_sidecar = false;
    let mut all_skipped = Vec::new();
    let mut all_failures = Vec::new();
    let mut all_warnings = Vec::new();

    for a in expired {
        let res = purge_single_asset_with_context(
            pool,
            &a.uuid,
            mode,
            managed_target_dir,
            watch_dir,
        )
        .await?;
        total_rows += res.rows_deleted;
        any_media = any_media || res.media_removed;
        any_sidecar = any_sidecar || res.sidecar_removed;
        all_skipped.extend(res.skipped_referenced_files);
        all_failures.extend(res.cleanup_failures);
        all_warnings.extend(res.warnings);
    }

    Ok(StructuredPurgeResult {
        operation: "auto_purge".to_string(),
        rows_deleted: total_rows,
        media_removed: any_media,
        sidecar_removed: any_sidecar,
        skipped_referenced_files: all_skipped,
        cleanup_failures: all_failures,
        warnings: all_warnings,
    })
}

pub async fn purge_asset_with_mode(
    pool: &SqlitePool,
    uuid: &str,
    mode: PurgeMode,
) -> Result<PurgeOutcome, sqlx::Error> {
    let res = purge_single_asset_with_context(pool, uuid, mode, None, None).await?;
    Ok(PurgeOutcome {
        rows_deleted: res.rows_deleted,
        file_removed: res.media_removed,
        sidecar_removed: res.sidecar_removed,
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
    let result = sqlx::query("UPDATE media_assets SET display_name = ?1 WHERE uuid = ?2 AND deleted_at IS NULL")
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
    let result = sqlx::query("UPDATE media_assets SET virtual_folder = ?1 WHERE uuid = ?2 AND deleted_at IS NULL")
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
    if let Some(status) = status_filter {
        let filtered = format!(
            "SELECT {} FROM media_assets WHERE status = ?1 AND deleted_at IS NULL ORDER BY uuid",
            SELECT_COLS
        );
        sqlx::query_as::<_, MediaAsset>(&filtered)
            .bind(status)
            .fetch_all(pool)
            .await
    } else {
        let sql = format!(
            "SELECT {} FROM media_assets WHERE deleted_at IS NULL ORDER BY uuid",
            SELECT_COLS
        );
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
        "SELECT {} FROM media_assets WHERE uuid IN ({}) AND deleted_at IS NULL",
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

// ── Database Viewer Read-Only Query Models & Handlers ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbOverview {
    pub total_assets: i64,
    pub master_clips: i64,
    pub subclips: i64,
    pub trashed_assets: i64,
    pub ready_assets: i64,
    pub processing_assets: i64,
    pub error_assets: i64,
    pub total_jobs: i64,
    pub pending_jobs: i64,
    pub processing_jobs: i64,
    pub completed_jobs: i64,
    pub failed_jobs: i64,
    pub cancelled_jobs: i64,
    pub db_size_bytes: i64,
    pub wal_mode: bool,
}

pub async fn get_db_overview(pool: &SqlitePool) -> Result<DbOverview, sqlx::Error> {
    let (total_assets,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM media_assets WHERE deleted_at IS NULL")
            .fetch_one(pool)
            .await?;
    let (trashed_assets,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM media_assets WHERE deleted_at IS NOT NULL")
            .fetch_one(pool)
            .await?;
    let (subclips,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM media_assets WHERE (trim_in_ms > 0 OR (trim_out_ms > 0 AND trim_out_ms < duration_ms)) AND deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    let master_clips = (total_assets - subclips).max(0);

    let (ready_assets,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM media_assets WHERE status = 'ready' AND deleted_at IS NULL")
            .fetch_one(pool)
            .await?;
    let (processing_assets,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM media_assets WHERE status = 'processing' AND deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    let (error_assets,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM media_assets WHERE status = 'error' AND deleted_at IS NULL")
            .fetch_one(pool)
            .await?;

    let (total_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs").fetch_one(pool).await?;
    let (pending_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE state = 'Pending'")
            .fetch_one(pool)
            .await?;
    let (processing_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE state = 'Processing'")
            .fetch_one(pool)
            .await?;
    let (completed_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE state = 'Completed'")
            .fetch_one(pool)
            .await?;
    let (failed_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE state = 'Failed'")
            .fetch_one(pool)
            .await?;
    let (cancelled_jobs,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE state = 'Cancelled'")
            .fetch_one(pool)
            .await?;

    let page_count: (i64,) = sqlx::query_as("PRAGMA page_count")
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
    let page_size: (i64,) = sqlx::query_as("PRAGMA page_size")
        .fetch_one(pool)
        .await
        .unwrap_or((4096,));
    let db_size_bytes = page_count.0 * page_size.0;

    let journal_mode: (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap_or(("".into(),));
    let wal_mode = journal_mode.0.to_ascii_lowercase() == "wal";

    Ok(DbOverview {
        total_assets,
        master_clips,
        subclips,
        trashed_assets,
        ready_assets,
        processing_assets,
        error_assets,
        total_jobs,
        pending_jobs,
        processing_jobs,
        completed_jobs,
        failed_jobs,
        cancelled_jobs,
        db_size_bytes,
        wal_mode,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAssetSummary {
    pub uuid: String,
    pub fingerprint: i64,
    pub current_path: String,
    pub display_path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub rating: String,
    pub tp: String,
    pub status: String,
    pub display_name: String,
    pub virtual_folder: String,
    pub original_virtual_folder: Option<String>,
    pub mezzanine_ok: bool,
    pub fps: f64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub keyframe_count: usize,
    pub warnings: Vec<String>,
    pub is_subclip: bool,
    pub parent_uuid: Option<String>,
    pub deleted_at: Option<String>,
    pub sidecar_exists: bool,
}

impl DbAssetSummary {
    pub fn from_asset(a: MediaAsset) -> Self {
        let is_subclip = a.trim_in_ms > 0
            || (a.trim_out_ms > 0 && a.trim_out_ms < a.duration_ms)
            || a.display_name.to_ascii_lowercase().contains("subclip")
            || a.display_name.to_ascii_lowercase().contains("sub-clip");

        let display_path = a
            .current_path
            .split('\\')
            .last()
            .unwrap_or(&a.current_path)
            .split('/')
            .last()
            .unwrap_or(&a.current_path)
            .to_string();

        let keyframe_count = serde_json::from_str::<Vec<i64>>(&a.keyframe_offsets_json)
            .map(|v| v.len())
            .unwrap_or(0);

        let warnings = serde_json::from_str::<Vec<String>>(&a.warnings).unwrap_or_default();

        let sidecar_exists = if !a.current_path.is_empty() {
            let p = std::path::Path::new(&a.current_path);
            crate::identity::sidecar_path_for(p).exists()
        } else {
            false
        };

        Self {
            uuid: a.uuid,
            fingerprint: a.fingerprint,
            current_path: a.current_path,
            display_path,
            duration_ms: a.duration_ms,
            trim_in_ms: a.trim_in_ms,
            trim_out_ms: a.trim_out_ms,
            rating: a.rating,
            tp: a.tp,
            status: a.status,
            display_name: a.display_name,
            virtual_folder: a.virtual_folder,
            original_virtual_folder: a.original_virtual_folder,
            mezzanine_ok: a.mezzanine_ok,
            fps: a.fps,
            fps_num: a.fps_num,
            fps_den: a.fps_den,
            total_frames: a.total_frames,
            gop_frames: a.gop_frames,
            keyframe_safe_start_ms: a.keyframe_safe_start_ms,
            keyframe_count,
            warnings,
            is_subclip,
            parent_uuid: None,
            deleted_at: a.deleted_at,
            sidecar_exists,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAssetsPage {
    pub items: Vec<DbAssetSummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

pub async fn query_db_assets(
    pool: &SqlitePool,
    filter: Option<&str>,
    search: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<DbAssetsPage, sqlx::Error> {
    let lim = limit.unwrap_or(25).clamp(1, 100);
    let off = offset.unwrap_or(0).max(0);

    let all_assets: Vec<MediaAsset> = sqlx::query_as(&format!(
        "SELECT {} FROM media_assets ORDER BY COALESCE(deleted_at, '9999') ASC, display_name ASC, uuid ASC",
        SELECT_COLS
    ))
    .fetch_all(pool)
    .await?;

    let filter_mode = filter.unwrap_or("all").to_ascii_lowercase();
    let search_term = search.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();

    let mut filtered: Vec<DbAssetSummary> = all_assets
        .into_iter()
        .map(DbAssetSummary::from_asset)
        .filter(|a| {
            // Apply filter
            let matches_filter = match filter_mode.as_str() {
                "master" => !a.is_subclip && a.deleted_at.is_none(),
                "subclip" => a.is_subclip && a.deleted_at.is_none(),
                "ready" => a.status == "ready" && a.deleted_at.is_none(),
                "processing" => a.status == "processing" && a.deleted_at.is_none(),
                "error" => a.status == "error" && a.deleted_at.is_none(),
                "trashed" => a.deleted_at.is_some(),
                _ => true,
            };
            if !matches_filter {
                return false;
            }

            // Apply search
            if search_term.is_empty() {
                return true;
            }
            a.display_name.to_ascii_lowercase().contains(&search_term)
                || a.uuid.to_ascii_lowercase().contains(&search_term)
                || a.virtual_folder.to_ascii_lowercase().contains(&search_term)
                || a.current_path.to_ascii_lowercase().contains(&search_term)
        })
        .collect();

    let total = filtered.len() as i64;
    let start = (off as usize).min(filtered.len());
    let end = (start + lim as usize).min(filtered.len());
    let items = filtered.drain(start..end).collect();

    Ok(DbAssetsPage {
        items,
        total,
        limit: lim,
        offset: off,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAssetDetail {
    pub summary: DbAssetSummary,
    pub keyframe_sample: Vec<i64>,
    pub warnings_json: String,
    pub keyframe_offsets_json: String,
}

pub async fn get_db_asset_detail(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<DbAssetDetail>, sqlx::Error> {
    let raw = find_by_uuid_raw(pool, uuid).await?;
    let Some(a) = raw else {
        return Ok(None);
    };

    let sample: Vec<i64> = serde_json::from_str::<Vec<i64>>(&a.keyframe_offsets_json)
        .map(|v| v.into_iter().take(100).collect())
        .unwrap_or_default();

    let warnings_json = a.warnings.clone();
    let keyframe_offsets_json = a.keyframe_offsets_json.clone();
    let summary = DbAssetSummary::from_asset(a);

    Ok(Some(DbAssetDetail {
        summary,
        keyframe_sample: sample,
        warnings_json,
        keyframe_offsets_json,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbJobSummary {
    pub id: String,
    pub input_path: String,
    pub input_path_display: String,
    pub output_path: Option<String>,
    pub output_path_display: Option<String>,
    pub profile: String,
    pub uuid: Option<String>,
    pub state: String,
    pub phase: String,
    pub progress: f64,
    pub current_stage: String,
    pub duration_secs: f64,
    pub error: Option<String>,
    pub error_category: Option<String>,
    pub attempt: i64,
    pub max_attempts: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub worker_id: Option<String>,
    pub encode_fps: f64,
    pub encode_bitrate: String,
    pub encode_speed: String,
    pub stderr_lines_count: usize,
}

impl DbJobSummary {
    pub fn from_row(r: DurableJobRow) -> Self {
        let input_path_display = r
            .input_path
            .split('\\')
            .last()
            .unwrap_or(&r.input_path)
            .split('/')
            .last()
            .unwrap_or(&r.input_path)
            .to_string();

        let output_path_display = r.output_path.as_ref().map(|p| {
            p.split('\\')
                .last()
                .unwrap_or(p)
                .split('/')
                .last()
                .unwrap_or(p)
                .to_string()
        });

        let stderr_lines_count = r
            .stderr_log_json
            .as_ref()
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
            .map(|v| v.len())
            .unwrap_or(0);

        Self {
            id: r.id,
            input_path: r.input_path,
            input_path_display,
            output_path: r.output_path,
            output_path_display,
            profile: r.profile,
            uuid: r.uuid,
            state: r.state,
            phase: r.phase,
            progress: r.progress,
            current_stage: r.current_stage,
            duration_secs: r.duration_secs,
            error: r.error,
            error_category: r.error_category,
            attempt: r.attempt,
            max_attempts: r.max_attempts,
            created_at: r.created_at,
            started_at: r.started_at,
            finished_at: r.finished_at,
            worker_id: r.worker_id,
            encode_fps: r.encode_fps,
            encode_bitrate: r.encode_bitrate,
            encode_speed: r.encode_speed,
            stderr_lines_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbJobsPage {
    pub items: Vec<DbJobSummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

pub async fn query_db_jobs(
    pool: &SqlitePool,
    state: Option<&str>,
    search: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<DbJobsPage, sqlx::Error> {
    let lim = limit.unwrap_or(25).clamp(1, 100);
    let off = offset.unwrap_or(0).max(0);

    let rows: Vec<DurableJobRow> =
        sqlx::query_as("SELECT * FROM transcode_jobs ORDER BY created_at DESC")
            .fetch_all(pool)
            .await?;

    let state_term = state.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    let search_term = search.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();

    let mut filtered: Vec<DbJobSummary> = rows
        .into_iter()
        .map(DbJobSummary::from_row)
        .filter(|j| {
            if !state_term.is_empty() && state_term != "all" {
                if j.state.to_ascii_lowercase() != state_term
                    && j.phase.to_ascii_lowercase() != state_term
                {
                    return false;
                }
            }
            if search_term.is_empty() {
                return true;
            }
            j.id.to_ascii_lowercase().contains(&search_term)
                || j.uuid
                    .as_ref()
                    .map(|u| u.to_ascii_lowercase().contains(&search_term))
                    .unwrap_or(false)
                || j.input_path.to_ascii_lowercase().contains(&search_term)
                || j.output_path
                    .as_ref()
                    .map(|o| o.to_ascii_lowercase().contains(&search_term))
                    .unwrap_or(false)
                || j.error
                    .as_ref()
                    .map(|e| e.to_ascii_lowercase().contains(&search_term))
                    .unwrap_or(false)
        })
        .collect();

    let total = filtered.len() as i64;
    let start = (off as usize).min(filtered.len());
    let end = (start + lim as usize).min(filtered.len());
    let items = filtered.drain(start..end).collect();

    Ok(DbJobsPage {
        items,
        total,
        limit: lim,
        offset: off,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbJobDetail {
    pub summary: DbJobSummary,
    pub stderr_log_tail: Vec<String>,
    pub fingerprint: Option<i64>,
    pub request_hash: Option<String>,
    pub leased_until: Option<String>,
    pub heartbeat_at: Option<String>,
    pub cancel_requested: bool,
}

pub async fn get_db_job_detail(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<DbJobDetail>, sqlx::Error> {
    let row: Option<DurableJobRow> =
        sqlx::query_as("SELECT * FROM transcode_jobs WHERE id = ?1 LIMIT 1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

    let Some(r) = row else {
        return Ok(None);
    };

    let stderr_tail: Vec<String> = r
        .stderr_log_json
        .as_ref()
        .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
        .map(|v| {
            let total = v.len();
            if total > 100 {
                v.into_iter().skip(total - 100).collect()
            } else {
                v
            }
        })
        .unwrap_or_default();

    let fingerprint = r.fingerprint;
    let request_hash = r.request_hash.clone();
    let leased_until = r.leased_until.clone();
    let heartbeat_at = r.heartbeat_at.clone();
    let cancel_requested = r.cancel_requested;
    let summary = DbJobSummary::from_row(r);

    Ok(Some(DbJobDetail {
        summary,
        stderr_log_tail: stderr_tail,
        fingerprint,
        request_hash,
        leased_until,
        heartbeat_at,
        cancel_requested,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFolderItem {
    pub virtual_folder: String,
    pub color: Option<String>,
    pub asset_count: i64,
    pub ready_count: i64,
    pub trashed_count: i64,
}

pub async fn get_db_folders(pool: &SqlitePool) -> Result<Vec<DbFolderItem>, sqlx::Error> {
    let colors = get_all_folder_colors(pool).await?;
    let color_map: std::collections::HashMap<String, String> =
        colors.into_iter().map(|c| (c.virtual_folder, c.color)).collect();

    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT virtual_folder,
                COUNT(*) as asset_count,
                SUM(CASE WHEN status = 'ready' AND deleted_at IS NULL THEN 1 ELSE 0 END) as ready_count,
                SUM(CASE WHEN deleted_at IS NOT NULL THEN 1 ELSE 0 END) as trashed_count
         FROM media_assets
         GROUP BY virtual_folder
         ORDER BY virtual_folder ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut result = Vec::new();
    let mut seen_folders = std::collections::HashSet::new();

    for (vf, asset_count, ready_count, trashed_count) in rows {
        seen_folders.insert(vf.clone());
        let color = color_map.get(&vf).cloned();
        result.push(DbFolderItem {
            virtual_folder: vf,
            color,
            asset_count,
            ready_count,
            trashed_count,
        });
    }

    // Add any configured color folders that currently have 0 assets
    for (vf, col) in color_map {
        if !seen_folders.contains(&vf) {
            result.push(DbFolderItem {
                virtual_folder: vf,
                color: Some(col),
                asset_count: 0,
                ready_count: 0,
                trashed_count: 0,
            });
        }
    }

    result.sort_by(|a, b| a.virtual_folder.cmp(&b.virtual_folder));
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTableColumnInfo {
    pub cid: i64,
    pub name: String,
    pub col_type: String,
    pub not_null: bool,
    pub is_pk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTableSchema {
    pub table_name: String,
    pub row_count: i64,
    pub columns: Vec<DbTableColumnInfo>,
}

pub async fn get_db_schema(pool: &SqlitePool) -> Result<Vec<DbTableSchema>, sqlx::Error> {
    let table_names = vec![
        "media_assets",
        "transcode_jobs",
        "virtual_folder_colors",
    ];

    let mut schemas = Vec::new();
    for t_name in table_names {
        let (row_count,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {}", t_name))
            .fetch_one(pool)
            .await
            .unwrap_or((0,));

        let col_rows: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as(&format!("PRAGMA table_info({})", t_name))
                .fetch_all(pool)
                .await
                .unwrap_or_default();

        let columns = col_rows
            .into_iter()
            .map(|(cid, name, col_type, not_null, _, pk)| DbTableColumnInfo {
                cid,
                name,
                col_type,
                not_null: not_null != 0,
                is_pk: pk != 0,
            })
            .collect();

        schemas.push(DbTableSchema {
            table_name: t_name.to_string(),
            row_count,
            columns,
        });
    }

    Ok(schemas)
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

    #[tokio::test]
    async fn test_soft_delete_single_asset_and_restore() {
        let (pool, temp_dir) = setup_test_pool().await;
        let uuid = "test-soft-del-1";
        insert_processing(&pool, uuid, 12345, "D:/target/clip1.mp4", "Clip 1")
            .await
            .unwrap();
        mark_ready(&pool, uuid, "D:/target/clip1.mp4", 5000, true, 25.0, 25, 1, 125, 50, 0, &[], "[]")
            .await
            .unwrap();
        set_virtual_folder(&pool, uuid, "/Shows/Drama").await.unwrap();

        // 1. Asset is active
        let active = find_by_uuid(&pool, uuid).await.unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().virtual_folder, "/Shows/Drama");

        // 2. Soft delete / trash asset
        let trashed = trash_asset(&pool, uuid).await.unwrap();
        assert!(trashed);

        // 3. Active queries must exclude trashed asset
        let active_after = find_by_uuid(&pool, uuid).await.unwrap();
        assert!(active_after.is_none(), "Active query must exclude trashed asset");

        let all_active = find_all(&pool, None).await.unwrap();
        assert!(all_active.is_empty(), "find_all must exclude trashed asset");

        // 4. Recycle bin query contains trashed asset
        let bin = list_recycle_bin(&pool).await.unwrap();
        assert_eq!(bin.len(), 1);
        assert_eq!(bin[0].uuid, uuid);
        assert!(bin[0].deleted_at.is_some());
        assert_eq!(bin[0].original_virtual_folder.as_deref(), Some("/Shows/Drama"));

        // 5. Restore asset to original folder
        let restored = restore_asset(&pool, uuid, None).await.unwrap();
        assert!(restored.is_some());
        let r = restored.unwrap();
        assert_eq!(r.virtual_folder, "/Shows/Drama");
        assert!(r.deleted_at.is_none());
        assert!(r.original_virtual_folder.is_none());

        // 6. Active query finds it again
        let active_restored = find_by_uuid(&pool, uuid).await.unwrap();
        assert!(active_restored.is_some());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_soft_delete_folder_and_prefix_boundary_isolation() {
        let (pool, temp_dir) = setup_test_pool().await;

        // Create 3 assets:
        // 1: /Shows/Drama
        // 2: /Shows/Drama/Season1
        // 3: /Shows/Dramatic (MUST NOT BE TRASHED BY /Shows/Drama!)
        insert_processing(&pool, "u1", 1, "D:/target/c1.mp4", "C1").await.unwrap();
        set_virtual_folder(&pool, "u1", "/Shows/Drama").await.unwrap();

        insert_processing(&pool, "u2", 2, "D:/target/c2.mp4", "C2").await.unwrap();
        set_virtual_folder(&pool, "u2", "/Shows/Drama/Season1").await.unwrap();

        insert_processing(&pool, "u3", 3, "D:/target/c3.mp4", "C3").await.unwrap();
        set_virtual_folder(&pool, "u3", "/Shows/Dramatic").await.unwrap();

        // Trash /Shows/Drama
        let affected = trash_folder(&pool, "/Shows/Drama").await.unwrap();
        assert_eq!(affected, 2);

        // /Shows/Dramatic must remain active
        let active3 = find_by_uuid(&pool, "u3").await.unwrap();
        assert!(active3.is_some(), "/Shows/Dramatic must not be affected by /Shows/Drama delete");

        // Active list should have only 1 asset
        let active_list = find_all(&pool, None).await.unwrap();
        assert_eq!(active_list.len(), 1);
        assert_eq!(active_list[0].uuid, "u3");

        // Recycle bin has 2 items
        let bin = list_recycle_bin(&pool).await.unwrap();
        assert_eq!(bin.len(), 2);

        // Restore folder /Shows/Drama
        let restored_count = restore_folder(&pool, "/Shows/Drama", false).await.unwrap();
        assert_eq!(restored_count, 2);

        let active_list_after = find_all(&pool, None).await.unwrap();
        assert_eq!(active_list_after.len(), 3);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_restore_folder_fallback_to_root() {
        let (pool, temp_dir) = setup_test_pool().await;
        insert_processing(&pool, "u10", 10, "D:/target/c10.mp4", "C10").await.unwrap();
        set_virtual_folder(&pool, "u10", "/OldShows/SeriesA").await.unwrap();

        trash_folder(&pool, "/OldShows").await.unwrap();

        // Restore with fallback_to_root = true
        let restored = restore_folder(&pool, "/OldShows", true).await.unwrap();
        assert_eq!(restored, 1);

        let a = find_by_uuid(&pool, "u10").await.unwrap().unwrap();
        assert_eq!(a.virtual_folder, "/", "Should fallback to root '/'");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_path_safety_validation() {
        let temp_target = std::env::temp_dir().join("transcode_target_safe");
        let temp_watch = std::env::temp_dir().join("transcode_watch_safe");
        let _ = std::fs::create_dir_all(&temp_target);
        let _ = std::fs::create_dir_all(&temp_watch);

        let safe_file = temp_target.join("output.mp4");
        let _ = std::fs::File::create(&safe_file);

        let watch_file = temp_watch.join("source.mp4");
        let _ = std::fs::File::create(&watch_file);

        // 1. Safe target file succeeds
        let valid = validate_purge_path(
            &safe_file.to_string_lossy(),
            Some(&temp_target),
            Some(&temp_watch),
        );
        assert!(valid.is_ok());

        // 2. Source file in watch folder is rejected!
        let in_watch = validate_purge_path(
            &watch_file.to_string_lossy(),
            Some(&temp_target),
            Some(&temp_watch),
        );
        assert!(in_watch.is_err(), "Must reject source file in watch folder");

        // 3. Path traversal is rejected!
        let traversal = validate_purge_path(
            &format!("{}/../etc/passwd", temp_target.to_string_lossy()),
            Some(&temp_target),
            Some(&temp_watch),
        );
        assert!(traversal.is_err(), "Must reject path traversal");

        // 4. Root is rejected!
        assert!(validate_purge_path("/", Some(&temp_target), Some(&temp_watch)).is_err());

        let _ = std::fs::remove_dir_all(&temp_target);
        let _ = std::fs::remove_dir_all(&temp_watch);
    }

    #[tokio::test]
    async fn test_purge_single_asset_with_real_files_and_sidecar() {
        let (pool, temp_dir) = setup_test_pool().await;
        let media_file = temp_dir.join("mezzanine_video.mp4");
        let sidecar_file = crate::identity::sidecar_path_for(&media_file);
        std::fs::File::create(&media_file).unwrap();
        std::fs::File::create(&sidecar_file).unwrap();

        let uuid = "purge-test-uuid";
        insert_processing(&pool, uuid, 8888, &media_file.to_string_lossy(), "Mezzanine")
            .await
            .unwrap();

        // Purge asset
        let result = purge_single_asset_with_context(
            &pool,
            uuid,
            PurgeMode::PreserveReferencedMezzanine,
            Some(&temp_dir),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.rows_deleted, 1);
        assert!(result.media_removed);
        assert!(result.sidecar_removed);
        assert!(!media_file.exists(), "Media file must be deleted");
        assert!(!sidecar_file.exists(), "Sidecar file must be deleted");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_auto_purge_retention_cutoff() {
        let (pool, temp_dir) = setup_test_pool().await;

        // Old asset deleted 15 days ago
        let old_time = (chrono::Utc::now() - chrono::Duration::days(15)).to_rfc3339();
        insert_processing(&pool, "old-asset", 111, "D:/target/old.mp4", "Old").await.unwrap();
        sqlx::query("UPDATE media_assets SET deleted_at = ?1 WHERE uuid = 'old-asset'")
            .bind(&old_time)
            .execute(&pool)
            .await
            .unwrap();

        // Recent asset deleted 2 days ago
        let recent_time = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        insert_processing(&pool, "recent-asset", 222, "D:/target/recent.mp4", "Recent").await.unwrap();
        sqlx::query("UPDATE media_assets SET deleted_at = ?1 WHERE uuid = 'recent-asset'")
            .bind(&recent_time)
            .execute(&pool)
            .await
            .unwrap();

        // Run auto-purge with 14-day policy
        let result = auto_purge_expired_with_context(
            &pool,
            14,
            PurgeMode::PreserveReferencedMezzanine,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.rows_deleted, 1, "Only 15-day-old asset should be purged");

        // Old asset is gone from DB
        let old_check = find_by_uuid_raw(&pool, "old-asset").await.unwrap();
        assert!(old_check.is_none());

        // Recent asset is still in recycle bin
        let recent_check = find_by_uuid_raw(&pool, "recent-asset").await.unwrap();
        assert!(recent_check.is_some());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_db_viewer_overview_and_assets() {
        let (pool, temp_dir) = setup_test_pool().await;

        // 1. Insert master clip
        insert_processing(&pool, "asset-master", 1001, "D:/target/master.mp4", "Master 1").await.unwrap();
        mark_ready(&pool, "asset-master", "D:/target/master.mp4", 10000, true, 25.0, 25, 1, 250, 50, 0, &["warning1".into()], "[]").await.unwrap();

        // 2. Insert subclip
        create_subclip(&pool, "asset-sub", "asset-master", "Subclip 1", 1000, 5000, true, "[]").await.unwrap();

        // 3. Insert trashed asset
        insert_processing(&pool, "asset-trashed", 1002, "D:/target/trashed.mp4", "Trashed 1").await.unwrap();
        trash_asset(&pool, "asset-trashed").await.unwrap();

        // 4. Test Overview
        let overview = get_db_overview(&pool).await.unwrap();
        assert_eq!(overview.total_assets, 2, "Active assets count should be 2");
        assert_eq!(overview.master_clips, 1, "Master clips count should be 1");
        assert_eq!(overview.subclips, 1, "Subclips count should be 1");
        assert_eq!(overview.trashed_assets, 1, "Trashed assets count should be 1");
        assert_eq!(overview.ready_assets, 2, "Ready assets count should be 2");
        assert!(overview.wal_mode, "WAL mode should be true");

        // 5. Test query_db_assets with filters
        let all_page = query_db_assets(&pool, Some("all"), None, Some(10), Some(0)).await.unwrap();
        assert_eq!(all_page.total, 3, "Total records including trashed should be 3");

        let master_page = query_db_assets(&pool, Some("master"), None, Some(10), Some(0)).await.unwrap();
        assert_eq!(master_page.items.len(), 1);
        assert_eq!(master_page.items[0].uuid, "asset-master");
        assert!(!master_page.items[0].is_subclip);

        let subclip_page = query_db_assets(&pool, Some("subclip"), None, Some(10), Some(0)).await.unwrap();
        assert_eq!(subclip_page.items.len(), 1);
        assert_eq!(subclip_page.items[0].uuid, "asset-sub");
        assert!(subclip_page.items[0].is_subclip);

        let trashed_page = query_db_assets(&pool, Some("trashed"), None, Some(10), Some(0)).await.unwrap();
        assert_eq!(trashed_page.items.len(), 1);
        assert_eq!(trashed_page.items[0].uuid, "asset-trashed");

        // 6. Test search
        let search_page = query_db_assets(&pool, None, Some("Master 1"), Some(10), Some(0)).await.unwrap();
        assert_eq!(search_page.items.len(), 1);
        assert_eq!(search_page.items[0].uuid, "asset-master");

        // 7. Test asset detail
        let detail = get_db_asset_detail(&pool, "asset-master").await.unwrap().unwrap();
        assert_eq!(detail.summary.uuid, "asset-master");
        assert_eq!(detail.summary.warnings.len(), 1);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_db_viewer_jobs_and_schema() {
        let (pool, temp_dir) = setup_test_pool().await;

        let job = crate::jobs::JobRecord::new("D:/watch/video.mp4", "A");
        insert_durable_job(&pool, &job).await.unwrap();

        // 1. Query jobs
        let jobs_page = query_db_jobs(&pool, None, None, Some(10), Some(0)).await.unwrap();
        assert_eq!(jobs_page.total, 1);
        assert_eq!(jobs_page.items[0].id, job.id);
        assert_eq!(jobs_page.items[0].state, "Pending");

        // 2. Job detail
        let job_detail = get_db_job_detail(&pool, &job.id).await.unwrap().unwrap();
        assert_eq!(job_detail.summary.id, job.id);

        // 3. Schema
        let schema = get_db_schema(&pool).await.unwrap();
        assert!(schema.iter().any(|s| s.table_name == "media_assets"));
        assert!(schema.iter().any(|s| s.table_name == "transcode_jobs"));
        assert!(schema.iter().any(|s| s.table_name == "virtual_folder_colors"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

