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
    pub status: String,
    pub display_name: String,
    pub virtual_folder: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetResponse {
    pub uuid: String,
    pub current_path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub rating: String,
    pub status: String,
    pub display_name: String,
    pub virtual_folder: String,
}

impl From<MediaAsset> for AssetResponse {
    fn from(a: MediaAsset) -> Self {
        Self {
            uuid: a.uuid,
            current_path: a.current_path,
            duration_ms: a.duration_ms,
            trim_in_ms: a.trim_in_ms,
            trim_out_ms: a.trim_out_ms,
            rating: a.rating,
            status: a.status,
            display_name: a.display_name,
            virtual_folder: a.virtual_folder,
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
            status       TEXT NOT NULL DEFAULT 'processing',
            display_name TEXT NOT NULL DEFAULT '',
            virtual_folder TEXT NOT NULL DEFAULT '/'
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
        "ALTER TABLE media_assets ADD COLUMN virtual_folder TEXT NOT NULL DEFAULT '/'",
    )
    .execute(&pool)
    .await
    {
        tracing::debug!("virtual_folder column may already exist: {}", e);
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
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE media_assets SET current_path = ?1, duration_ms = ?2, status = 'ready' WHERE uuid = ?3",
    )
    .bind(output_path)
    .bind(duration_ms)
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
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets WHERE uuid = ?1",
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
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets WHERE fingerprint = ?1",
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
    let result = sqlx::query("UPDATE media_assets SET rating = ?1 WHERE uuid = ?2")
        .bind(rating)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub const VALID_RATINGS: &[&str] = &["K", "8", "12", "16", "18"];

pub fn is_valid_rating(_rating: &str) -> bool {
    true
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
            "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets WHERE status = ?1 ORDER BY uuid",
        )
        .bind(status)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, MediaAsset>(
            "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets ORDER BY uuid",
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
        "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets WHERE uuid IN ({})",
        placeholders
    );
    let mut query = sqlx::query_as::<_, MediaAsset>(&sql);
    for uuid in uuids {
        query = query.bind(uuid);
    }
    query.fetch_all(pool).await
}
