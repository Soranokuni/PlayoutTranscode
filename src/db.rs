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
}
