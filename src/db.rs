use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
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
    /// SHA-256 of the *source* file, hex. `None` for rows written before T2-6,
    /// which is why dedup treats a missing value as "cannot confirm".
    pub source_sha256: Option<String>,
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

const SELECT_COLS: &str = "uuid, fingerprint, source_sha256, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json, deleted_at, original_virtual_folder";

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

// ---------------------------------------------------------------------------
// Registry backups (T2-13)
//
// The asset registry is the playout source of truth. It holds every uuid,
// virtual folder, rating, trim window and compliance flag an operator has ever
// set, and none of it can be reconstructed from the media files. Losing it
// means re-ingesting the library and re-entering every piece of metadata by
// hand -- and SQLite files do get lost, to a full volume mid-write, to antivirus
// quarantine, to someone copying a WAL-mode database while the service runs.
//
// `VACUUM INTO` is the right primitive: it is a consistent snapshot taken
// through the same connection pool, safe while the service is writing, and the
// result is a plain defragmented database file. Copying the file is not safe;
// `.backup` needs the CLI.
// ---------------------------------------------------------------------------

/// Directory under the data directory that holds registry snapshots.
pub const BACKUP_DIR_NAME: &str = "backups";

/// How many daily snapshots to keep. Two weeks of retention would be nicer, but
/// a snapshot is roughly the size of the live registry, and an operator who has
/// not noticed a problem in a week is not going to notice it in two.
pub const BACKUP_RETENTION: usize = 7;

/// A snapshot on disk.
#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    pub file_name: String,
    pub size_bytes: u64,
    /// RFC 3339, from the filesystem.
    pub created_at: Option<String>,
}

/// Where snapshots live for a given data directory.
pub fn backup_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(BACKUP_DIR_NAME)
}

/// The snapshot filename for a date, e.g. `media_assets-2026-09-18.db`.
///
/// Date-stamped rather than timestamped on purpose: a second backup on the same
/// day overwrites the first, so an hourly trigger cannot fill the volume.
pub fn backup_file_name(date: &str) -> String {
    format!("media_assets-{}.db", date)
}

/// Take a consistent snapshot of the registry into `<data_dir>/backups/`.
///
/// Returns the path written. Safe to call while the service is running and
/// writing; `VACUUM INTO` takes its own read transaction.
pub async fn backup_now(pool: &SqlitePool, data_dir: &Path) -> Result<std::path::PathBuf, String> {
    let dir = backup_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let dest = dir.join(backup_file_name(&date));

    // Checkpoint first, so the snapshot includes everything committed to the
    // WAL rather than only what has been folded back into the main file.
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await;

    // Vacuum into a unique temporary name, then rename it into place.
    //
    // `VACUUM INTO` refuses to overwrite, so the obvious implementation is
    // "delete today's file, then vacuum onto it" -- and that has a window in
    // which today's backup does not exist. Two concurrent calls (the daily task
    // firing while an operator clicks Backup) can then delete the file the
    // other is halfway through writing: the loser errors and the winner leaves
    // a truncated snapshot. Write-then-rename has no such window -- the old
    // snapshot stays intact until a complete new one atomically replaces it.
    let staging = dir.join(format!(
        ".{}.{}.tmp",
        backup_file_name(&date),
        uuid::Uuid::new_v4()
    ));

    // The path is interpolated because SQLite does not accept a bound parameter
    // here. Single quotes are doubled so a path containing one cannot terminate
    // the literal; the value is server-generated, never caller-supplied.
    let escaped = staging.to_string_lossy().replace('\'', "''");
    if let Err(e) = sqlx::query(&format!("VACUUM INTO '{}'", escaped))
        .execute(pool)
        .await
    {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("VACUUM INTO failed: {}", e));
    }

    if let Err(e) = std::fs::rename(&staging, &dest) {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("cannot publish {}: {}", dest.display(), e));
    }

    prune_backups(&dir, BACKUP_RETENTION);
    Ok(dest)
}

/// List snapshots, newest first.
pub fn list_backups(data_dir: &Path) -> Vec<BackupInfo> {
    let dir = backup_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out: Vec<BackupInfo> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("media_assets-") && name.ends_with(".db")
        })
        .map(|e| {
            let meta = e.metadata().ok();
            BackupInfo {
                file_name: e.file_name().to_string_lossy().into_owned(),
                size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                created_at: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
            }
        })
        .collect();

    // By name, which sorts chronologically because the stamp is ISO-8601.
    out.sort_by(|a, b| b.file_name.cmp(&a.file_name));
    out
}

/// Delete all but the newest `keep` snapshots.
pub fn prune_backups(dir: &Path, keep: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("media_assets-") && n.ends_with(".db"))
        .collect();
    names.sort();

    let mut removed = 0;
    while names.len() > keep {
        let oldest = names.remove(0);
        if std::fs::remove_file(dir.join(&oldest)).is_ok() {
            removed += 1;
            tracing::info!("Pruned old registry backup {}", oldest);
        }
    }
    removed
}

pub async fn init_pool(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let db_dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let _ = std::fs::create_dir_all(db_dir);

    // The pragmas belong on the connect options, not on a query after
    // connecting: a `PRAGMA` statement reaches exactly one of the five pooled
    // connections. `journal_mode` happened to work anyway because WAL is
    // recorded in the file; `cache_size` and friends are per connection.
    //
    // `synchronous` is deliberately left at FULL. Under WAL, NORMAL can lose
    // the last committed transactions on power loss, and a lost `mark_ready`
    // leaves a published mezzanine with no `ready` row -- the F-18 orphan
    // class. The registry is the playout source of truth.
    let options = std::str::FromStr::from_str(&format!(
        "sqlite:{}?mode=rwc",
        db_path.display()
    ))
    .map(|o: SqliteConnectOptions| {
        o.pragma("journal_mode", "WAL")
            // 16 MB page cache instead of the 2 MB default.
            .pragma("cache_size", "-16384")
            .pragma("temp_store", "MEMORY")
            // 128 MB read-only mapping; safe under WAL.
            .pragma("mmap_size", "134217728")
    })?;

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS media_assets (
            uuid         TEXT PRIMARY KEY,
            fingerprint  INTEGER NOT NULL,
            source_sha256 TEXT DEFAULT NULL,
            current_path TEXT NOT NULL,
            duration_ms  INTEGER NOT NULL DEFAULT 0,
            trim_in_ms   INTEGER NOT NULL DEFAULT 0,
            trim_out_ms  INTEGER NOT NULL DEFAULT 0,
            rating       TEXT NOT NULL DEFAULT 'NONE',
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
        // T2-6. Nullable on purpose: rows ingested before this column existed
        // have no full hash, and backfilling would mean re-reading the whole
        // library from disk at startup.
        ("source_sha256", "TEXT", "NULL"),
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

    // T2-7. Every paginated listing filters on (status, deleted_at) or orders
    // within a virtual folder; without these the LIMIT/OFFSET still scans the
    // whole table and paging buys nothing but a smaller response.
    for idx in [
        "CREATE INDEX IF NOT EXISTS idx_media_assets_status_deleted ON media_assets(status, deleted_at)",
        "CREATE INDEX IF NOT EXISTS idx_media_assets_virtual_folder ON media_assets(virtual_folder)",
        // Reference-counted purge and subclip protection both ask
        // `WHERE current_path = ?`, which was a full scan.
        "CREATE INDEX IF NOT EXISTS idx_media_assets_current_path ON media_assets(current_path)",
        "CREATE INDEX IF NOT EXISTS idx_transcode_jobs_created_at ON transcode_jobs(created_at)",
    ] {
        let _ = sqlx::query(idx).execute(&pool).await;
    }

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
    source_sha256: Option<&str>,
    path: &str,
    display_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_assets (uuid, fingerprint, source_sha256, current_path, display_name, status, rating) VALUES (?1, ?2, ?3, ?4, ?5, 'processing', ?6)",
    )
    .bind(uuid)
    .bind(fingerprint)
    .bind(source_sha256)
    .bind(path)
    .bind(display_name)
    .bind(DEFAULT_RATING)
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

/// What a re-ingest did to the rows that shared its fingerprint.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FingerprintPurge {
    /// Unusable rows deleted: a failed or half-finished ingest of the whole file.
    pub deleted: u64,
    /// `ready` rows whose mezzanine has gone missing, demoted to `error` so the
    /// operator's metadata survives.
    pub demoted: u64,
    /// Rows deliberately left alone — subclips, and `ready` rows that are fine.
    pub protected: u64,
}

/// Clear the way for a re-ingest of `fingerprint`, without destroying work.
///
/// This used to be `DELETE FROM media_assets WHERE fingerprint = ?`, which is
/// F-26: a subclip carries its **parent's** fingerprint, so re-ingesting a
/// programme deleted every subclip an operator had cut from it, along with
/// their ratings, virtual folders and compliance metadata. None of that is
/// recoverable from the source file.
///
/// The rule now:
///
/// * a **subclip** (trimmed: `trim_in_ms > 0`, or `trim_out_ms` is neither 0
///   nor the full duration) is never touched, whatever its status;
/// * a `ready` full-length row whose file still exists is never touched —
///   the caller only reaches here when it decided the existing asset is not
///   usable, and "not usable" must not mean "delete someone's library entry";
/// * a `ready` full-length row whose file has **gone** is demoted to `error`,
///   not deleted, so the metadata survives for the re-ingest to be reconciled
///   against by an operator;
/// * only `error` / `processing` full-length rows are actually deleted. Those
///   are the leftovers of a failed ingest and carry nothing worth keeping.
///
/// `file_exists` is injected so the decision is unit-testable without a
/// filesystem.
pub async fn purge_unusable_rows_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
    file_exists: impl Fn(&str) -> bool,
) -> Result<FingerprintPurge, sqlx::Error> {
    let rows: Vec<(String, String, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT uuid, status, trim_in_ms, trim_out_ms, duration_ms, current_path
         FROM media_assets WHERE fingerprint = ?1",
    )
    .bind(fingerprint)
    .fetch_all(pool)
    .await?;

    let mut out = FingerprintPurge::default();

    for (uuid, status, trim_in, trim_out, duration, path) in rows {
        if is_subclip_row(trim_in, trim_out, duration) {
            out.protected += 1;
            continue;
        }
        match status.as_str() {
            "ready" => {
                if !path.is_empty() && file_exists(&path) {
                    out.protected += 1;
                } else {
                    sqlx::query("UPDATE media_assets SET status = 'error' WHERE uuid = ?1")
                        .bind(&uuid)
                        .execute(pool)
                        .await?;
                    out.demoted += 1;
                    tracing::warn!(
                        "Asset {} was ready but its mezzanine is missing; demoted to 'error' \
                         rather than deleted, so its metadata survives the re-ingest",
                        uuid
                    );
                }
            }
            "error" | "processing" => {
                sqlx::query("DELETE FROM media_assets WHERE uuid = ?1")
                    .bind(&uuid)
                    .execute(pool)
                    .await?;
                out.deleted += 1;
            }
            // Anything else (a status a later version introduces) is left
            // alone. Failing safe here costs a duplicate row; failing open
            // costs an operator's work.
            _ => out.protected += 1,
        }
    }

    Ok(out)
}

/// Is this row a trimmed excerpt rather than the whole file?
///
/// `trim_out_ms == 0` means "unset" in rows written before trimming existed,
/// and `trim_out_ms == duration_ms` is the full-length case `mark_ready`
/// writes. Anything else is a cut.
pub fn is_subclip_row(trim_in_ms: i64, trim_out_ms: i64, duration_ms: i64) -> bool {
    trim_in_ms > 0 || (trim_out_ms != 0 && trim_out_ms != duration_ms)
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
        let prefix = like_prefix(norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = ?1, 
                 original_virtual_folder = COALESCE(original_virtual_folder, virtual_folder)
             WHERE (virtual_folder = ?2 OR virtual_folder LIKE ?3 ESCAPE '\\') AND deleted_at IS NULL"
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
        // Callers must validate first (`post_restore_asset` returns 422);
        // keep the defensive fallback so a bad internal caller cannot write
        // an unvalidated folder string into the registry.
        if is_valid_virtual_folder(target) {
            target.to_string()
        } else {
            tracing::warn!("restore_asset called with invalid target folder; using '/'");
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
        let prefix = like_prefix(norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = NULL, 
                 virtual_folder = '/',
                 original_virtual_folder = NULL
             WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2 ESCAPE '\\') AND deleted_at IS NOT NULL"
        )
        .bind(norm)
        .bind(prefix)
        .execute(pool)
        .await?
    } else {
        let prefix = like_prefix(norm);
        sqlx::query(
            "UPDATE media_assets 
             SET deleted_at = NULL, 
                 virtual_folder = COALESCE(original_virtual_folder, '/'),
                 original_virtual_folder = NULL
             WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2 ESCAPE '\\') AND deleted_at IS NOT NULL"
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

    // Fail closed. Both the "inside target" and "not inside watch" checks used
    // to run only when *both* sides canonicalized, so an empty or temporarily
    // unreachable `target_folder` skipped them entirely and `remove_file`
    // proceeded on whatever `current_path` held — which for `processing` and
    // `error` rows is the source file in the watch folder (F-19).
    let Some(target) = managed_target_dir else {
        return Err("target directory not verified".to_string());
    };
    let Ok(can_target) = target.canonicalize() else {
        return Err("target directory could not be resolved".to_string());
    };
    let Ok(can_path) = path.canonicalize() else {
        return Err("path could not be resolved".to_string());
    };
    if !can_path.starts_with(&can_target) {
        return Err("Path is outside managed target directory root".to_string());
    }

    if let Some(watch) = watch_dir {
        let Ok(can_watch) = watch.canonicalize() else {
            return Err("watch directory could not be resolved".to_string());
        };
        if can_path.starts_with(&can_watch) {
            return Err("Refusing to purge source media in watch folder".to_string());
        }
    }

    Ok(can_path)
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
    // Only a `ready` row's `current_path` points at a published mezzanine.
    // For `processing` and `error` rows it is still the *source* file, so the
    // row goes but the file must not (F-19).
    let should_remove_file = if should_remove_file && a.status != "ready" {
        warnings.push(format!(
            "Physical file cleanup skipped: asset status is '{}', not 'ready'",
            a.status
        ));
        false
    } else {
        should_remove_file
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
        let prefix = like_prefix(norm);
        let sql = format!(
            "SELECT {} FROM media_assets WHERE (original_virtual_folder = ?1 OR original_virtual_folder LIKE ?2 ESCAPE '\\' OR virtual_folder = ?1 OR virtual_folder LIKE ?2 ESCAPE '\\') AND deleted_at IS NOT NULL",
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

/// Purge with an explicit managed target directory.
///
/// The old `purge_asset_with_mode`/`purge_asset_completely` wrappers passed
/// `None` for both directories, which is precisely the fail-open condition
/// F-19 describes. They are gone; every caller must name the target directory
/// it is willing to delete inside, exactly as the HTTP handler does.
pub async fn purge_asset_in_target(
    pool: &SqlitePool,
    uuid: &str,
    mode: PurgeMode,
    managed_target_dir: &Path,
) -> Result<PurgeOutcome, sqlx::Error> {
    let res =
        purge_single_asset_with_context(pool, uuid, mode, Some(managed_target_dir), None).await?;
    Ok(PurgeOutcome {
        rows_deleted: res.rows_deleted,
        file_removed: res.media_removed,
        sidecar_removed: res.sidecar_removed,
    })
}

pub const VALID_RATINGS: &[&str] = &["K", "8", "12", "16", "18"];

/// Age rating written for a freshly ingested asset.
///
/// Ingest has no way to know a programme's suitability mark, so it must not
/// assert one. `NONE` is the "unrated" token PlayOut already maps to its
/// `none` compliance rating; the operator sets the real mark in PlayOut,
/// which persists it through `PUT /api/assets/{uuid}/rating`. Subclips still
/// inherit their parent's rating, and nothing here rewrites existing rows.
pub const DEFAULT_RATING: &str = "NONE";

/// Upper bound on a rating payload, including the broadcast-metadata tail.
pub const MAX_RATING_LEN: usize = 4096;

pub fn is_valid_rating(rating: &str) -> bool {
    // The tail after the first `|` used to be an unbounded, unchecked payload
    // that was stored verbatim and handed back to PlayOut (F-08).
    if rating.len() > MAX_RATING_LEN {
        return false;
    }
    if rating.chars().any(|c| c.is_control()) {
        return false;
    }
    let (base, tail) = match rating.split_once('|') {
        Some((first, rest)) => (first, Some(rest)),
        None => (rating, None),
    };
    // A structured tail must be well-formed JSON; anything else is free text.
    if let Some(tail) = tail {
        let t = tail.trim();
        if (t.starts_with('[') || t.starts_with('{'))
            && serde_json::from_str::<serde_json::Value>(t).is_err()
        {
            return false;
        }
    }
    let trimmed = base.trim().trim_end_matches('+').to_ascii_uppercase();
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

/// Characters allowed in a virtual-folder segment on top of alphanumerics.
///
/// `_` is included (it is common in real folder names) and is neutralised by
/// [`like_prefix`]; `%` is not, because a folder whose name is a bare wildcard
/// is an operator trap rather than a useful name.
const VIRTUAL_FOLDER_PUNCT: &[char] = &[
    ' ', '_', '-', '.', '(', ')', '[', ']', '&', '+', ',', '\'', '!',
];

pub fn is_valid_virtual_folder(path: &str) -> bool {
    if path.is_empty() || !path.starts_with('/') {
        return false;
    }
    if path.len() > 512 {
        return false;
    }
    if path == "/" {
        return true;
    }
    if path.ends_with('/') {
        return false;
    }
    for segment in path[1..].split('/') {
        if segment.is_empty() {
            return false;
        }
        if segment != segment.trim() {
            return false;
        }
        if segment == "." || segment == ".." {
            return false;
        }
        for ch in segment.chars() {
            if ch.is_control() {
                return false;
            }
            // `%` is a LIKE wildcard with no legitimate use in a folder name;
            // reject the Windows separator too so a folder name can never be
            // mistaken for a filesystem path.
            if ch == '%' || ch == '\\' {
                return false;
            }
            if !(ch.is_alphanumeric() || VIRTUAL_FOLDER_PUNCT.contains(&ch)) {
                return false;
            }
        }
    }
    true
}

/// Build the `LIKE` pattern matching everything *under* `norm`.
///
/// Every folder query used `format!("{}/%", norm)` with no `ESCAPE` clause, so
/// `folder_path = "/%"` matched every asset in any sub-folder and a legitimate
/// folder named `/promo_2026` also matched `/promoX2026` (F-05). The returned
/// pattern must always be used with `ESCAPE '\'`.
pub fn like_prefix(norm: &str) -> String {
    let mut out = String::with_capacity(norm.len() + 4);
    for ch in norm.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push_str("/%");
    out
}

/// The default page size for `GET /api/assets` (T2-7).
///
/// Chosen to be larger than most stations' whole library, so the common case
/// is still a single request, while a library that has grown past it degrades
/// into paging rather than into a multi-hundred-megabyte response.
pub const ASSETS_DEFAULT_LIMIT: i64 = 1000;

/// The most rows one request may ask for, whatever `?limit=` says.
///
/// An unbounded `limit` is the same unbounded response F-06 is about, just
/// spelled by the caller instead of the server.
pub const ASSETS_MAX_LIMIT: i64 = 5000;

/// Clamp a caller-supplied page size into `1..=ASSETS_MAX_LIMIT`.
///
/// `None` is the default, not "no limit" — there is no way to ask for the whole
/// library in one response any more, deliberately.
pub fn clamp_asset_limit(requested: Option<i64>) -> i64 {
    match requested {
        None => ASSETS_DEFAULT_LIMIT,
        Some(n) if n < 1 => 1,
        Some(n) => n.min(ASSETS_MAX_LIMIT),
    }
}

/// One page of assets, plus how many there are in total.
pub struct AssetPage {
    pub assets: Vec<MediaAsset>,
    /// Matching rows ignoring `limit`/`offset` — served as `X-Total-Count`, so
    /// a client knows whether to ask for another page.
    pub total: i64,
}

/// A bounded page of live assets, ordered stably by uuid.
///
/// `find_all` used to fetch every row and let the handler serialise all of
/// them. With `keyframe_offsets` on each row that is tens of kilobytes per
/// asset, and a 5 000-asset library produced a response PlayOut's 16 MiB cap
/// rejected outright (F-06). `LIMIT`/`OFFSET` are applied in SQL, so the cost
/// is paid by the database, not by materialising the library in memory first.
pub async fn find_page(
    pool: &SqlitePool,
    status_filter: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<AssetPage, sqlx::Error> {
    let limit = clamp_asset_limit(Some(limit));
    let offset = offset.max(0);

    // ORDER BY uuid, not rowid: paging has to be stable across requests, and a
    // concurrent ingest must not shuffle a row the client has already seen onto
    // the next page.
    let (where_clause, bind_status) = match status_filter {
        Some(_) => ("status = ?1 AND deleted_at IS NULL", true),
        None => ("deleted_at IS NULL", false),
    };

    let count_sql = format!("SELECT COUNT(*) FROM media_assets WHERE {}", where_clause);
    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql);
    if bind_status {
        count_q = count_q.bind(status_filter.unwrap_or_default());
    }
    let (total,) = count_q.fetch_one(pool).await?;

    let sql = format!(
        "SELECT {} FROM media_assets WHERE {} ORDER BY uuid LIMIT {} OFFSET {}",
        SELECT_COLS, where_clause, limit, offset
    );
    let mut q = sqlx::query_as::<_, MediaAsset>(&sql);
    if bind_status {
        q = q.bind(status_filter.unwrap_or_default());
    }
    let assets = q.fetch_all(pool).await?;

    Ok(AssetPage { assets, total })
}

/// Every live asset, unbounded.
///
/// Retained for internal callers that genuinely need the whole set (recovery
/// sweeps, folder reconciliation). **Not** reachable from the HTTP surface —
/// `GET /api/assets` goes through [`find_page`].
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

/// Upsert one job row through any executor -- the pool, or a transaction.
///
/// Executor-generic so `persist_jobs` can run a whole coalesced batch inside a
/// single transaction (T2-4) without a second copy of this 31-column statement.
async fn upsert_job<'e, E>(executor: E, job: &crate::jobs::JobRecord) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
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
    .execute(executor)
    .await?;

    Ok(())
}

/// Persist a single job immediately. Used by startup population and tests; the
/// running service goes through the coalescing persister and `persist_jobs`.
pub async fn insert_durable_job(
    pool: &SqlitePool,
    job: &crate::jobs::JobRecord,
) -> Result<(), sqlx::Error> {
    upsert_job(pool, job).await
}

/// Persist a coalesced batch of jobs in one transaction.
///
/// Before T2-4 every in-memory job mutation spawned its own independent upsert,
/// so two rapid writes for the same job could land out of order and leave the
/// database claiming "Encoding 97%" for a job that had already completed. The
/// persister hands this the newest record per job id, and one transaction makes
/// the batch atomic and cheap: an encode used to cause one commit per FFmpeg
/// progress line.
pub async fn persist_jobs(
    pool: &SqlitePool,
    jobs: &[crate::jobs::JobRecord],
) -> Result<(), sqlx::Error> {
    if jobs.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for job in jobs {
        upsert_job(&mut *tx, job).await?;
    }
    tx.commit().await
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

/// Jobs still waiting to be picked up, oldest first.
pub async fn load_pending_jobs(
    pool: &SqlitePool,
) -> Result<Vec<crate::jobs::JobRecord>, sqlx::Error> {
    let rows: Vec<DurableJobRow> = sqlx::query_as(
        "SELECT * FROM transcode_jobs WHERE state = 'Pending' ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into_job_record()).collect())
}

/// Mark the given jobs failed because their source file is gone.
///
/// `recover_stale_jobs` re-queues interrupted work to `Pending`, but nothing
/// ever consumed those rows: only the filesystem watcher feeds the dispatcher,
/// so a job whose source had since been moved or deleted stayed `Pending`
/// forever and showed up in `/api/jobs` and `/api/stats` as permanently
/// outstanding work (F-13). Failing them at startup makes the queue reflect
/// reality; a job whose source *does* still exist is left alone, because the
/// watcher will re-offer the file and the dispatcher now adopts the existing
/// record rather than creating a second one.
pub async fn fail_jobs_with_missing_source(
    pool: &SqlitePool,
    job_ids: &[String],
) -> Result<usize, sqlx::Error> {
    if job_ids.is_empty() {
        return Ok(0);
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let mut affected = 0usize;
    for id in job_ids {
        let res = sqlx::query(
            "UPDATE transcode_jobs SET
                state = 'Failed',
                phase = 'failed',
                current_stage = 'Failed',
                error = 'Source file no longer exists',
                error_category = 'source_missing_on_recovery',
                worker_id = NULL,
                leased_until = NULL,
                finished_at = ?1
             WHERE id = ?2 AND state = 'Pending'",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        affected += res.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(affected)
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
        assert!(is_valid_rating("NONE"));
        assert!(is_valid_rating(""));
        assert!(is_valid_rating("16|NONE|NONE|[{\"start\":0,\"text\":\"ΠΕΡΙΕΧΕΙ ΣΚΗΝΕΣ ΒΙΑΣ\"}]"));
        assert!(is_valid_rating("12|TP|SHOW|[]"));
        assert!(!is_valid_rating("21"));
        assert!(!is_valid_rating("21|NONE|NONE|[]"));
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

    /// SB-09: the pragmas moved onto the connect options so every pooled
    /// connection gets them, and `synchronous` stays at FULL -- NORMAL under
    /// WAL can lose the last commits on power loss, which would leave a
    /// published mezzanine with no `ready` row (the F-18 orphan class).
    #[tokio::test]
    async fn every_pooled_connection_gets_the_pragmas_and_synchronous_stays_full() {
        let (pool, _temp_dir) = setup_test_pool().await;

        // More round trips than the pool has connections, so at least one
        // answer comes from a connection other than the first.
        for _ in 0..12 {
            let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(journal.to_ascii_lowercase(), "wal");

            let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(synchronous, 2, "synchronous must stay FULL");

            let cache_size: i64 = sqlx::query_scalar("PRAGMA cache_size")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(cache_size, -16384);

            let temp_store: i64 = sqlx::query_scalar("PRAGMA temp_store")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(temp_store, 2, "temp_store MEMORY");
        }
    }

    /// A freshly ingested asset must not claim a suitability mark. PlayOut is
    /// where an operator sets it; ingest only records "unrated".
    #[tokio::test]
    async fn a_new_ingest_has_no_age_rating() {
        let (pool, _temp_dir) = setup_test_pool().await;
        insert_processing(&pool, "fresh-1", 1, None, "C:/x/a.mp4", "a")
            .await
            .unwrap();
        let asset = find_by_uuid(&pool, "fresh-1").await.unwrap().unwrap();
        assert_eq!(asset.rating, DEFAULT_RATING);
        assert_ne!(asset.rating, "K");
        assert!(is_valid_rating(&asset.rating));

        // An operator-set rating still sticks, and a subclip still inherits it.
        assert!(set_rating(&pool, "fresh-1", "12").await.unwrap());
        let asset = find_by_uuid(&pool, "fresh-1").await.unwrap().unwrap();
        assert_eq!(asset.rating, "12");
    }

    #[tokio::test]
    async fn test_purge_parent_with_no_subclips() {
        let (pool, temp_dir) = setup_test_pool().await;
        let video_path = temp_dir.join("video1.mp4");
        let sidecar_path = crate::identity::sidecar_path_for(&video_path);

        std::fs::create_dir_all(sidecar_path.parent().unwrap()).unwrap();
        std::fs::File::create(&video_path).unwrap();
        std::fs::File::create(&sidecar_path).unwrap();

        let uuid = "parent-1";
        insert_processing(&pool, uuid, 12345, None, &video_path.to_string_lossy(), "video1")
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

        let outcome = purge_asset_in_target(&pool, uuid, PurgeMode::PreserveReferencedMezzanine, &temp_dir)
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

        std::fs::create_dir_all(sidecar_path.parent().unwrap()).unwrap();
        std::fs::File::create(&video_path).unwrap();
        std::fs::File::create(&sidecar_path).unwrap();

        let parent_uuid = "parent-shared";
        let subclip_uuid = "subclip-shared";

        insert_processing(
            &pool,
            parent_uuid,
            67890,
        None,
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
            purge_asset_in_target(&pool, parent_uuid, PurgeMode::PreserveReferencedMezzanine, &temp_dir)
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
            purge_asset_in_target(&pool, subclip_uuid, PurgeMode::PreserveReferencedMezzanine, &temp_dir)
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
        None,
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

        let out = purge_asset_in_target(&pool, sub1, PurgeMode::PreserveReferencedMezzanine, &temp_dir)
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
        insert_processing(&pool, uuid, 99999, None, &staging_path.to_string_lossy(), "video")
            .await
            .unwrap();

        let out = purge_asset_in_target(&pool, uuid, PurgeMode::PreserveReferencedMezzanine, &temp_dir)
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
    async fn recovery_fails_pending_jobs_whose_source_is_gone() {
        let (pool, temp_dir) = setup_test_pool().await;
        let present_path = temp_dir.join("still-here.mov");
        std::fs::write(&present_path, b"media").expect("write fixture");

        // Pending, source still on disk: left alone, because the watcher will
        // re-offer it and the dispatcher adopts this record.
        let mut keep = crate::jobs::JobRecord::new(&present_path.to_string_lossy(), "ProfileA");
        keep.state = crate::jobs::JobState::Pending;
        keep.phase = crate::jobs::JobPhase::Queued;
        insert_durable_job(&pool, &keep).await.unwrap();

        // Pending, source gone: would sit Pending forever (F-13).
        let mut orphan = crate::jobs::JobRecord::new("D:/media/deleted-while-down.mov", "ProfileA");
        orphan.state = crate::jobs::JobState::Pending;
        orphan.phase = crate::jobs::JobPhase::Queued;
        insert_durable_job(&pool, &orphan).await.unwrap();

        // Completed, source gone: must not be touched.
        let mut done = crate::jobs::JobRecord::new("D:/media/already-done.mov", "ProfileA");
        done.state = crate::jobs::JobState::Completed;
        done.phase = crate::jobs::JobPhase::Completed;
        insert_durable_job(&pool, &done).await.unwrap();

        let pending = load_pending_jobs(&pool).await.unwrap();
        assert_eq!(pending.len(), 2, "only Pending rows are considered");

        let missing: Vec<String> = pending
            .iter()
            .filter(|j| !std::path::Path::new(&j.input_path).exists())
            .map(|j| j.id.clone())
            .collect();
        assert_eq!(missing, vec![orphan.id.clone()]);

        let n = fail_jobs_with_missing_source(&pool, &missing).await.unwrap();
        assert_eq!(n, 1);

        let all = load_all_durable_jobs(&pool).await.unwrap();
        let o = all.iter().find(|j| j.id == orphan.id).unwrap();
        assert_eq!(o.state, crate::jobs::JobState::Failed);
        assert_eq!(o.phase, crate::jobs::JobPhase::Failed);
        assert_eq!(
            o.error_category.as_deref(),
            Some("source_missing_on_recovery")
        );
        assert!(o.finished_at.is_some());

        let k = all.iter().find(|j| j.id == keep.id).unwrap();
        assert_eq!(k.state, crate::jobs::JobState::Pending);
        let d = all.iter().find(|j| j.id == done.id).unwrap();
        assert_eq!(d.state, crate::jobs::JobState::Completed);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn persist_jobs_writes_a_whole_batch_in_one_transaction() {
        let (pool, temp_dir) = setup_test_pool().await;
        let batch: Vec<crate::jobs::JobRecord> = (0..25)
            .map(|i| crate::jobs::JobRecord::new(&format!("D:/media/clip{}.mov", i), "ProfileA"))
            .collect();

        persist_jobs(&pool, &batch).await.unwrap();
        assert_eq!(load_all_durable_jobs(&pool).await.unwrap().len(), 25);

        // Re-persisting the same ids updates rather than duplicating.
        let mut again = batch.clone();
        for j in &mut again {
            j.progress = 100.0;
        }
        persist_jobs(&pool, &again).await.unwrap();
        let rows = load_all_durable_jobs(&pool).await.unwrap();
        assert_eq!(rows.len(), 25);
        assert!(rows.iter().all(|r| r.progress == 100.0));

        // An empty batch is a no-op, not an empty transaction.
        persist_jobs(&pool, &[]).await.unwrap();

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
    async fn test_request_hash_dedup() {
        let (pool, temp_dir) = setup_test_pool().await;
        let mut job = crate::jobs::JobRecord::new("D:/media/clip.mp4", "ProfileA");
        job.request_hash = Some("hash-abc-123".into());
        insert_durable_job(&pool, &job).await.unwrap();

        let found = find_active_by_request_hash(&pool, "hash-abc-123")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, job.id);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_soft_delete_single_asset_and_restore() {
        let (pool, temp_dir) = setup_test_pool().await;
        let uuid = "test-soft-del-1";
        insert_processing(&pool, uuid, 12345, None, "D:/target/clip1.mp4", "Clip 1")
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
        insert_processing(&pool, "u1", 1, None, "D:/target/c1.mp4", "C1").await.unwrap();
        set_virtual_folder(&pool, "u1", "/Shows/Drama").await.unwrap();

        insert_processing(&pool, "u2", 2, None, "D:/target/c2.mp4", "C2").await.unwrap();
        set_virtual_folder(&pool, "u2", "/Shows/Drama/Season1").await.unwrap();

        insert_processing(&pool, "u3", 3, None, "D:/target/c3.mp4", "C3").await.unwrap();
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
    async fn test_folder_ops_escape_like_wildcards() {
        let (pool, temp_dir) = setup_test_pool().await;

        // Legacy rows can carry LIKE metacharacters even though the validator
        // now rejects `%` at the API boundary, so the SQL must escape them.
        insert_processing(&pool, "u1", 1, None, "D:/target/c1.mp4", "C1").await.unwrap();
        set_virtual_folder(&pool, "u1", "/promo_2026").await.unwrap();
        insert_processing(&pool, "u2", 2, None, "D:/target/c2.mp4", "C2").await.unwrap();
        set_virtual_folder(&pool, "u2", "/promoX2026").await.unwrap();
        insert_processing(&pool, "u3", 3, None, "D:/target/c3.mp4", "C3").await.unwrap();
        set_virtual_folder(&pool, "u3", "/promo_2026/teasers").await.unwrap();
        insert_processing(&pool, "u4", 4, None, "D:/target/c4.mp4", "C4").await.unwrap();
        set_virtual_folder(&pool, "u4", "/a%b").await.unwrap();
        insert_processing(&pool, "u5", 5, None, "D:/target/c5.mp4", "C5").await.unwrap();
        set_virtual_folder(&pool, "u5", "/a%b/c").await.unwrap();
        insert_processing(&pool, "u6", 6, None, "D:/target/c6.mp4", "C6").await.unwrap();
        set_virtual_folder(&pool, "u6", "/aQb").await.unwrap();

        // `_` must not act as a single-character wildcard.
        let affected = trash_folder(&pool, "/promo_2026").await.unwrap();
        assert_eq!(affected, 2, "/promoX2026 must not be trashed");
        assert!(find_by_uuid(&pool, "u2").await.unwrap().is_some());

        // `%` must not act as a multi-character wildcard.
        let affected = trash_folder(&pool, "/a%b").await.unwrap();
        assert_eq!(affected, 2, "/aQb must not be trashed");
        assert!(find_by_uuid(&pool, "u6").await.unwrap().is_some());

        let restored = restore_folder(&pool, "/a%b", false).await.unwrap();
        assert_eq!(restored, 2);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_like_prefix_escapes_metacharacters() {
        assert_eq!(like_prefix("/promo_2026"), "/promo\\_2026/%");
        assert_eq!(like_prefix("/a%b"), "/a\\%b/%");
        assert_eq!(like_prefix("/plain"), "/plain/%");
    }

    #[test]
    fn test_is_valid_virtual_folder_rules() {
        assert!(is_valid_virtual_folder("/"));
        assert!(is_valid_virtual_folder("/Shows"));
        assert!(is_valid_virtual_folder("/Shows/Season 1"));
        assert!(is_valid_virtual_folder("/promo_2026"));
        assert!(is_valid_virtual_folder("/Ειδήσεις"));
        assert!(is_valid_virtual_folder("/Spots (2026)/A&B [HD]"));

        // F-05: `/%` matched every asset in any sub-folder.
        assert!(!is_valid_virtual_folder("/%"));
        assert!(!is_valid_virtual_folder("/a%b"));
        assert!(!is_valid_virtual_folder(""));
        assert!(!is_valid_virtual_folder("Shows"));
        assert!(!is_valid_virtual_folder("/Shows/"));
        assert!(!is_valid_virtual_folder("/Shows//S1"));
        assert!(!is_valid_virtual_folder("/.."));
        assert!(!is_valid_virtual_folder("/a/../b"));
        assert!(!is_valid_virtual_folder("/."));
        assert!(!is_valid_virtual_folder("/ leading"));
        assert!(!is_valid_virtual_folder("/trailing "));
        assert!(!is_valid_virtual_folder("/bell\u{7}"));
        assert!(!is_valid_virtual_folder("/back\\slash"));
        assert!(!is_valid_virtual_folder(&format!("/{}", "x".repeat(600))));
    }

    #[tokio::test]
    async fn test_restore_folder_fallback_to_root() {
        let (pool, temp_dir) = setup_test_pool().await;
        insert_processing(&pool, "u10", 10, None, "D:/target/c10.mp4", "C10").await.unwrap();
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
    async fn test_purge_is_fail_closed_without_a_verified_target() {
        let (_pool, temp_dir) = setup_test_pool().await;
        let media_file = temp_dir.join("mezzanine.mp4");
        std::fs::File::create(&media_file).unwrap();

        // F-19: with no managed target directory the old code skipped both
        // location checks and deleted whatever `current_path` held.
        assert!(validate_purge_path(&media_file.to_string_lossy(), None, None).is_err());

        // An unresolvable target directory must also fail closed, not fall
        // through — a target folder on a temporarily unreachable share is the
        // realistic version of this.
        assert!(validate_purge_path(
            &media_file.to_string_lossy(),
            Some(&temp_dir.join("not-mounted")),
            None,
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_purge_of_error_row_keeps_the_source_file() {
        let (pool, temp_dir) = setup_test_pool().await;
        // For a `processing`/`error` row, `current_path` is still the SOURCE
        // file in the watch folder. The row must go; the file must not.
        let source = temp_dir.join("source_clip.mp4");
        std::fs::File::create(&source).unwrap();

        let uuid = "error-row-uuid";
        insert_processing(&pool, uuid, 4242, None, &source.to_string_lossy(), "Source")
            .await
            .unwrap();
        mark_error(&pool, uuid).await.unwrap();

        let result = purge_single_asset_with_context(
            &pool,
            uuid,
            PurgeMode::PreserveReferencedMezzanine,
            Some(&temp_dir),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.rows_deleted, 1, "the row is still removed");
        assert!(!result.media_removed, "the source file must be retained");
        assert!(
            source.exists(),
            "purging a failed asset must never delete its source media"
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("not 'ready'")),
            "the operator must be told why cleanup was skipped: {:?}",
            result.warnings
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_purge_single_asset_with_real_files_and_sidecar() {
        let (pool, temp_dir) = setup_test_pool().await;
        let media_file = temp_dir.join("mezzanine_video.mp4");
        let sidecar_file = crate::identity::sidecar_path_for(&media_file);
        // Since T3-5 the resolver always answers `<root>/sidecars/...`.
        std::fs::create_dir_all(sidecar_file.parent().unwrap()).unwrap();
        std::fs::File::create(&media_file).unwrap();
        std::fs::File::create(&sidecar_file).unwrap();

        let uuid = "purge-test-uuid";
        insert_processing(&pool, uuid, 8888, None, &media_file.to_string_lossy(), "Mezzanine")
            .await
            .unwrap();
        // Only a `ready` row's current_path points at a published mezzanine;
        // T1-7 refuses to delete files for any other status.
        mark_ready(
            &pool,
            uuid,
            &media_file.to_string_lossy(),
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
        insert_processing(&pool, "old-asset", 111, None, "D:/target/old.mp4", "Old").await.unwrap();
        sqlx::query("UPDATE media_assets SET deleted_at = ?1 WHERE uuid = 'old-asset'")
            .bind(&old_time)
            .execute(&pool)
            .await
            .unwrap();

        // Recent asset deleted 2 days ago
        let recent_time = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        insert_processing(&pool, "recent-asset", 222, None, "D:/target/recent.mp4", "Recent").await.unwrap();
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
        insert_processing(&pool, "asset-master", 1001, None, "D:/target/master.mp4", "Master 1").await.unwrap();
        mark_ready(&pool, "asset-master", "D:/target/master.mp4", 10000, true, 25.0, 25, 1, 250, 50, 0, &["warning1".into()], "[]").await.unwrap();

        // 2. Insert subclip
        create_subclip(&pool, "asset-sub", "asset-master", "Subclip 1", 1000, 5000, true, "[]").await.unwrap();

        // 3. Insert trashed asset
        insert_processing(&pool, "asset-trashed", 1002, None, "D:/target/trashed.mp4", "Trashed 1").await.unwrap();
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
    // ---- T2-6: re-ingest must not destroy operator work (F-26) ----

    /// Give `uuid` a `ready` row with an explicit trim window.
    ///
    /// `trim_out == duration` is the full-length case `mark_ready` writes;
    /// anything narrower is a subclip an operator cut by hand.
    async fn ready_row(
        pool: &SqlitePool,
        uuid: &str,
        fingerprint: i64,
        path: &str,
        duration_ms: i64,
        trim_in_ms: i64,
        trim_out_ms: i64,
    ) {
        insert_processing(pool, uuid, fingerprint, None, path, uuid)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE media_assets SET status = 'ready', mezzanine_ok = 1, current_path = ?1,
             duration_ms = ?2, trim_in_ms = ?3, trim_out_ms = ?4 WHERE uuid = ?5",
        )
        .bind(path)
        .bind(duration_ms)
        .bind(trim_in_ms)
        .bind(trim_out_ms)
        .bind(uuid)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn a_trim_window_identifies_a_subclip() {
        // Full length, two spellings: trim_out unset (legacy rows) and
        // trim_out == duration (what mark_ready writes).
        assert!(!is_subclip_row(0, 0, 10_000));
        assert!(!is_subclip_row(0, 10_000, 10_000));
        // Cuts.
        assert!(is_subclip_row(500, 10_000, 10_000));
        assert!(is_subclip_row(0, 4_000, 10_000));
        assert!(is_subclip_row(1_000, 4_000, 10_000));
    }

    #[tokio::test]
    async fn re_ingesting_a_parent_does_not_delete_its_subclips() {
        let (pool, dir) = setup_test_pool().await;
        let parent_file = dir.join("parent.mp4");

        // The parent's mezzanine has gone missing -- the case that sends the
        // processor down the purge path in the first place.
        let fp = 777_001;
        ready_row(
            &pool,
            "parent",
            fp,
            &parent_file.to_string_lossy(),
            60_000,
            0,
            60_000,
        )
        .await;
        // Subclips carry the PARENT's fingerprint. That is what made the old
        // blanket DELETE destroy them.
        ready_row(&pool, "clip-a", fp, "D:/target/clip-a.mp4", 60_000, 1_000, 5_000).await;
        ready_row(&pool, "clip-b", fp, "D:/target/clip-b.mp4", 60_000, 0, 9_000).await;
        // And a genuinely dead leftover, which should go.
        insert_processing(&pool, "dead", fp, None, "D:/watch/x.mxf", "dead")
            .await
            .unwrap();
        sqlx::query("UPDATE media_assets SET status = 'error' WHERE uuid = 'dead'")
            .execute(&pool)
            .await
            .unwrap();

        let outcome = purge_unusable_rows_by_fingerprint(&pool, fp, |p| {
            std::path::Path::new(p).exists()
        })
        .await
        .unwrap();

        assert_eq!(outcome.deleted, 1, "only the failed-ingest row is deleted");
        assert_eq!(
            outcome.demoted, 1,
            "the parent was ready but its file is gone -- demote, do not delete"
        );
        assert_eq!(outcome.protected, 2, "both subclips survive");

        let survivors: Vec<(String, String)> =
            sqlx::query_as("SELECT uuid, status FROM media_assets ORDER BY uuid")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<&str> = survivors.iter().map(|(u, _)| u.as_str()).collect();
        assert_eq!(names, vec!["clip-a", "clip-b", "parent"]);

        // The parent kept its row -- and therefore its rating, virtual folder
        // and compliance metadata -- it just is not ready any more.
        let parent_status = &survivors.iter().find(|(u, _)| u == "parent").unwrap().1;
        assert_eq!(parent_status, "error");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_ready_asset_whose_file_still_exists_is_never_purged() {
        let (pool, dir) = setup_test_pool().await;
        let file = dir.join("live.mp4");
        std::fs::File::create(&file).unwrap();

        let fp = 777_002;
        ready_row(&pool, "live", fp, &file.to_string_lossy(), 1_000, 0, 1_000).await;

        let outcome = purge_unusable_rows_by_fingerprint(&pool, fp, |p| {
            std::path::Path::new(p).exists()
        })
        .await
        .unwrap();

        assert_eq!(outcome, FingerprintPurge { deleted: 0, demoted: 0, protected: 1 });
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM media_assets")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_source_hash_round_trips_and_is_null_for_legacy_rows() {
        let (pool, dir) = setup_test_pool().await;

        insert_processing(&pool, "hashed", 1, Some("deadbeef"), "D:/w/a.mxf", "A")
            .await
            .unwrap();
        insert_processing(&pool, "legacy", 2, None, "D:/w/b.mxf", "B")
            .await
            .unwrap();

        let a = find_by_fingerprint(&pool, 1).await.unwrap().unwrap();
        let b = find_by_fingerprint(&pool, 2).await.unwrap().unwrap();
        assert_eq!(a.source_sha256.as_deref(), Some("deadbeef"));
        assert_eq!(
            b.source_sha256, None,
            "a row with no stored hash must read back as None, which is what \
             makes dedup refuse to confirm rather than guess"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

}


#[cfg(test)]
mod rating_tests {
    use super::*;

    #[test]
    fn rating_payload_is_bounded() {
        assert!(is_valid_rating("K"));
        assert!(is_valid_rating("12+"));
        assert!(is_valid_rating(""));
        assert!(is_valid_rating("NONE"));

        // Broadcast metadata tail: free text is fine, and a tail that claims
        // to be JSON must actually parse.
        assert!(is_valid_rating("K|some free text"));
        assert!(is_valid_rating(r#"K|["a","b"]"#));
        assert!(is_valid_rating(r#"K|{"violence":true}"#));
        assert!(!is_valid_rating(r#"K|[{"broken": }"#));
        assert!(!is_valid_rating("K|{unclosed"));

        // The tail used to be unbounded (F-08).
        assert!(is_valid_rating(&format!("K|{}", "x".repeat(MAX_RATING_LEN - 2))));
        assert!(!is_valid_rating(&format!("K|{}", "x".repeat(MAX_RATING_LEN))));

        assert!(!is_valid_rating("K|line\nbreak"));
        assert!(!is_valid_rating("NOT-A-RATING"));
    }


}
