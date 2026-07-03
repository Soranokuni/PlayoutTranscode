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
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: String,
    pub keyframe_offsets_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetResponse {
    pub uuid: String,
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
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: Vec<String>,
    pub keyframe_offsets: Vec<i64>,
}

impl From<MediaAsset> for AssetResponse {
    fn from(a: MediaAsset) -> Self {
        let warnings: Vec<String> = serde_json::from_str(&a.warnings).unwrap_or_default();
        let keyframe_offsets: Vec<i64> = serde_json::from_str(&a.keyframe_offsets_json).unwrap_or_default();
        Self {
            uuid: a.uuid,
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
            total_frames: a.total_frames,
            gop_frames: a.gop_frames,
            keyframe_safe_start_ms: a.keyframe_safe_start_ms,
            warnings,
            keyframe_offsets,
        }
    }
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

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN display_name TEXT NOT NULL DEFAULT ''",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("display_name column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN rating TEXT NOT NULL DEFAULT 'K'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("rating column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN tp TEXT NOT NULL DEFAULT 'None'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("tp column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN virtual_folder TEXT NOT NULL DEFAULT '/'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("virtual_folder column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN mezzanine_ok BOOLEAN NOT NULL DEFAULT 0",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("mezzanine_ok column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN fps REAL NOT NULL DEFAULT 0.0",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("fps column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN total_frames INTEGER NOT NULL DEFAULT 0",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("total_frames column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN gop_frames INTEGER NOT NULL DEFAULT 0",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("gop_frames column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN keyframe_safe_start_ms INTEGER NOT NULL DEFAULT 0",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("keyframe_safe_start_ms column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN warnings TEXT NOT NULL DEFAULT '[]'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("warnings column may already exist: {}", e);
    }

    if let Err(e) = sqlx::query(
        "ALTER TABLE media_assets ADD COLUMN keyframe_offsets_json TEXT NOT NULL DEFAULT '[]'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("keyframe_offsets_json column may already exist: {}", e);
    }

    {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT uuid, current_path FROM media_assets WHERE display_name = ''",
        )
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
                let _ = sqlx::query(
                    "UPDATE media_assets SET display_name = ?1 WHERE uuid = ?2",
                )
                .bind(&display_name)
                .bind(uuid)
                .execute(&pool)
                .await;
            }
        }
    }

    let result = sqlx::query(
        "UPDATE media_assets SET status = 'error' WHERE status = 'processing'",
    )
    .execute(&pool)
    .await?;
    if result.rows_affected() > 0 {
        tracing::warn!(
            "Recovered {} orphaned asset row(s) left in 'processing' state (marked 'error')",
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
            status = 'ready',
            mezzanine_ok = ?3,
            fps = ?4,
            total_frames = ?5,
            gop_frames = ?6,
            keyframe_safe_start_ms = ?7,
            warnings = ?8,
            keyframe_offsets_json = ?9
         WHERE uuid = ?10",
    )
    .bind(output_path)
    .bind(duration_ms)
    .bind(mezzanine_ok)
    .bind(fps)
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

pub async fn update_path_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
    new_path: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE media_assets SET current_path = ?1, status = 'ready' WHERE fingerprint = ?2",
    )
    .bind(new_path)
    .bind(fingerprint)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn find_by_uuid(
    pool: &SqlitePool,
    uuid: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    sqlx::query_as::<_, MediaAsset>(
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json FROM media_assets WHERE uuid = ?1",
    )
    .bind(uuid)
    .fetch_optional(pool)
    .await
}

pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    sqlx::query_as::<_, MediaAsset>(
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json FROM media_assets WHERE fingerprint = ?1",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
}

pub async fn set_trim(
    pool: &SqlitePool,
    uuid: &str,
    trim_in_ms: i64,
    trim_out_ms: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE media_assets SET trim_in_ms = ?1, trim_out_ms = ?2 WHERE uuid = ?3",
    )
    .bind(trim_in_ms)
    .bind(trim_out_ms)
    .bind(uuid)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_rating(
    pool: &SqlitePool,
    uuid: &str,
    rating: &str,
) -> Result<bool, sqlx::Error> {
    let asset = find_by_uuid(pool, uuid).await?;
    if let Some(a) = asset {
        let result = sqlx::query("UPDATE media_assets SET rating = ?1 WHERE fingerprint = ?2")
            .bind(rating)
            .bind(a.fingerprint)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() > 0)
    } else {
        Ok(false)
    }
}

pub async fn set_tp(
    pool: &SqlitePool,
    uuid: &str,
    tp: &str,
) -> Result<bool, sqlx::Error> {
    let asset = find_by_uuid(pool, uuid).await?;
    if let Some(a) = asset {
        let result = sqlx::query("UPDATE media_assets SET tp = ?1 WHERE fingerprint = ?2")
            .bind(tp)
            .bind(a.fingerprint)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() > 0)
    } else {
        Ok(false)
    }
}

pub async fn create_subclip(
    pool: &SqlitePool,
    new_uuid: &str,
    parent_uuid: &str,
    display_name: &str,
    trim_in_ms: i64,
    trim_out_ms: i64,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let parent = find_by_uuid(pool, parent_uuid).await?;
    if let Some(p) = parent {
        sqlx::query(
            "INSERT INTO media_assets (uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
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
        .bind(p.mezzanine_ok)
        .bind(p.fps)
        .bind(p.total_frames)
        .bind(p.gop_frames)
        .bind(p.keyframe_safe_start_ms)
        .bind(&p.warnings)
        .bind(&p.keyframe_offsets_json)
        .execute(pool)
        .await?;
        
        find_by_uuid(pool, new_uuid).await
    } else {
        Ok(None)
    }
}

pub async fn purge_asset_by_path_or_fingerprint(
    pool: &SqlitePool,
    current_path: &str,
    fingerprint: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM media_assets WHERE current_path = ?1 OR fingerprint = ?2")
        .bind(current_path)
        .bind(fingerprint)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
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
    if let Some(status) = status_filter {
        sqlx::query_as::<_, MediaAsset>(
            "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json FROM media_assets WHERE status = ?1 ORDER BY uuid",
        )
        .bind(status)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, MediaAsset>(
            "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json FROM media_assets ORDER BY uuid",
        )
        .fetch_all(pool)
        .await
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
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json FROM media_assets WHERE uuid IN ({})",
        placeholders
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

