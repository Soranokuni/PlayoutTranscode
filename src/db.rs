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
    /// Identifies the QC verdict on this row: which bytes were judged, and
    /// under which encoding and validation settings. `None` on a row published
    /// before the column existed, and on any row that has not been through QC.
    ///
    /// This is what makes a failure re-checkable rather than merely repeated.
    /// See [`find_reproducible_qc_failure`].
    pub qc_verdict_key: Option<String>,
    /// The full-length asset this row was cut from, for a sub-clip; `None` for
    /// a full-length row (T-2b).
    ///
    /// Before this column, a sub-clip was identified by its *trim window*, and
    /// nothing at all distinguished it in the dedupe lookup: it carries its
    /// parent's `fingerprint` and its parent's `current_path`, with a NULL
    /// `source_sha256`, so `find_by_fingerprint` could hand back a sub-clip as
    /// the candidate duplicate of its own parent's source and the whole
    /// programme was re-ingested.
    pub parent_uuid: Option<String>,
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
    /// The service has recorded that this exact media, under these exact
    /// settings, already failed, and will skip it rather than encode it again.
    /// Additive; older clients ignore it.
    pub retry_suppressed: bool,
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
            retry_suppressed: is_retry_suppressed(a.qc_verdict_key.as_deref()),
            deleted_at: a.deleted_at,
            original_virtual_folder: a.original_virtual_folder,
        }
    }
}

const SELECT_COLS: &str = "uuid, fingerprint, source_sha256, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json, deleted_at, original_virtual_folder, parent_uuid, qc_verdict_key";

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
    /// Rows left alone because the same media failed permanently under the same
    /// settings. Retrying them would burn an encode to reach a known answer.
    pub kept_permanent: usize,
}

/// Reclaim `error`/`processing` rows whose `current_path` (= source path on those states)
/// still lives inside the watch folder, so the watcher will re-queue them. The remaining
/// dead rows are kept (their source file is no longer reachable). Returns counts for logging.
pub async fn recover_failed_assets(
    pool: &SqlitePool,
    watch_folder: &Path,
    auto_retry: bool,
    verdict_key_for: impl Fn(&str) -> String,
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

            // T-5 put a new kind of row into `error`: a mezzanine that encoded
            // cleanly and then failed QC. Its `current_path` is the *published*
            // file, not a source in the watch folder, and it carries the
            // duration, geometry, keyframes and warnings that say why it is not
            // airable. This sweep is for the debris of a failed ingest, which
            // never reached `mark_ready` and so has `duration_ms = 0`.
            //
            // Without this, deleting the mezzanine of a QC-failed asset would
            // silently delete the row too -- the same loss that
            // `purge_unusable_rows_by_fingerprint` demotes rather than deletes
            // to avoid.
            if a.duration_ms > 0 {
                out.kept_dead += 1;
                continue;
            }

            // This media has already been through the encoder, under these
            // exact settings, and the retry classifier called the failure
            // permanent. Purging the row would hand the file straight back to
            // the watcher and spend the whole encode reaching the same
            // conclusion — which is what happened on every single restart.
            //
            // Keyed on the bytes *and* the settings, so it is a conclusion that
            // can be revisited rather than a blacklist: replace the file, or
            // change the encoding or validation settings, and the key no longer
            // matches and it is tried again. Deleting the row does the same,
            // deliberately.
            let judged_permanent = a
                .qc_verdict_key
                .as_deref()
                .zip(a.source_sha256.as_deref())
                .is_some_and(|(stored, sha)| stored == verdict_key_for(sha));
            if judged_permanent {
                out.kept_permanent += 1;
                tracing::debug!(
                    "Startup recovery: {} failed permanently on unchanged media and unchanged \
                     settings; not retrying",
                    a.uuid
                );
                continue;
            }

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
/// The data directory a registry file lives in, for locating its backup folder.
fn data_dir_for_backup(db_path: &Path) -> std::path::PathBuf {
    db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// A one-off snapshot taken immediately before a migration rewrites rows.
///
/// Distinct from the daily snapshot and never rotated out by it: the whole
/// point is that it survives long enough to be useful if the migration turns
/// out to have been wrong. Named for the version that took it and the moment it
/// was taken, so several upgrades leave several files rather than overwriting
/// each other.
async fn snapshot_before_migration(
    pool: &SqlitePool,
    dir: &Path,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;

    let dest = dir.join(format!(
        "pre-migration-v{}-{}.db",
        env!("CARGO_PKG_VERSION"),
        chrono::Local::now().format("%Y-%m-%d-%H%M%S")
    ));
    if dest.exists() {
        return Ok(dest);
    }

    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await;

    let staging = dir.join(format!(".pre-migration-{}.tmp", uuid::Uuid::new_v4()));
    let escaped = staging.to_string_lossy().replace('\'', "''");
    if let Err(e) = sqlx::query(&format!("VACUUM INTO '{}'", escaped))
        .execute(pool)
        .await
    {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("VACUUM INTO failed: {}", e));
    }
    std::fs::rename(&staging, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        format!("cannot publish the snapshot: {}", e)
    })?;
    Ok(dest)
}

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

/// Rewrite rows that a new version needs to correct, once.
///
/// Split out of [`init_pool`] deliberately. Opening the registry is additive --
/// `CREATE TABLE IF NOT EXISTS`, `ADD COLUMN`, `CREATE INDEX`, `CREATE TRIGGER`
/// -- and safe for any process that merely wants to read it: a backup, a test,
/// or a service that is about to discover it may not run at all.
///
/// This is the part that changes an operator's data, so it runs only after the
/// service has established that it owns the media folder it is about to publish
/// into. A start that gets refused must leave the registry exactly as it found
/// it; the first time the ownership guard actually fired, it fired *after* this
/// work had already been done, which was harmless only by luck.
pub async fn run_data_migrations(pool: &SqlitePool, db_path: &Path) -> Result<(), sqlx::Error> {
    // Everything above this line is additive: `CREATE TABLE IF NOT EXISTS`,
    // `ADD COLUMN`, `CREATE INDEX`. Everything below rewrites rows the operator
    // already has -- it demotes statuses and adopts sub-clips onto parents --
    // and the registry holds every uuid, rating, virtual folder, trim window and
    // compliance flag anyone has ever set, none of which can be reconstructed
    // from the media files.
    //
    // The daily snapshot is taken by a background task that does not start until
    // the service is up, which is well after this runs. So the first
    // service start on a new version would rewrite the registry with no snapshot
    // of what it looked like beforehand. Take one here, once, only when there is
    // actually something to rewrite -- so it costs a `VACUUM INTO` on the
    // upgrade start and nothing on any start after it.
    {
        let pending: i64 = sqlx::query_scalar(
            "SELECT
               (SELECT COUNT(*) FROM media_assets
                 WHERE status = 'ready' AND mezzanine_ok = 0)
             + (SELECT COUNT(*) FROM media_assets
                 WHERE parent_uuid IS NULL
                   AND source_sha256 IS NULL
                   AND (trim_in_ms > 0
                        OR (trim_out_ms <> 0 AND trim_out_ms <> duration_ms)))",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        if pending > 0 {
            let dir = backup_dir(&data_dir_for_backup(db_path));
            match snapshot_before_migration(pool, &dir).await {
                Ok(path) => tracing::warn!(
                    "{} row(s) need a one-off migration; snapshot of the registry as it was \
                     written to {}",
                    pending,
                    path.display()
                ),
                // Not fatal. A service that will not start because it could not
                // write a snapshot is worse than one that starts without it --
                // but it must be loud, because this is the one start where the
                // snapshot mattered.
                Err(e) => tracing::error!(
                    "Could not snapshot the registry before migrating {} row(s): {}. \
                     Continuing; stop the service and copy media_assets.db by hand if you \
                     want a pre-migration copy.",
                    pending,
                    e
                ),
            }
        }
    }

    // T-5. `status = 'ready'` with `mezzanine_ok = 0` is a contradiction the
    // contract never defined, and its three readers each resolved it
    // differently: PlayOut's v2 library mapping said `error`, its v1 and batch
    // paths said `ready`, and this service's own dedupe said "not usable" and
    // re-transcoded forever. The same asset was red on one half of the
    // operator's screen and green on the other.
    //
    // A QC-failed mezzanine is `error`. That is what PlayOut's v2 mapping
    // already assumes, so nothing downstream has to change, and it gives the
    // dedupe a signal it reads correctly.
    //
    // W-1. This must run before the sub-clip adoption below. `init_pool` has
    // already installed the `ready => mezzanine_ok` triggers, and a sub-clip cut
    // from a QC-failed parent inherited `ready / mezzanine_ok = 0`. Adopting it
    // first is an UPDATE on a row the trigger rejects: the migration aborts,
    // `app.rs` refuses to start, and the demote that would have fixed the row
    // never runs -- so every restart fails the same way.
    {
        let demoted = sqlx::query(
            "UPDATE media_assets SET status = 'error' WHERE status = 'ready' AND mezzanine_ok = 0",
        )
        .execute(pool)
        .await?;
        if demoted.rows_affected() > 0 {
            tracing::warn!(
                "Demoted {} asset(s) that were 'ready' with a failed mezzanine to 'error' \
                 (T-5); they were never safe to air",
                demoted.rows_affected()
            );
        }
    }

    // T-2b. Adopt the sub-clips that predate `parent_uuid`.
    //
    // A sub-clip is recognisable by its trim window and by sharing a
    // `current_path` with exactly one full-length row -- it plays the same
    // physical file as its parent, which is the whole point of a sub-clip. Rows
    // whose parent cannot be identified that way are left NULL and stay
    // invisible to this column; they are still excluded from the dedupe lookup
    // by the `source_sha256 IS NOT NULL` half of the predicate.
    {
        let adopted = sqlx::query(
            "UPDATE media_assets AS child SET parent_uuid = (
                 SELECT p.uuid FROM media_assets AS p
                 WHERE p.current_path = child.current_path
                   AND p.uuid <> child.uuid
                   AND p.source_sha256 IS NOT NULL
                   AND p.trim_in_ms = 0
                   AND (p.trim_out_ms = 0 OR p.trim_out_ms = p.duration_ms)
                 LIMIT 1
             )
             WHERE child.parent_uuid IS NULL
               AND child.source_sha256 IS NULL
               AND (child.trim_in_ms > 0
                    OR (child.trim_out_ms <> 0 AND child.trim_out_ms <> child.duration_ms))",
        )
        .execute(pool)
        .await?;
        if adopted.rows_affected() > 0 {
            tracing::info!(
                "Adopted {} pre-existing sub-clip row(s) onto parent_uuid",
                adopted.rows_affected()
            );
        }
    }

    Ok(())
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
    let pool = init_schema(pool).await?;

    tracing::info!(
        "Database initialized at {} (WAL mode, media_assets ready)",
        db_path.display()
    );

    Ok(pool)
}

/// The additive half of [`init_pool`], on a pool that is already connected.
/// Split out so the migration tests can build the real schema -- triggers
/// included -- on an in-memory database.
async fn init_schema(pool: SqlitePool) -> Result<SqlitePool, sqlx::Error> {
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
            original_virtual_folder TEXT DEFAULT NULL,
            parent_uuid TEXT DEFAULT NULL,
            qc_verdict_key TEXT DEFAULT NULL
        )",
    )
    .execute(&pool)
    .await?;

    // T-3. A stable identity for *this registry*, so the media folder it
    // publishes into can be stamped with it and a second registry pointed at
    // the same folder can be refused. Generated once, on the database that owns
    // it, and never changed: it survives a data-directory move, which is the
    // case a path comparison would get wrong.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS registry_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query("INSERT OR IGNORE INTO registry_meta (key, value) VALUES ('registry_id', ?1)")
        .bind(uuid::Uuid::new_v4().to_string())
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
        // T-2b. Nullable: NULL means "this is a full-length asset", which is
        // what every row written before this column existed was, bar the
        // sub-clips the backfill below identifies.
        ("parent_uuid", "TEXT", "NULL"),
        // T-2. What was judged, and under what settings. Nullable: rows
        // published before it existed fall back to the narrower legacy test in
        // `find_reproducible_qc_failure`.
        ("qc_verdict_key", "TEXT", "NULL"),
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

    // ...and now make it unrepresentable. SQLite cannot add a `CHECK` to an
    // existing table without rebuilding it, and rebuilding the one table the
    // whole station's playout depends on to add an assertion is the wrong
    // trade. A pair of `BEFORE` triggers is the same guarantee at the same
    // place -- every writer, including a hand-run `UPDATE` in the DB viewer --
    // and it installs idempotently on a live registry.
    for trigger in [
        "CREATE TRIGGER IF NOT EXISTS trg_media_assets_ready_requires_mezzanine_insert
         BEFORE INSERT ON media_assets
         FOR EACH ROW WHEN NEW.status = 'ready' AND NEW.mezzanine_ok = 0
         BEGIN SELECT RAISE(ABORT, 'status ready requires mezzanine_ok'); END",
        "CREATE TRIGGER IF NOT EXISTS trg_media_assets_ready_requires_mezzanine_update
         BEFORE UPDATE ON media_assets
         FOR EACH ROW WHEN NEW.status = 'ready' AND NEW.mezzanine_ok = 0
         BEGIN SELECT RAISE(ABORT, 'status ready requires mezzanine_ok'); END",
    ] {
        sqlx::query(trigger).execute(&pool).await?;
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
        // Every ingest that matches a fingerprint asks this question once.
        "CREATE INDEX IF NOT EXISTS idx_media_assets_qc_verdict_key ON media_assets(qc_verdict_key)",
        "CREATE INDEX IF NOT EXISTS idx_media_assets_source_sha256 ON media_assets(source_sha256)",
    ] {
        let _ = sqlx::query(idx).execute(&pool).await;
    }

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

/// Publish the outcome of a completed encode.
///
/// Despite the name this is also how a *failed* QC lands: the row gets its real
/// duration, geometry, keyframes and warnings either way, and `mezzanine_ok`
/// decides whether the status is `ready` or `error` (T-5). The metadata is
/// worth keeping in both cases -- it is what tells an operator, and the dedupe,
/// why the asset is not airable.
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
    qc_verdict_key: Option<&str>,
) -> Result<(), sqlx::Error> {
    let warnings_json = serde_json::to_string(warnings).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "UPDATE media_assets SET
            current_path = ?1,
            duration_ms = ?2,
            trim_in_ms = 0,
            trim_out_ms = ?2,
            -- T-5. `ready` is a promise that the asset can go to air now. A
            -- mezzanine that failed QC cannot, so it is `error` and the
            -- contradiction the three consumers each read differently is never
            -- written in the first place. The triggers in `init_pool` refuse it
            -- even if some other writer tries.
            status = CASE WHEN ?3 THEN 'ready' ELSE 'error' END,
            mezzanine_ok = ?3,
            fps = ?4,
            fps_num = ?5,
            fps_den = ?6,
            total_frames = ?7,
            gop_frames = ?8,
            keyframe_safe_start_ms = ?9,
            warnings = ?10,
            keyframe_offsets_json = ?11,
            -- What was judged and under what settings, so a failure can be
            -- re-examined when the settings change instead of being re-run
            -- verbatim on every restart (T-2).
            qc_verdict_key = ?12
         WHERE uuid = ?13",
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
    .bind(qc_verdict_key)
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

/// Record an ingest failure that the retry classifier judged **permanent**,
/// together with the verdict key identifying what was judged and under what
/// settings.
///
/// The key is what stops [`recover_failed_assets`] purging the row for retry on
/// the next start. Without it the sweep deletes the row, the watcher re-offers
/// the file, and the whole encode is spent reaching the same conclusion —
/// on every restart, for ever, on media that cannot be ingested.
pub async fn mark_error_permanent(
    pool: &SqlitePool,
    uuid: &str,
    qc_verdict_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE media_assets SET status = 'error', qc_verdict_key = ?1 WHERE uuid = ?2",
    )
    .bind(qc_verdict_key)
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

/// This registry's stable identity (T-3). See `media_root`.
pub async fn registry_id(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM registry_meta WHERE key = 'registry_id'")
        .fetch_one(pool)
        .await
}

/// QC findings that can fail for reasons outside the media.
///
/// The inverse list, and deliberately so. Almost every QC finding is
/// *reproducible*: the pipeline probes a finished encode against its source
/// under a fixed policy, so the same bytes judged under the same settings reach
/// the same verdict, every time, for ever. Trying again is pure waste.
///
/// These are the exceptions -- a finding that says more about the machine than
/// about the media. A row whose findings are *all* on this list stays
/// retryable, because the next attempt genuinely might differ.
pub const ENVIRONMENTAL_QC_CODES: &[&str] = &[
    // ffprobe could not be run, or exited non-zero. A missing or busy toolchain
    // is not a property of the programme, and blacklisting a good master
    // because ffprobe was mid-upgrade would be a poor trade.
    "keyframe_scan_failed",
];

/// QC findings that are unmistakably properties of the **source**.
///
/// Only used for rows published before `qc_verdict_key` existed, where there is
/// no record of the settings the verdict was reached under. Narrow on purpose:
/// without the settings, "reproducible" cannot be established, so the legacy
/// path asserts it only for a finding that no configuration could plausibly
/// have caused.
pub const SOURCE_ATTRIBUTABLE_QC_CODES: &[&str] = &["duration_delta_exceeded"];

/// Has this exact media, judged under these exact settings, already failed QC?
///
/// This is what stops the service re-encoding known-bad media on every start.
/// Three sources whose container duration disagrees with their real one
/// accumulated eleven registry rows between them, three more on every restart,
/// because a QC-failed row was never recognised as a duplicate and the
/// re-ingest cleanup refused to delete it.
///
/// The verdict is keyed on **the bytes and the settings together**, which is
/// what makes the skip safe to make permanent:
///
/// * change the media and the source hash changes, so it is judged afresh. A
///   part-copied file that later completes is a different key, not a blacklist
///   entry.
/// * change the encoding profile or the validation policy -- widen
///   `max_duration_delta_ms`, turn off `enforce_closed_gop` -- and every stored
///   verdict stops matching, so all of it gets another chance. An operator who
///   loosens the tolerance *specifically to accept these files* must not find
///   them still being skipped.
/// * change neither, and there is nothing to learn from encoding it again.
///
/// A row whose findings are all [`ENVIRONMENTAL_QC_CODES`] is never permanent.
///
/// `legacy_source_sha256` covers rows published before the key existed, which
/// have no record of the settings that judged them; for those the much narrower
/// [`SOURCE_ATTRIBUTABLE_QC_CODES`] test applies.
pub async fn find_reproducible_qc_failure(
    pool: &SqlitePool,
    verdict_key: &str,
    legacy_source_sha256: &str,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets
         WHERE deleted_at IS NULL
           AND parent_uuid IS NULL
           AND status = 'error'
           AND mezzanine_ok = 0
           AND (qc_verdict_key = ?1 OR (qc_verdict_key IS NULL AND source_sha256 = ?2))
         ORDER BY (qc_verdict_key IS NOT NULL) DESC, rowid DESC",
        SELECT_COLS
    );
    let rows = sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(verdict_key)
        .bind(legacy_source_sha256)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().find(|a| {
        let findings: Vec<String> = serde_json::from_str(&a.warnings).unwrap_or_default();
        if findings.is_empty() {
            // A row with no recorded findings says nothing about why it failed.
            // It is not evidence of bad media.
            return false;
        }
        if a.qc_verdict_key.is_some() {
            // The precise test: the same bytes under the same settings reached
            // this verdict. Reproducible unless every finding is environmental.
            findings
                .iter()
                .any(|c| !ENVIRONMENTAL_QC_CODES.contains(&c.as_str()))
        } else {
            // The legacy test: no record of the settings, so only a finding
            // that no setting could have caused counts.
            findings
                .iter()
                .any(|c| SOURCE_ATTRIBUTABLE_QC_CODES.contains(&c.as_str()))
        }
    }))
}

/// Marks a verdict an operator has explicitly overruled.
///
/// **Not** `NULL`. Clearing the column to NULL would drop the row into the
/// *legacy* branch of [`find_reproducible_qc_failure`] — "no record of the
/// settings, fall back to the source hash" — which catches exactly the
/// `duration_delta_exceeded` rows an operator is most likely to be overruling.
/// The override would appear to work and change nothing.
///
/// A real key is 64 hex characters, so this prefix can never collide with one:
/// the row keeps a non-NULL key, stays out of the legacy branch, and matches no
/// computed verdict. The next failure overwrites it with a real one.
pub const QC_VERDICT_CLEARED: &str = "cleared-by-operator";

/// Will the service refuse to re-ingest this media?
///
/// True exactly when a live verdict is recorded against it: an operator's
/// override writes [`QC_VERDICT_CLEARED`], which is not a live verdict, and a
/// row that never failed has none at all.
///
/// Derived rather than stored so there is one definition of "held back", shared
/// by the API payloads and the UI badge. Inferring it client-side from the
/// status and the warnings got it wrong the moment an operator cleared a
/// verdict: neither of those changes, so the badge went on claiming the media
/// would not be retried after it had been released.
pub fn is_retry_suppressed(qc_verdict_key: Option<&str>) -> bool {
    matches!(qc_verdict_key, Some(k) if k != QC_VERDICT_CLEARED)
}

/// Let the service reconsider media it has given up on.
///
/// The operator's override for the skip. The service's rule is "same bytes,
/// same settings, same answer"; this is how a human says "humour me".
///
/// Returns false when there was no verdict to overrule, so the caller can say
/// "nothing to do" rather than implying it changed something.
pub async fn clear_qc_verdict(pool: &SqlitePool, uuid: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE media_assets SET qc_verdict_key = ?1
         WHERE uuid = ?2 AND qc_verdict_key IS NOT NULL AND qc_verdict_key <> ?1",
    )
    .bind(QC_VERDICT_CLEARED)
    .bind(uuid)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Stamp a verdict key onto a row that predates the column, once the current
/// settings have been confirmed to reproduce its failure.
///
/// Lets the legacy set converge on the precise test instead of relying on
/// [`SOURCE_ATTRIBUTABLE_QC_CODES`] for ever -- and, more usefully, means a
/// later settings change releases these rows too.
pub async fn adopt_qc_verdict_key(
    pool: &SqlitePool,
    uuid: &str,
    verdict_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE media_assets SET qc_verdict_key = ?1 WHERE uuid = ?2 AND qc_verdict_key IS NULL")
        .bind(verdict_key)
        .bind(uuid)
        .execute(pool)
        .await?;
    Ok(())
}

/// Find the active asset to test a new ingest against, by sampled fingerprint.
///
/// Two things this must not do, both learned the hard way:
///
/// * **Return a sub-clip** (T-2b). A sub-clip carries its parent's fingerprint
///   and its parent's path with a NULL `source_sha256`, so the dedupe read it
///   as a legacy row it could not confirm and re-ingested the parent's source.
///   Making a sub-clip started a re-encode loop of its own, distinct from the
///   QC one.
/// * **Return an arbitrary row.** There was no `ORDER BY` and no `LIMIT`, so
///   with eleven rows sharing a fingerprint the answer was whatever SQLite
///   yielded first. Stable in practice, which is precisely why the loop never
///   accidentally corrected itself. Prefer the row that is actually usable.
pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: i64,
) -> Result<Option<MediaAsset>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM media_assets
         WHERE fingerprint = ?1
           AND deleted_at IS NULL
           AND parent_uuid IS NULL
           AND source_sha256 IS NOT NULL
         ORDER BY mezzanine_ok DESC, (status = 'ready') DESC, rowid DESC
         LIMIT 1",
        SELECT_COLS
    );
    sqlx::query_as::<_, MediaAsset>(&sql)
        .bind(fingerprint)
        .fetch_optional(pool)
        .await
}

/// A distinct mezzanine on disk, and every registry row that plays it.
#[derive(Debug, Clone)]
pub struct KeyframeRescanTarget {
    pub current_path: String,
    /// The rows sharing this file: the full-length asset and any sub-clips cut
    /// from it, all of which copied the parent's (wrong) offsets.
    pub uuids: Vec<String>,
    pub keyframe_safe_start_ms: i64,
    /// The stored offsets, verbatim, so the backfill can tell "already
    /// corrected" from "coincidentally has the right safe start".
    pub keyframe_offsets_json: String,
}

/// Every mezzanine whose recorded keyframes came from the broken parser (T-1c).
///
/// Two shapes qualify and both are wrong:
///
/// * `keyframe_safe_start_ms > 0` -- the keyframe at pts 0 was dropped, so the
///   first *surviving* offset became the safe start. Every real mezzanine this
///   service has produced is in this group.
/// * an empty offsets list -- either the scan failed, or it succeeded and every
///   line was discarded. Selecting only on `> 0` would skip these, and they are
///   exactly the rows T-7 is about.
///
/// Grouped by file, because a sub-clip carries a copy of its parent's offsets
/// and has to be corrected with it, and because re-scanning one physical file
/// once is the whole point.
///
/// `duration_ms > 0` restricts this to rows that actually reached `mark_ready`.
/// A failed ingest's leftovers also carry an empty offsets list, but their
/// `current_path` is still the *source* file, and re-scanning a source to write
/// mezzanine keyframes onto a dead row is meaningless.
pub async fn keyframe_rescan_targets(
    pool: &SqlitePool,
) -> Result<Vec<KeyframeRescanTarget>, sqlx::Error> {
    let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT current_path, uuid, keyframe_safe_start_ms, keyframe_offsets_json FROM media_assets
         WHERE deleted_at IS NULL
           AND current_path <> ''
           AND duration_ms > 0
           AND (keyframe_safe_start_ms > 0
                OR keyframe_offsets_json IN ('[]', ''))
         ORDER BY current_path, rowid",
    )
    .fetch_all(pool)
    .await?;

    let mut targets: Vec<KeyframeRescanTarget> = Vec::new();
    for (path, uuid, safe_start, offsets_json) in rows {
        match targets.last_mut() {
            Some(t) if t.current_path == path => t.uuids.push(uuid),
            _ => targets.push(KeyframeRescanTarget {
                current_path: path,
                uuids: vec![uuid],
                keyframe_safe_start_ms: safe_start,
                keyframe_offsets_json: offsets_json,
            }),
        }
    }
    Ok(targets)
}

/// Write corrected keyframes onto every row that plays one file.
pub async fn apply_keyframe_rescan(
    pool: &SqlitePool,
    current_path: &str,
    keyframe_safe_start_ms: i64,
    keyframe_offsets_json: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE media_assets SET keyframe_safe_start_ms = ?1, keyframe_offsets_json = ?2
         WHERE current_path = ?3 AND deleted_at IS NULL",
    )
    .bind(keyframe_safe_start_ms)
    .bind(keyframe_offsets_json)
    .bind(current_path)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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
            "INSERT INTO media_assets (uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, tp, status, display_name, virtual_folder, mezzanine_ok, fps, fps_num, fps_den, total_frames, gop_frames, keyframe_safe_start_ms, warnings, keyframe_offsets_json, parent_uuid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
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
        // T-2b. Without this the sub-clip is indistinguishable from a legacy
        // row in the dedupe lookup: same fingerprint as its parent, same file,
        // and a NULL `source_sha256` that reads as "cannot confirm, re-ingest".
        // Making a sub-clip re-encoded the programme it was cut from.
        .bind(parent_uuid)
        .execute(pool)
        .await?;

        find_by_uuid(pool, new_uuid).await
    } else {
        Ok(None)
    }
}

/// What a pass of [`reconcile_missing_paths`] changed.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MissingReconcile {
    /// `ready` rows whose mezzanine is no longer on disk.
    pub went_missing: u64,
    /// `missing` rows whose mezzanine is back -- a remounted share, usually.
    pub came_back: u64,
    /// Rows sitting at `missing` after this pass.
    pub missing_total: i64,
}

/// Stat every published mezzanine and keep `status` honest about it (T-4).
///
/// The registry could say `ready` about a file that is not there, and the first
/// thing that noticed was PlayOut's pre-flight check at TAKE -- on air, one clip
/// too late. Twelve rundown rows pointed at a retired registry's paths and
/// nothing upstream of transmission knew.
///
/// PlayOut now stats its own rundown rather than waiting for this, but the
/// server is the side that actually knows, and `missing` is a status its
/// `IngestorStatus` union already has a branch for.
///
/// `ready` and `missing` are the only two states this moves between, in both
/// directions. An `error` row is already not airable and the operator's reason
/// for it is worth more than this one; a `processing` row's `current_path` is
/// still its *source*, so stat'ing it would mean something else entirely.
///
/// `file_exists` is injected so the sweep is testable without a filesystem, and
/// so the caller can run the real `stat`s off the async runtime -- on an SMB
/// target each one is a round trip.
pub async fn reconcile_missing_paths(
    pool: &SqlitePool,
    file_exists: impl Fn(&str) -> bool,
) -> Result<MissingReconcile, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT uuid, status, current_path FROM media_assets
         WHERE deleted_at IS NULL AND status IN ('ready', 'missing')",
    )
    .fetch_all(pool)
    .await?;

    let mut out = MissingReconcile::default();

    for (uuid, status, path) in rows {
        let present = !path.is_empty() && file_exists(&path);
        match (status.as_str(), present) {
            ("ready", false) => {
                sqlx::query("UPDATE media_assets SET status = 'missing' WHERE uuid = ?1")
                    .bind(&uuid)
                    .execute(pool)
                    .await?;
                out.went_missing += 1;
                tracing::warn!(
                    "Asset {} is 'ready' but its mezzanine is gone from {}; marked 'missing'",
                    uuid,
                    path
                );
            }
            ("missing", true) => {
                // Back to `ready` only if the mezzanine still passed QC. The
                // trigger in `init_pool` refuses the other case outright, and
                // it is the right refusal: a QC-failed row belongs at `error`.
                let restored = sqlx::query(
                    "UPDATE media_assets SET status = 'ready'
                     WHERE uuid = ?1 AND mezzanine_ok = 1",
                )
                .bind(&uuid)
                .execute(pool)
                .await?;
                if restored.rows_affected() > 0 {
                    out.came_back += 1;
                    tracing::info!("Asset {} is back on disk at {}; restored to 'ready'", uuid, path);
                } else {
                    sqlx::query("UPDATE media_assets SET status = 'error' WHERE uuid = ?1")
                        .bind(&uuid)
                        .execute(pool)
                        .await?;
                    tracing::warn!(
                        "Asset {} is back on disk but never passed QC; moved to 'error'",
                        uuid
                    );
                }
            }
            _ => {}
        }
    }

    out.missing_total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM media_assets WHERE status = 'missing' AND deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await?;

    Ok(out)
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
    let rows: Vec<(String, String, i64, i64, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT uuid, status, trim_in_ms, trim_out_ms, duration_ms, current_path, parent_uuid
         FROM media_assets WHERE fingerprint = ?1",
    )
    .bind(fingerprint)
    .fetch_all(pool)
    .await?;

    let mut out = FingerprintPurge::default();

    for (uuid, status, trim_in, trim_out, duration, path, parent_uuid) in rows {
        if parent_uuid.is_some() || is_subclip_row(trim_in, trim_out, duration) {
            out.protected += 1;
            continue;
        }
        // T-5 made a QC-failed mezzanine `error` rather than `ready`, which
        // walked it straight into the `error` arm below -- and that arm deletes
        // the row while its published mezzanine stays on disk, orphaned in the
        // Caspar media folder with nothing referencing it. A row that reached
        // `mark_ready` has a real duration; a failed ingest's leftovers have 0.
        // That is the line between "someone's asset, which happens not to be
        // airable" and "debris".
        if status == "error" && duration > 0 && !path.is_empty() && file_exists(&path) {
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

/// What a purge should do with the media file, as opposed to the registry row.
///
/// The two things an operator means by "delete" are genuinely different and
/// only one of them is reversible-ish: a row can be re-ingested from the source,
/// a deleted mezzanine cannot.
///
/// The previous pair of variants, `PreserveReferencedMezzanine` and
/// `DeleteUnreferencedMezzanine`, evaluated to the identical rule
/// (`remaining_refs == 0`) -- they are two names for the same sentence -- so the
/// `preserve_subclips_on_purge` setting that chose between them had no effect
/// whatsoever. Sub-clips were protected regardless, by the reference count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PurgeMode {
    /// Delete the registry row and leave the media where it is.
    ///
    /// For taking a row out of the library without touching the broadcast
    /// folder -- a duplicate entry, a mistaken ingest of a file somebody else
    /// owns.
    KeepMedia,
    /// Delete the row, and the media file too, when nothing else references it.
    ///
    /// Never deletes a file a sub-clip still plays: that is the reference
    /// count's job and it is not negotiable by the caller.
    #[default]
    DeleteMediaIfUnreferenced,
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
        PurgeMode::KeepMedia => false,
        PurgeMode::DeleteMediaIfUnreferenced => remaining_refs == 0 && !path.is_empty(),
    };
    // W-2. Whether the file is ours to delete is a question about where it is,
    // not about the row's status. This used to skip every non-`ready` row on
    // the theory that only `ready` pointed at a published mezzanine (F-19), but
    // T-5 made a QC-failed *published* mezzanine `error` -- and those files
    // outlived their rows: 11 orphaned `.mp4`s in the live media folder, all
    // still playable by name in CasparCG. `validate_purge_path` already fails
    // closed on anything outside `target_folder` or inside `watch_folder`, and
    // config refuses overlapping roots, so a source file for a `processing` or
    // pre-publish `error` row can never pass it.
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
/// Build the `LIKE` pattern matching everything *containing* `term`.
///
/// The DB viewer's search box used to filter in Rust with `contains`, so a `%`
/// or `_` an operator typed was a literal. Pushing the search into SQL must not
/// quietly turn it into a wildcard, so the term is escaped the same way
/// `like_prefix` escapes a folder name and used with `ESCAPE '\'`.
pub fn like_contains(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 4);
    out.push('%');
    for ch in term.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

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

/// Forget a finished job row.
///
/// The durable half of dismissing a job. The in-memory queue is the live view;
/// this stops the record coming back the next time the service starts and
/// repopulates from `transcode_jobs`.
pub async fn delete_durable_job(pool: &SqlitePool, id: &str) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("DELETE FROM transcode_jobs WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// Forget several finished job rows, in one statement.
pub async fn delete_durable_jobs(pool: &SqlitePool, ids: &[String]) -> Result<u64, sqlx::Error> {
    if ids.is_empty() {
        return Ok(0);
    }
    // Chunked because SQLite's default parameter limit is 999 and a busy day
    // can leave more failed jobs than that.
    let mut total = 0;
    for chunk in ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM transcode_jobs WHERE id IN ({})", placeholders);
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id);
        }
        total += q.execute(pool).await?.rows_affected();
    }
    Ok(total)
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
    /// See [`is_retry_suppressed`].
    pub retry_suppressed: bool,
    pub is_subclip: bool,
    pub parent_uuid: Option<String>,
    pub deleted_at: Option<String>,
    pub sidecar_exists: bool,
}

/// Does the identity sidecar for this asset still exist on disk?
///
/// A blocking `stat`, and on an SMB target folder a 1-10 ms round trip, so it
/// must never run on the async runtime for more than a page of rows (F-07).
fn sidecar_exists_for(current_path: &str) -> bool {
    if current_path.is_empty() {
        return false;
    }
    crate::identity::sidecar_path_for(std::path::Path::new(current_path)).exists()
}

impl DbAssetSummary {
    /// Everything derivable from the row alone. `sidecar_exists` is left
    /// `false` for the caller to fill in, off the runtime, for the page it is
    /// actually going to return.
    fn from_asset_row(a: MediaAsset) -> Self {
        // `parent_uuid` is the fact; the rest is the inference that had to
        // stand in for it before the column existed (T-2b). The name test in
        // particular is a guess -- an asset an operator called "Subclip reel"
        // is not one -- but it is kept for rows the startup adoption could not
        // match to a parent.
        let is_subclip = a.parent_uuid.is_some()
            || a.trim_in_ms > 0
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
            retry_suppressed: is_retry_suppressed(a.qc_verdict_key.as_deref()),
            is_subclip,
            parent_uuid: a.parent_uuid,
            deleted_at: a.deleted_at,
            sidecar_exists: false,
        }
    }

    /// The single-row path (`get_db_asset_detail`), where one `stat` is fine.
    pub fn from_asset(a: MediaAsset) -> Self {
        let current_path = a.current_path.clone();
        let mut summary = Self::from_asset_row(a);
        summary.sidecar_exists = sidecar_exists_for(&current_path);
        summary
    }
}

/// An asset counts as a subclip when it carries a trim window or says so in
/// its name. Kept here as one string because the SQL listing and the Rust
/// projection must not drift: both `DbAssetSummary::is_subclip` and the
/// `master`/`subclip` filters are this predicate.
const SUBCLIP_PREDICATE: &str = "(trim_in_ms > 0 \
     OR (trim_out_ms > 0 AND trim_out_ms < duration_ms) \
     OR lower(display_name) LIKE '%subclip%' \
     OR lower(display_name) LIKE '%sub-clip%')";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAssetsPage {
    pub items: Vec<DbAssetSummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

/// List assets for the DB viewer, filtered, searched and paged **in SQL**.
///
/// This used to `SELECT` the whole table with no `WHERE` and no `LIMIT`, map
/// every row through `DbAssetSummary::from_asset` -- which parsed the
/// keyframe-offsets JSON and issued a filesystem `stat` per row -- and then
/// filter and page the result in memory. With 5 000 assets one keystroke in
/// the search box (debounced at 300 ms) cost 5 000 row decodes, 5 000 JSON
/// parses and 5 000 stats, on the async runtime that also answers
/// `/api/health`.
///
/// The response shape is unchanged, so the DB tab needs no change.
pub async fn query_db_assets(
    pool: &SqlitePool,
    filter: Option<&str>,
    search: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<DbAssetsPage, sqlx::Error> {
    let lim = limit.unwrap_or(25).clamp(1, 100);
    let off = offset.unwrap_or(0).max(0);

    let filter_mode = filter.unwrap_or("all").to_ascii_lowercase();
    let search_term = search
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();

    // The filter arm is chosen from a fixed set, never interpolated from the
    // request; only the search term is bound, and it is bound, never formatted.
    let mut clauses: Vec<String> = Vec::new();
    match filter_mode.as_str() {
        "master" => clauses.push(format!("deleted_at IS NULL AND NOT {}", SUBCLIP_PREDICATE)),
        "subclip" => clauses.push(format!("deleted_at IS NULL AND {}", SUBCLIP_PREDICATE)),
        "ready" => clauses.push("status = 'ready' AND deleted_at IS NULL".to_string()),
        "processing" => clauses.push("status = 'processing' AND deleted_at IS NULL".to_string()),
        "error" => clauses.push("status = 'error' AND deleted_at IS NULL".to_string()),
        "trashed" => clauses.push("deleted_at IS NOT NULL".to_string()),
        _ => {}
    }

    let mut binds: Vec<String> = Vec::new();
    if !search_term.is_empty() {
        clauses.push(
            "(lower(display_name) LIKE ? ESCAPE '\\' \
              OR lower(uuid) LIKE ? ESCAPE '\\' \
              OR lower(virtual_folder) LIKE ? ESCAPE '\\' \
              OR lower(current_path) LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        let pattern = like_contains(&search_term);
        binds.extend(std::iter::repeat_n(pattern, 4));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM media_assets{}", where_sql);
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let total = count_q.fetch_one(pool).await?;

    // The `COALESCE` in the ORDER BY defeats every index -- `EXPLAIN QUERY PLAN`
    // still reports `USE TEMP B-TREE FOR ORDER BY` even with an index on
    // (deleted_at, display_name, uuid), which only turns the table scan into a
    // covering-index scan. So no such index is added: measured over 3 000 rows
    // the whole call is 0.66 ms unfiltered and 1.61 ms with a search term. The
    // cost here was never the sort, it was the 5 000 stats and 5 000 JSON
    // parses this function used to do.
    let page_sql = format!(
        "SELECT {} FROM media_assets{} ORDER BY COALESCE(deleted_at, '9999') ASC, display_name ASC, uuid ASC LIMIT ? OFFSET ?",
        SELECT_COLS, where_sql
    );
    let mut page_q = sqlx::query_as::<_, MediaAsset>(&page_sql);
    for b in &binds {
        page_q = page_q.bind(b);
    }
    let rows: Vec<MediaAsset> = page_q.bind(lim).bind(off).fetch_all(pool).await?;

    let mut items: Vec<DbAssetSummary> =
        rows.into_iter().map(DbAssetSummary::from_asset_row).collect();

    // Operators use `sidecar_exists` to spot F-28-style drift, so it stays --
    // but only for the <=100 rows being returned, and off the runtime.
    let paths: Vec<String> = items.iter().map(|i| i.current_path.clone()).collect();
    let flags = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|p| sidecar_exists_for(p))
            .collect::<Vec<bool>>()
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("sidecar existence check failed: {}", e);
        Vec::new()
    });
    for (item, exists) in items.iter_mut().zip(flags) {
        item.sidecar_exists = exists;
    }

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

/// List jobs for the DB viewer, filtered, searched and paged **in SQL**.
///
/// Same shape of fix as `query_db_assets`: this used to `SELECT *` the whole
/// `transcode_jobs` table -- `stderr_log_json` and all, up to 200 lines per
/// failed job -- and filter it in memory for a 25-row page.
pub async fn query_db_jobs(
    pool: &SqlitePool,
    state: Option<&str>,
    search: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<DbJobsPage, sqlx::Error> {
    let lim = limit.unwrap_or(25).clamp(1, 100);
    let off = offset.unwrap_or(0).max(0);

    let state_term = state
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let search_term = search
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();

    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    // A job matches on either its state or its phase, as it did in memory --
    // that is what lets the viewer filter on `skipped`, which is a phase.
    if !state_term.is_empty() && state_term != "all" {
        clauses.push("(lower(state) = ? OR lower(phase) = ?)".to_string());
        binds.push(state_term.clone());
        binds.push(state_term.clone());
    }

    if !search_term.is_empty() {
        clauses.push(
            "(lower(id) LIKE ? ESCAPE '\\' \
              OR lower(uuid) LIKE ? ESCAPE '\\' \
              OR lower(input_path) LIKE ? ESCAPE '\\' \
              OR lower(output_path) LIKE ? ESCAPE '\\' \
              OR lower(error) LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        let pattern = like_contains(&search_term);
        binds.extend(std::iter::repeat_n(pattern, 5));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM transcode_jobs{}", where_sql);
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let total = count_q.fetch_one(pool).await?;

    // idx_transcode_jobs_created_at already covers the ordering.
    let page_sql = format!(
        "SELECT * FROM transcode_jobs{} ORDER BY created_at DESC LIMIT ? OFFSET ?",
        where_sql
    );
    let mut page_q = sqlx::query_as::<_, DurableJobRow>(&page_sql);
    for b in &binds {
        page_q = page_q.bind(b);
    }
    let rows: Vec<DurableJobRow> = page_q.bind(lim).bind(off).fetch_all(pool).await?;

    let items: Vec<DbJobSummary> = rows.into_iter().map(DbJobSummary::from_row).collect();

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
            None,
        )
        .await
        .unwrap();

        assert!(video_path.exists());
        assert!(sidecar_path.exists());

        let outcome = purge_asset_in_target(&pool, uuid, PurgeMode::DeleteMediaIfUnreferenced, &temp_dir)
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
            None,
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
            purge_asset_in_target(&pool, parent_uuid, PurgeMode::DeleteMediaIfUnreferenced, &temp_dir)
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
            purge_asset_in_target(&pool, subclip_uuid, PurgeMode::DeleteMediaIfUnreferenced, &temp_dir)
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
            None,
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

        let out = purge_asset_in_target(&pool, sub1, PurgeMode::DeleteMediaIfUnreferenced, &temp_dir)
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

        let out = purge_asset_in_target(&pool, uuid, PurgeMode::DeleteMediaIfUnreferenced, &temp_dir)
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


    /// SB-01: the jobs listing filters and pages in SQL too. The state filter
    /// still matches either `state` or `phase` -- that is what makes `skipped`
    /// (a phase, not a state) selectable -- and the search term is still a
    /// literal, not a LIKE pattern.
    #[tokio::test]
    async fn job_listing_filters_on_state_or_phase_and_pages_in_sql() {
        let (pool, temp_dir) = setup_test_pool().await;

        let mut batch: Vec<crate::jobs::JobRecord> = Vec::new();
        for i in 0..6 {
            let mut j = crate::jobs::JobRecord::new(&format!("D:/media/clip_{}.mov", i), "ProfileA");
            j.id = format!("job-{}", i);
            batch.push(j);
        }
        batch[0].state = crate::jobs::JobState::Completed;
        batch[0].phase = crate::jobs::JobPhase::Skipped;
        batch[1].state = crate::jobs::JobState::Completed;
        batch[1].phase = crate::jobs::JobPhase::Completed;
        batch[2].state = crate::jobs::JobState::Failed;
        batch[2].error = Some("disk full while writing".to_string());
        // A name with a LIKE metacharacter in it.
        batch[3].input_path = "D:/media/100% final.mov".to_string();
        persist_jobs(&pool, &batch).await.unwrap();

        let all = query_db_jobs(&pool, Some("all"), None, Some(100), Some(0))
            .await
            .unwrap();
        assert_eq!(all.total, 6);

        // `skipped` is a phase; selecting it must still work.
        let skipped = query_db_jobs(&pool, Some("skipped"), None, Some(100), Some(0))
            .await
            .unwrap();
        let ids: Vec<&str> = skipped.items.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["job-0"]);

        // `Completed` is a state, and matching is case-insensitive as before.
        let completed = query_db_jobs(&pool, Some("completed"), None, Some(100), Some(0))
            .await
            .unwrap();
        assert_eq!(completed.total, 2);

        // Search reaches the error text and the input path.
        let by_error = query_db_jobs(&pool, None, Some("DISK FULL"), Some(100), Some(0))
            .await
            .unwrap();
        let ids: Vec<&str> = by_error.items.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["job-2"]);

        // `_` stays literal: "clip_1" must not also match "clipX1".
        let underscore = query_db_jobs(&pool, None, Some("clip_1"), Some(100), Some(0))
            .await
            .unwrap();
        let ids: Vec<&str> = underscore.items.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["job-1"]);

        let percent = query_db_jobs(&pool, None, Some("100%"), Some(100), Some(0))
            .await
            .unwrap();
        let ids: Vec<&str> = percent.items.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["job-3"]);

        // Paging tiles the filter.
        let first = query_db_jobs(&pool, None, None, Some(4), Some(0)).await.unwrap();
        let second = query_db_jobs(&pool, None, None, Some(4), Some(4)).await.unwrap();
        assert_eq!(first.items.len(), 4);
        assert_eq!(second.items.len(), 2);
        assert_eq!(first.total, 6);
        assert_eq!(second.total, 6);

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
        mark_ready(&pool, uuid, "D:/target/clip1.mp4", 5000, true, 25.0, 25, 1, 125, 50, 0, &[], "[]", None)
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
        // For a `processing`/pre-publish `error` row, `current_path` is still
        // the SOURCE file in the watch folder. The row must go; the file must
        // not (F-19). Since W-2 that is decided by the path, not the status.
        let target = temp_dir.join("target");
        let watch = temp_dir.join("watch");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&watch).unwrap();
        let source = watch.join("source_clip.mp4");
        std::fs::File::create(&source).unwrap();

        let uuid = "error-row-uuid";
        insert_processing(&pool, uuid, 4242, None, &source.to_string_lossy(), "Source")
            .await
            .unwrap();
        mark_error(&pool, uuid).await.unwrap();

        let result = purge_single_asset_with_context(
            &pool,
            uuid,
            PurgeMode::DeleteMediaIfUnreferenced,
            Some(&target),
            Some(&watch),
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
                .any(|w| w.contains("outside managed target")),
            "the operator must be told why cleanup was skipped: {:?}",
            result.warnings
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// W-2. T-5 made a QC-failed *published* mezzanine `error`. Its file sits
    /// in `target_folder` like any other mezzanine, and purging the row must
    /// take the file and sidecar with it -- otherwise CasparCG can still play
    /// an asset the registry no longer knows about.
    #[tokio::test]
    async fn test_purge_of_qc_failed_mezzanine_removes_the_file() {
        let (pool, temp_dir) = setup_test_pool().await;
        let target = temp_dir.join("target");
        let watch = temp_dir.join("watch");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&watch).unwrap();
        let media_file = target.join("qc_failed.mp4");
        let sidecar_file = crate::identity::sidecar_path_for(&media_file);
        std::fs::create_dir_all(sidecar_file.parent().unwrap()).unwrap();
        std::fs::File::create(&media_file).unwrap();
        std::fs::File::create(&sidecar_file).unwrap();

        let uuid = "qc-failed-published";
        insert_processing(&pool, uuid, 4343, Some("aa"), "D:/w/src.mxf", "QC fail")
            .await
            .unwrap();
        mark_ready(
            &pool,
            uuid,
            &media_file.to_string_lossy(),
            10000,
            false,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &["duration_delta_exceeded".to_string()],
            "[0]",
            None,
        )
        .await
        .unwrap();
        assert_eq!(find_by_uuid(&pool, uuid).await.unwrap().unwrap().status, "error");

        let result = purge_single_asset_with_context(
            &pool,
            uuid,
            PurgeMode::DeleteMediaIfUnreferenced,
            Some(&target),
            Some(&watch),
        )
        .await
        .unwrap();

        assert_eq!(result.rows_deleted, 1);
        assert!(result.media_removed, "warnings: {:?}", result.warnings);
        assert!(result.sidecar_removed);
        assert!(!media_file.exists(), "a QC-failed mezzanine must not outlive its row");
        assert!(!sidecar_file.exists());

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
            None,
        )
        .await
        .unwrap();

        // Purge asset
        let result = purge_single_asset_with_context(
            &pool,
            uuid,
            PurgeMode::DeleteMediaIfUnreferenced,
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
            PurgeMode::DeleteMediaIfUnreferenced,
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


    /// SB-01: filtering, searching and paging moved from Rust into SQL. This
    /// runs the old in-memory implementation and the new query over the same
    /// fixture and asserts they select the same uuids, in the same order, for
    /// every filter and for searches containing LIKE metacharacters.
    #[tokio::test]
    async fn sql_filtering_selects_exactly_what_the_in_memory_filter_did() {
        let (pool, temp_dir) = setup_test_pool().await;

        // Names chosen to exercise the subclip heuristics and the LIKE
        // metacharacters an operator can type into the search box.
        let fixture: &[(&str, &str, &str)] = &[
            ("a-plain", "Evening News", "D:/target/news.mp4"),
            ("a-subclip-name", "Promo subclip 2", "D:/target/promo.mp4"),
            ("a-sub-clip-name", "Trailer SUB-CLIP", "D:/target/trailer.mp4"),
            ("a-percent", "100% Crete", "D:/target/100%25.mp4"),
            ("a-underscore", "wild_card", "D:/target/wild_card.mp4"),
            ("a-wildish", "wildXcard", "D:/target/wildXcard.mp4"),
            ("a-folderish", "Doc", "D:/target/docs/doc.mp4"),
        ];
        for (uuid, name, path) in fixture {
            insert_processing(&pool, uuid, 1, None, path, name)
                .await
                .unwrap();
        }
        // A mix of statuses and one trashed row.
        mark_ready(
            &pool, "a-plain", "D:/target/news.mp4", 10_000, true, 25.0, 25, 1, 250, 50, 0, &[], "[]",
            None,
        )
        .await
        .unwrap();
        mark_ready(
            &pool,
            "a-percent",
            "D:/target/100%25.mp4",
            10_000,
            true,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &[],
            "[]",
            None,
        )
        .await
        .unwrap();
        mark_error(&pool, "a-underscore").await.unwrap();
        trash_asset(&pool, "a-folderish").await.unwrap();
        // A real trim window, so `is_subclip` is true for a reason other than
        // the display name.
        sqlx::query("UPDATE media_assets SET trim_in_ms = 500 WHERE uuid = 'a-wildish'")
            .execute(&pool)
            .await
            .unwrap();

        /// The filter exactly as it was written before this change.
        fn reference(
            all: &[MediaAsset],
            filter_mode: &str,
            search_term: &str,
        ) -> Vec<String> {
            let mut rows: Vec<&MediaAsset> = all.iter().collect();
            rows.sort_by(|a, b| {
                let ka = a.deleted_at.clone().unwrap_or_else(|| "9999".to_string());
                let kb = b.deleted_at.clone().unwrap_or_else(|| "9999".to_string());
                ka.cmp(&kb)
                    .then(a.display_name.cmp(&b.display_name))
                    .then(a.uuid.cmp(&b.uuid))
            });
            rows.into_iter()
                .map(|a| DbAssetSummary::from_asset_row(a.clone()))
                .filter(|a| {
                    let matches_filter = match filter_mode {
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
                    if search_term.is_empty() {
                        return true;
                    }
                    a.display_name.to_ascii_lowercase().contains(search_term)
                        || a.uuid.to_ascii_lowercase().contains(search_term)
                        || a.virtual_folder.to_ascii_lowercase().contains(search_term)
                        || a.current_path.to_ascii_lowercase().contains(search_term)
                })
                .map(|a| a.uuid)
                .collect()
        }

        let all: Vec<MediaAsset> = sqlx::query_as(&format!(
            "SELECT {} FROM media_assets",
            SELECT_COLS
        ))
        .fetch_all(&pool)
        .await
        .unwrap();

        for filter in [
            "all",
            "master",
            "subclip",
            "ready",
            "processing",
            "error",
            "trashed",
        ] {
            for search in ["", "wild", "%", "_", "100%", "wild_card", "TARGET", "nope"] {
                let expected = reference(&all, filter, &search.to_ascii_lowercase());
                let page = query_db_assets(&pool, Some(filter), Some(search), Some(100), Some(0))
                    .await
                    .unwrap();
                let got: Vec<String> = page.items.iter().map(|i| i.uuid.clone()).collect();
                assert_eq!(
                    got, expected,
                    "filter={:?} search={:?} diverged",
                    filter, search
                );
                assert_eq!(page.total, expected.len() as i64, "total for {:?}", filter);
            }
        }

        // A `_` stays a literal: it must not match `wildXcard` the way an
        // unescaped LIKE wildcard would (the F-05 class of bug).
        let page = query_db_assets(&pool, None, Some("wild_card"), Some(100), Some(0))
            .await
            .unwrap();
        let got: Vec<String> = page.items.iter().map(|i| i.uuid.clone()).collect();
        assert_eq!(got, vec!["a-underscore".to_string()]);

        // Paging is done in SQL now; the pages must still tile the filter.
        let first = query_db_assets(&pool, Some("all"), None, Some(3), Some(0))
            .await
            .unwrap();
        let second = query_db_assets(&pool, Some("all"), None, Some(3), Some(3))
            .await
            .unwrap();
        assert_eq!(first.total, 7);
        assert_eq!(second.total, 7);
        assert_eq!(first.items.len(), 3);
        let full = query_db_assets(&pool, Some("all"), None, Some(100), Some(0))
            .await
            .unwrap();
        let tiled: Vec<String> = first
            .items
            .iter()
            .chain(second.items.iter())
            .map(|i| i.uuid.clone())
            .collect();
        let expected: Vec<String> = full.items[..6].iter().map(|i| i.uuid.clone()).collect();
        assert_eq!(tiled, expected);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_db_viewer_overview_and_assets() {
        let (pool, temp_dir) = setup_test_pool().await;

        // 1. Insert master clip
        insert_processing(&pool, "asset-master", 1001, None, "D:/target/master.mp4", "Master 1").await.unwrap();
        mark_ready(&pool, "asset-master", "D:/target/master.mp4", 10000, true, 25.0, 25, 1, 250, 50, 0, &["warning1".into()], "[]", None).await.unwrap();

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

    /// The second re-encode loop, and the one that survived every other fix.
    ///
    /// Media that cannot be ingested at all -- a codec ffmpeg will not take, a
    /// file ffprobe rejects -- fails with no QC findings, so the QC rule does
    /// not cover it. And the startup sweep *deletes* its row so the watcher
    /// re-offers the file, which means a full encode attempt is spent on it
    /// every single restart. Three such files sit in this station's watch
    /// folder today.
    #[tokio::test]
    async fn media_that_cannot_be_ingested_is_not_retried_on_every_restart() {
        let (pool, dir) = setup_test_pool().await;
        let watch = dir.join("watch");
        std::fs::create_dir_all(&watch).unwrap();
        let source = watch.join("unsupported.mxf");
        std::fs::File::create(&source).unwrap();
        let path = source.to_string_lossy().to_string();

        // The shape those three rows have: error, never published, source still
        // sitting in the watch folder.
        insert_processing(&pool, "bad", 910_010, Some("sha-bad"), &path, "Bad")
            .await
            .unwrap();
        mark_error_permanent(&pool, "bad", "key-now").await.unwrap();

        let key_for = |sha: &str| {
            if sha == "sha-bad" {
                "key-now".to_string()
            } else {
                format!("key-{}", sha)
            }
        };

        // Restart: the sweep leaves it alone instead of handing it back.
        let out = recover_failed_assets(&pool, &watch, true, key_for)
            .await
            .unwrap();
        assert_eq!(out.kept_permanent, 1);
        assert_eq!(out.purged_for_retry, 0, "it must not be queued again");
        assert!(find_by_uuid(&pool, "bad").await.unwrap().is_some());

        // ...and again, and again.
        let again = recover_failed_assets(&pool, &watch, true, key_for)
            .await
            .unwrap();
        assert_eq!(again.kept_permanent, 1);
        assert_eq!(again.purged_for_retry, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same sweep must still retry everything it used to. A permanent
    /// verdict is scoped to the bytes and the settings that produced it.
    #[tokio::test]
    async fn a_changed_file_or_changed_settings_is_still_retried() {
        let (pool, dir) = setup_test_pool().await;
        let watch = dir.join("watch");
        std::fs::create_dir_all(&watch).unwrap();

        for (uuid, sha, name) in [
            ("settings-moved", "sha-a", "a.mxf"),
            ("transient", "sha-b", "b.mxf"),
        ] {
            let source = watch.join(name);
            std::fs::File::create(&source).unwrap();
            insert_processing(
                &pool,
                uuid,
                910_011,
                Some(sha),
                &source.to_string_lossy(),
                uuid,
            )
            .await
            .unwrap();
        }
        // One was judged permanent under settings that have since changed...
        mark_error_permanent(&pool, "settings-moved", "key-old")
            .await
            .unwrap();
        // ...the other failed transiently and carries no verdict at all.
        mark_error(&pool, "transient").await.unwrap();

        let out = recover_failed_assets(&pool, &watch, true, |_| "key-new".to_string())
            .await
            .unwrap();
        assert_eq!(out.kept_permanent, 0);
        assert_eq!(
            out.purged_for_retry, 2,
            "a settings change, and an unjudged failure, both get another go"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-5 again. The demoted rows land in `error`, which the startup recovery
    /// sweep also walks -- and that sweep deletes. A published mezzanine is not
    /// the debris of a failed ingest, whatever its status says.
    #[tokio::test]
    async fn the_recovery_sweep_does_not_delete_a_published_qc_failure() {
        let (pool, dir) = setup_test_pool().await;
        let watch = dir.join("watch");
        std::fs::create_dir_all(&watch).unwrap();

        // A QC-failed mezzanine whose file has since been deleted by hand.
        insert_processing(&pool, "qc", 910_001, Some("aa"), "D:/w/a.ts", "A")
            .await
            .unwrap();
        mark_ready(
            &pool,
            "qc",
            "D:/media/gone.mp4",
            40_000,
            false,
            25.0,
            25,
            1,
            1000,
            50,
            0,
            &["duration_delta_exceeded".to_string()],
            "[0,2000]",
            None,
        )
        .await
        .unwrap();

        // Genuine debris: never published, source gone.
        insert_processing(&pool, "debris", 910_002, Some("bb"), "D:/w/vanished.ts", "B")
            .await
            .unwrap();
        mark_error(&pool, "debris").await.unwrap();

        let out = recover_failed_assets(&pool, &watch, true, |_| "unused".to_string())
            .await
            .unwrap();
        assert_eq!(out.purged_dead, 1, "the debris goes");
        assert_eq!(out.kept_dead, 1, "the published failure stays");
        assert!(find_by_uuid(&pool, "qc").await.unwrap().is_some());
        assert!(find_by_uuid(&pool, "debris").await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opening the registry must not change it. The ownership guard runs after
    /// the pool is open -- it needs this registry's identity, which lives in the
    /// registry -- so a start that is about to be refused opens the database
    /// first. It must find it exactly as it left it.
    #[tokio::test]
    async fn opening_the_registry_does_not_rewrite_any_rows() {
        let (pool, dir) = setup_test_pool().await;
        let db_path = dir.join("test.db");

        // A row in the state the migration exists to correct, written past the
        // trigger the way a previous version would have left it on disk.
        insert_processing(&pool, "legacy", 920_001, Some("aa"), "D:/w/a.ts", "A")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE media_assets SET mezzanine_ok = 0, duration_ms = 40000 WHERE uuid = 'legacy'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DROP TRIGGER trg_media_assets_ready_requires_mezzanine_update")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE media_assets SET status = 'ready' WHERE uuid = 'legacy'")
            .execute(&pool)
            .await
            .unwrap();
        drop(pool);

        // Re-open: additive only. The bad row is still exactly as it was.
        let pool = init_pool(&db_path).await.unwrap();
        assert_eq!(
            find_by_uuid(&pool, "legacy").await.unwrap().unwrap().status,
            "ready",
            "opening the registry must not rewrite rows -- the start may yet be refused"
        );

        // And the data migration, run once ownership is established, corrects it.
        run_data_migrations(&pool, &db_path).await.unwrap();
        assert_eq!(
            find_by_uuid(&pool, "legacy").await.unwrap().unwrap().status,
            "error"
        );

        // Idempotent.
        run_data_migrations(&pool, &db_path).await.unwrap();
        assert_eq!(
            find_by_uuid(&pool, "legacy").await.unwrap().unwrap().status,
            "error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// W-1. A sub-clip cut from a QC-failed parent before T-5 inherited
    /// `ready / mezzanine_ok = 0`. With the triggers installed, adopting it onto
    /// `parent_uuid` before demoting it aborted the migration -- on every
    /// start, because the demote never got to run.
    #[tokio::test]
    async fn subclip_of_a_qc_failed_parent_does_not_block_the_migration() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = init_schema(pool).await.unwrap();

        // The registry as a pre-T-5 version left it on disk: written past the
        // triggers, which the new version then installs on open.
        for t in [
            "trg_media_assets_ready_requires_mezzanine_insert",
            "trg_media_assets_ready_requires_mezzanine_update",
        ] {
            sqlx::query(&format!("DROP TRIGGER {t}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO media_assets
               (uuid, fingerprint, source_sha256, current_path, duration_ms,
                trim_in_ms, trim_out_ms, status, mezzanine_ok)
             VALUES
               ('parent', 1, 'aa', 'D:/media/a.mp4', 60000, 0, 60000, 'ready', 0),
               ('sub',    1, NULL, 'D:/media/a.mp4', 60000, 10117, 23814, 'ready', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let pool = init_schema(pool).await.unwrap();

        // A path whose backup folder is scratch: the pre-migration snapshot fires.
        let dir = std::env::temp_dir().join(format!("pt_test_w1_{}", uuid::Uuid::new_v4()));
        run_data_migrations(&pool, &dir.join("test.db"))
            .await
            .expect("the migration must not trip the ready => mezzanine_ok trigger");

        let sub = find_by_uuid(&pool, "sub").await.unwrap().unwrap();
        assert_eq!(sub.status, "error");
        assert_eq!(sub.parent_uuid.as_deref(), Some("parent"), "and it is still adopted");
        assert_eq!(find_by_uuid(&pool, "parent").await.unwrap().unwrap().status, "error");

        // Idempotent: the next start is a no-op, not a second failure.
        run_data_migrations(&pool, &dir.join("test.db")).await.unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-5. The contradiction that had the same asset red in the library and
    /// green in the rundown can no longer be written, by anyone.
    #[tokio::test]
    async fn ready_with_a_failed_mezzanine_cannot_be_written() {
        let (pool, dir) = setup_test_pool().await;

        insert_processing(&pool, "qc-fail", 900_001, Some("aa"), "D:/w/a.mxf", "A")
            .await
            .unwrap();

        // The publish path: a failed QC lands as `error`, keeping every scrap
        // of metadata that says why.
        mark_ready(
            &pool,
            "qc-fail",
            "D:/media/a.mp4",
            84_520,
            false,
            25.0,
            25,
            1,
            2113,
            50,
            0,
            &["duration_delta_exceeded".to_string()],
            "[0,2000,4000]",
            None,
        )
        .await
        .unwrap();

        let a = find_by_uuid(&pool, "qc-fail").await.unwrap().unwrap();
        assert_eq!(a.status, "error", "a QC-failed mezzanine is not `ready`");
        assert!(!a.mezzanine_ok);
        assert_eq!(a.duration_ms, 84_520, "but it keeps its real metadata");
        assert_eq!(a.keyframe_offsets_json, "[0,2000,4000]");

        // And the back door is shut too: no hand-run UPDATE, no future writer,
        // no restore path can put the row back into the impossible state.
        let forced = sqlx::query("UPDATE media_assets SET status = 'ready' WHERE uuid = ?1")
            .bind("qc-fail")
            .execute(&pool)
            .await;
        assert!(
            forced.is_err(),
            "the schema must refuse `ready` on a failed mezzanine, not just the publish path"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-5 again, from the other side: a passing encode is still `ready`.
    #[tokio::test]
    async fn a_passing_mezzanine_is_still_published_as_ready() {
        let (pool, dir) = setup_test_pool().await;
        insert_processing(&pool, "good", 900_002, Some("bb"), "D:/w/b.mxf", "B")
            .await
            .unwrap();
        mark_ready(
            &pool, "good", "D:/media/b.mp4", 1_000, true, 25.0, 25, 1, 25, 50, 0, &[], "[0]",
            None,
        )
        .await
        .unwrap();
        let a = find_by_uuid(&pool, "good").await.unwrap().unwrap();
        assert_eq!(a.status, "ready");
        assert_eq!(a.trim_in_ms, 0);
        assert_eq!(a.trim_out_ms, 1_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-2b. The loop an operator started by cutting a sub-clip: the sub-clip
    /// carries its parent's fingerprint and path with a NULL `source_sha256`,
    /// so the dedupe read it as an unconfirmable legacy row and re-ingested the
    /// programme it was cut from.
    #[tokio::test]
    async fn a_subclip_is_never_the_dedupe_candidate_for_its_parents_source() {
        let (pool, dir) = setup_test_pool().await;
        let fp = 900_003;

        insert_processing(&pool, "parent", fp, Some("cafe"), "D:/w/p.mxf", "Parent")
            .await
            .unwrap();
        mark_ready(
            &pool,
            "parent",
            "D:/media/p.mp4",
            84_520,
            true,
            25.0,
            25,
            1,
            2113,
            50,
            0,
            &[],
            "[0,2000,4000]",
            None,
        )
        .await
        .unwrap();

        create_subclip(
            &pool,
            "child",
            "parent",
            "Parent (Sub-clip)",
            4_000,
            8_000,
            true,
            "[]",
        )
        .await
        .unwrap()
        .expect("the sub-clip must be created");

        let child = find_by_uuid(&pool, "child").await.unwrap().unwrap();
        assert_eq!(child.parent_uuid.as_deref(), Some("parent"));
        assert_eq!(
            child.fingerprint, fp,
            "it does share the parent's fingerprint"
        );
        assert!(
            child.source_sha256.is_none(),
            "and has no source hash of its own"
        );

        // Which is exactly why the lookup has to exclude it. `rowid DESC` alone
        // would have *preferred* it -- sub-clips are created after parents.
        let candidate = find_by_fingerprint(&pool, fp).await.unwrap().unwrap();
        assert_eq!(
            candidate.uuid, "parent",
            "the dedupe candidate must be the full-length asset, never a sub-clip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Publish a QC failure: `mark_ready` with `mezzanine_ok = false`, which
    /// lands the row at `error` (T-5) carrying the findings that say why.
    async fn failed_row(
        pool: &SqlitePool,
        uuid: &str,
        fingerprint: i64,
        sha: &str,
        findings: &[&str],
        verdict_key: Option<&str>,
    ) {
        insert_processing(pool, uuid, fingerprint, Some(sha), "D:/w/x.ts", uuid)
            .await
            .unwrap();
        let findings: Vec<String> = findings.iter().map(|s| s.to_string()).collect();
        mark_ready(
            pool,
            uuid,
            &format!("D:/media/{}.mp4", uuid),
            40_000,
            false,
            25.0,
            25,
            1,
            1000,
            50,
            0,
            &findings,
            "[0,2000]",
            verdict_key,
        )
        .await
        .unwrap();
    }

    /// T-2. Eleven rows for three sources, three more on every restart: the
    /// dedupe refused to recognise a QC-failed row as a duplicate, re-encoded
    /// it, reached the identical verdict, and wrote another row.
    ///
    /// The verdict is keyed on the bytes **and** the settings, so it is a
    /// re-checkable fact rather than a blacklist.
    #[tokio::test]
    async fn known_bad_media_is_not_re_encoded_under_the_same_settings() {
        let (pool, dir) = setup_test_pool().await;

        failed_row(
            &pool,
            "bad",
            900_004,
            "beef",
            &["duration_delta_exceeded"],
            Some("key-A"),
        )
        .await;

        // Same media, same settings -> the same verdict. Do not encode it again.
        let hit = find_reproducible_qc_failure(&pool, "key-A", "beef")
            .await
            .unwrap()
            .expect("known-bad media under unchanged settings must be recognised");
        assert_eq!(hit.uuid, "bad");

        // Different media entirely: neither the verdict key nor the bytes
        // match, so there is nothing known about it and it gets encoded.
        assert!(
            find_reproducible_qc_failure(&pool, "key-of-other-media", "f00d")
                .await
                .unwrap()
                .is_none(),
            "an unrelated source must not be caught by someone else's verdict"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of keying on the settings. An operator who widens
    /// `max_duration_delta_ms` is doing it *precisely* to accept the files that
    /// keep failing on it; finding them still skipped would be the worse bug.
    #[tokio::test]
    async fn changing_the_settings_gives_failed_media_another_hearing() {
        let (pool, dir) = setup_test_pool().await;

        failed_row(
            &pool,
            "bad",
            900_020,
            "beef",
            &["duration_delta_exceeded"],
            Some("settings-before"),
        )
        .await;

        assert!(
            find_reproducible_qc_failure(&pool, "settings-before", "beef")
                .await
                .unwrap()
                .is_some(),
            "unchanged settings: skip"
        );
        assert!(
            find_reproducible_qc_failure(&pool, "settings-after", "beef")
                .await
                .unwrap()
                .is_none(),
            "the settings that reached this verdict have changed, so it must be re-examined"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failure that says more about the machine than the media stays
    /// retryable. Blacklisting a good master because ffprobe was mid-upgrade is
    /// not a trade worth making.
    #[tokio::test]
    async fn an_environmental_failure_is_never_permanent() {
        let (pool, dir) = setup_test_pool().await;

        failed_row(
            &pool,
            "toolchain",
            900_021,
            "aaaa",
            &["keyframe_scan_failed"],
            Some("key-E"),
        )
        .await;
        assert!(
            find_reproducible_qc_failure(&pool, "key-E", "aaaa")
                .await
                .unwrap()
                .is_none(),
            "ffprobe failing to run is not evidence about the programme"
        );

        // But the same row alongside a real finding is still bad media.
        failed_row(
            &pool,
            "both",
            900_022,
            "bbbb",
            &["keyframe_scan_failed", "duration_delta_exceeded"],
            Some("key-B"),
        )
        .await;
        assert!(
            find_reproducible_qc_failure(&pool, "key-B", "bbbb")
                .await
                .unwrap()
                .is_some(),
            "one reproducible finding is enough"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rows published before `qc_verdict_key` existed -- the eleven already in
    /// the registry -- have no record of the settings that judged them, so the
    /// much narrower source-attributable test applies to them instead.
    #[tokio::test]
    async fn legacy_rows_fall_back_to_the_narrow_test_and_then_converge() {
        let (pool, dir) = setup_test_pool().await;

        failed_row(&pool, "legacy", 900_023, "cccc", &["duration_delta_exceeded"], None).await;
        failed_row(&pool, "unclear", 900_024, "dddd", &["missing_faststart"], None).await;

        let hit = find_reproducible_qc_failure(&pool, "any-key", "cccc")
            .await
            .unwrap()
            .expect("a legacy duration-delta failure is still known-bad media");
        assert_eq!(hit.uuid, "legacy");
        assert!(hit.qc_verdict_key.is_none());

        assert!(
            find_reproducible_qc_failure(&pool, "any-key", "dddd")
                .await
                .unwrap()
                .is_none(),
            "without a record of the settings, only an unmistakable finding counts"
        );

        // Stamping the key converges the legacy row onto the precise test, so a
        // later settings change releases it too.
        adopt_qc_verdict_key(&pool, "legacy", "key-now").await.unwrap();
        assert!(find_reproducible_qc_failure(&pool, "key-now", "cccc")
            .await
            .unwrap()
            .is_some());
        assert!(
            find_reproducible_qc_failure(&pool, "key-later", "cccc")
                .await
                .unwrap()
                .is_none(),
            "once stamped, a settings change releases a legacy row as well"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The operator's override must actually release the media.
    ///
    /// Clearing the key to NULL looked right and did nothing: a NULL key drops
    /// the row into the *legacy* branch, which matches on the source hash alone
    /// and catches exactly the `duration_delta_exceeded` rows an operator is
    /// most likely to be overruling. The sentinel keeps it out of both branches.
    #[tokio::test]
    async fn overruling_a_verdict_really_does_release_the_media() {
        let (pool, dir) = setup_test_pool().await;
        failed_row(&pool, "held", 900_030, "beef", &["duration_delta_exceeded"], Some("key-A")).await;

        assert!(find_reproducible_qc_failure(&pool, "key-A", "beef")
            .await
            .unwrap()
            .is_some());

        assert!(clear_qc_verdict(&pool, "held").await.unwrap());
        assert!(
            find_reproducible_qc_failure(&pool, "key-A", "beef")
                .await
                .unwrap()
                .is_none(),
            "the row must not be caught by the verdict branch..."
        );
        assert!(
            find_reproducible_qc_failure(&pool, "any-other-key", "beef")
                .await
                .unwrap()
                .is_none(),
            "...nor fall through into the legacy source-hash branch"
        );

        // Idempotent, and honest about it.
        assert!(!clear_qc_verdict(&pool, "held").await.unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `retry_suppressed` on the wire must mean what it says: that the service
    /// will actually skip this media. It is what the UI badge renders, and a
    /// badge that says "won't retry" about media the service happily retries is
    /// worse than no badge.
    #[test]
    fn retry_suppressed_matches_what_the_service_actually_does() {
        // No verdict at all: nothing is being skipped.
        assert!(!is_retry_suppressed(None));
        // A live verdict: skipped.
        assert!(is_retry_suppressed(Some("a4f9...")));
        // An operator's override: released, and the badge must go with it.
        assert!(!is_retry_suppressed(Some(QC_VERDICT_CLEARED)));
    }

    /// A row with no recorded findings is not evidence of anything. Those are
    /// the failed *jobs* -- an encode that never reached QC -- and the retry
    /// classifier owns them.
    #[tokio::test]
    async fn a_failure_with_no_findings_is_not_treated_as_bad_media() {
        let (pool, dir) = setup_test_pool().await;
        failed_row(&pool, "nofindings", 900_025, "eeee", &[], Some("key-N")).await;
        assert!(find_reproducible_qc_failure(&pool, "key-N", "eeee")
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A passing asset is never a reason to skip anything.
    #[tokio::test]
    async fn a_healthy_asset_is_not_a_reproducible_failure() {
        let (pool, dir) = setup_test_pool().await;
        insert_processing(&pool, "good", 900_026, Some("ffff"), "D:/w/g.mxf", "G")
            .await
            .unwrap();
        mark_ready(
            &pool, "good", "D:/media/g.mp4", 1_000, true, 25.0, 25, 1, 25, 50, 0, &[], "[0]", None,
        )
        .await
        .unwrap();
        assert!(find_reproducible_qc_failure(&pool, "whatever", "ffff")
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-5 + T-2 together. Making a QC-failed row `error` walked it into the
    /// purge's `error` arm, which deletes -- and its mezzanine is a real file
    /// in the Caspar media folder that nothing would then reference.
    #[tokio::test]
    async fn a_published_but_qc_failed_mezzanine_survives_a_re_ingest_sweep() {
        let (pool, dir) = setup_test_pool().await;
        let file = dir.join("qc_failed.mp4");
        std::fs::File::create(&file).unwrap();
        let fp = 900_006;

        insert_processing(&pool, "published", fp, Some("ee"), "D:/w/e.ts", "E")
            .await
            .unwrap();
        mark_ready(
            &pool,
            "published",
            &file.to_string_lossy(),
            40_000,
            false,
            25.0,
            25,
            1,
            1000,
            50,
            0,
            &["duration_delta_exceeded".to_string()],
            "[0,2000]",
            None,
        )
        .await
        .unwrap();

        // The debris of a genuinely failed ingest, by contrast: never
        // published, so no duration, and its `current_path` is still the source.
        insert_processing(&pool, "debris", fp, Some("ee"), "D:/w/e.ts", "E")
            .await
            .unwrap();
        mark_error(&pool, "debris").await.unwrap();

        let outcome =
            purge_unusable_rows_by_fingerprint(&pool, fp, |p| std::path::Path::new(p).exists())
                .await
                .unwrap();
        assert_eq!(
            outcome,
            FingerprintPurge {
                deleted: 1,
                demoted: 0,
                protected: 1
            }
        );
        assert!(
            find_by_uuid(&pool, "published").await.unwrap().is_some(),
            "a published mezzanine is someone's asset even when it is not airable"
        );
        assert!(find_by_uuid(&pool, "debris").await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-4. `ready` must mean the file is there, in both directions.
    #[tokio::test]
    async fn the_reconcile_moves_assets_between_ready_and_missing() {
        let (pool, dir) = setup_test_pool().await;

        insert_processing(&pool, "here", 900_007, Some("11"), "D:/w/f.mxf", "F")
            .await
            .unwrap();
        mark_ready(
            &pool, "here", "D:/media/f.mp4", 1_000, true, 25.0, 25, 1, 25, 50, 0, &[], "[0]",
            None,
        )
        .await
        .unwrap();

        // The share is down.
        let gone = reconcile_missing_paths(&pool, |_| false).await.unwrap();
        assert_eq!(gone.went_missing, 1);
        assert_eq!(gone.missing_total, 1);
        assert_eq!(
            find_by_uuid(&pool, "here").await.unwrap().unwrap().status,
            "missing"
        );

        // A second pass while it is still down must not double-count.
        let still = reconcile_missing_paths(&pool, |_| false).await.unwrap();
        assert_eq!(still.went_missing, 0);
        assert_eq!(still.missing_total, 1);

        // The share is back.
        let back = reconcile_missing_paths(&pool, |_| true).await.unwrap();
        assert_eq!(back.came_back, 1);
        assert_eq!(back.missing_total, 0);
        assert_eq!(
            find_by_uuid(&pool, "here").await.unwrap().unwrap().status,
            "ready"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reconcile must leave the states it does not own alone: an `error`
    /// row's reason is worth more than "the file is not there", and a
    /// `processing` row's `current_path` is still its *source*.
    #[tokio::test]
    async fn the_reconcile_leaves_error_and_processing_rows_alone() {
        let (pool, dir) = setup_test_pool().await;

        insert_processing(&pool, "working", 900_008, Some("22"), "D:/w/g.mxf", "G")
            .await
            .unwrap();
        insert_processing(&pool, "broken", 900_009, Some("33"), "D:/w/h.mxf", "H")
            .await
            .unwrap();
        mark_error(&pool, "broken").await.unwrap();

        let r = reconcile_missing_paths(&pool, |_| false).await.unwrap();
        assert_eq!(r.went_missing, 0);
        assert_eq!(
            find_by_uuid(&pool, "working").await.unwrap().unwrap().status,
            "processing"
        );
        assert_eq!(
            find_by_uuid(&pool, "broken").await.unwrap().unwrap().status,
            "error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-1c. The backfill has to find both shapes of wrong row: the ordinary
    /// one whose safe start is a GOP late, and the T-7 one whose offsets list
    /// is empty and whose safe start is already 0.
    #[tokio::test]
    async fn the_rescan_selects_both_shapes_of_bad_keyframe_row() {
        let (pool, dir) = setup_test_pool().await;

        // Wrong: the keyframe at 0 was dropped.
        insert_processing(&pool, "late", 900_010, Some("44"), "D:/w/i.mxf", "I")
            .await
            .unwrap();
        mark_ready(
            &pool,
            "late",
            "D:/media/i.mp4",
            84_520,
            true,
            25.0,
            25,
            1,
            2113,
            50,
            2000,
            &[],
            "[2000,4000,6000]",
            None,
        )
        .await
        .unwrap();

        // Wrong differently: the scan produced nothing and QC passed anyway.
        insert_processing(&pool, "empty", 900_011, Some("55"), "D:/w/j.mxf", "J")
            .await
            .unwrap();
        mark_ready(
            &pool, "empty", "D:/media/j.mp4", 1_000, true, 25.0, 25, 1, 25, 50, 0, &[], "[]",
            None,
        )
        .await
        .unwrap();

        // Correct, and must be left out of the sweep entirely.
        insert_processing(&pool, "fine", 900_012, Some("66"), "D:/w/k.mxf", "K")
            .await
            .unwrap();
        mark_ready(
            &pool,
            "fine",
            "D:/media/k.mp4",
            4_000,
            true,
            25.0,
            25,
            1,
            100,
            50,
            0,
            &[],
            "[0,2000]",
            None,
        )
        .await
        .unwrap();

        let targets = keyframe_rescan_targets(&pool).await.unwrap();
        let paths: Vec<&str> = targets.iter().map(|t| t.current_path.as_str()).collect();
        assert!(paths.contains(&"D:/media/i.mp4"));
        assert!(paths.contains(&"D:/media/j.mp4"));
        assert!(!paths.contains(&"D:/media/k.mp4"));

        // And a correction reaches every row that plays the file, which is how
        // a sub-clip's copy of its parent's offsets gets fixed with it.
        create_subclip(
            &pool,
            "late-cut",
            "late",
            "I (Sub-clip)",
            4_000,
            8_000,
            true,
            "[]",
        )
        .await
        .unwrap()
        .unwrap();
        let n = apply_keyframe_rescan(&pool, "D:/media/i.mp4", 0, "[0,2000,4000,6000]")
            .await
            .unwrap();
        assert_eq!(n, 2, "the parent and its sub-clip");
        for uuid in ["late", "late-cut"] {
            let a = find_by_uuid(&pool, uuid).await.unwrap().unwrap();
            assert_eq!(a.keyframe_safe_start_ms, 0);
            assert_eq!(a.keyframe_offsets_json, "[0,2000,4000,6000]");
        }

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

        let a = find_by_uuid(&pool, "hashed").await.unwrap().unwrap();
        let b = find_by_uuid(&pool, "legacy").await.unwrap().unwrap();
        assert_eq!(a.source_sha256.as_deref(), Some("deadbeef"));
        assert!(
            b.source_sha256.is_none(),
            "a row with no stored hash must read back as None, which is what makes dedup              refuse to confirm rather than guess"
        );

        // And the dedupe lookup itself now skips the hashless row (T-2b). It
        // could never be confirmed as a duplicate anyway -- there is nothing to
        // compare against -- so returning it only ever cost a re-ingest, and it
        // is the same shape a sub-clip has, which cost a great deal more.
        assert!(find_by_fingerprint(&pool, 1).await.unwrap().is_some());
        assert!(
            find_by_fingerprint(&pool, 2).await.unwrap().is_none(),
            "a row with no source_sha256 is not a dedupe candidate"
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
