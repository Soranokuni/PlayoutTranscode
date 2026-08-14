use sqlx::sqlite::SqlitePoolOptions;
use std::path::PathBuf;

#[tokio::test]
async fn test_chaos_duplicate_enqueue_prevention() {
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .expect("in-memory db failed");

    // Initialize V2 tables
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transcode_jobs (
            id TEXT PRIMARY KEY,
            input_path TEXT NOT NULL,
            output_path TEXT,
            profile TEXT NOT NULL,
            phase TEXT NOT NULL,
            state TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 1,
            max_attempts INTEGER NOT NULL DEFAULT 3,
            error TEXT,
            error_category TEXT,
            request_hash TEXT,
            worker_id TEXT,
            heartbeat_at DATETIME,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_transcode_jobs_req_active 
        ON transcode_jobs(request_hash) WHERE state IN ('Pending', 'Processing');",
    )
    .execute(&pool)
    .await
    .unwrap();

    let req_hash = "deadbeef12345678";

    // First insert succeeds
    let res1 = sqlx::query(
        "INSERT INTO transcode_jobs (id, input_path, profile, phase, state, request_hash)
         VALUES ('job-1', 'D:/media/clip.mp4', 'playoutvue-h264-1080p25', 'queued', 'Pending', ?)",
    )
    .bind(req_hash)
    .execute(&pool)
    .await;
    assert!(res1.is_ok());

    // Duplicate active insert fails with constraint violation
    let res2 = sqlx::query(
        "INSERT INTO transcode_jobs (id, input_path, profile, phase, state, request_hash)
         VALUES ('job-2', 'D:/media/clip.mp4', 'playoutvue-h264-1080p25', 'queued', 'Pending', ?)",
    )
    .bind(req_hash)
    .execute(&pool)
    .await;
    assert!(
        res2.is_err(),
        "Duplicate active request_hash must be rejected"
    );

    // Complete job 1
    sqlx::query(
        "UPDATE transcode_jobs SET state = 'Completed', phase = 'completed' WHERE id = 'job-1'",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Now a new job with the same hash can be enqueued (since previous is completed)
    let res3 = sqlx::query(
        "INSERT INTO transcode_jobs (id, input_path, profile, phase, state, request_hash)
         VALUES ('job-3', 'D:/media/clip.mp4', 'playoutvue-h264-1080p25', 'queued', 'Pending', ?)",
    )
    .bind(req_hash)
    .execute(&pool)
    .await;
    assert!(
        res3.is_ok(),
        "Completed job allows new transcode for updated file"
    );
}

#[tokio::test]
async fn test_chaos_stale_lease_recovery_under_crash() {
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .expect("in-memory db failed");

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transcode_jobs (
            id TEXT PRIMARY KEY,
            input_path TEXT NOT NULL,
            output_path TEXT,
            profile TEXT NOT NULL,
            phase TEXT NOT NULL,
            state TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 1,
            max_attempts INTEGER NOT NULL DEFAULT 3,
            error TEXT,
            error_category TEXT,
            request_hash TEXT,
            worker_id TEXT,
            heartbeat_at DATETIME,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Simulate worker crashing during encoding with a stale heartbeat
    sqlx::query(
        "INSERT INTO transcode_jobs (id, input_path, profile, phase, state, attempt, max_attempts, worker_id, heartbeat_at)
         VALUES ('crashed-job-1', 'D:/media/video.mp4', 'playoutvue-h264-1080p25', 'encoding', 'Processing', 1, 3, 'worker-dead', datetime('now', '-300 seconds'))"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Run recovery query (evicts leases older than 60s)
    let updated = sqlx::query(
        "UPDATE transcode_jobs 
         SET phase = 'queued', state = 'Pending', worker_id = NULL, error = 'Recovered from worker crash', attempt = attempt + 1
         WHERE state = 'Processing' AND heartbeat_at < datetime('now', '-60 seconds')"
    )
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(updated.rows_affected(), 1);

    // Verify job was re-queued with incremented attempt
    let (state, phase, attempt): (String, String, i64) = sqlx::query_as(
        "SELECT state, phase, attempt FROM transcode_jobs WHERE id = 'crashed-job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(state, "Pending");
    assert_eq!(phase, "queued");
    assert_eq!(attempt, 2);
}

#[test]
fn test_chaos_orphan_staging_file_cleanup() {
    let temp_dir = std::env::temp_dir().join(format!("pt_chaos_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let stale_tmp = temp_dir.join(".tmp_12345_clip.mp4");
    let valid_media = temp_dir.join("finished_clip.mp4");
    let sidecar_tmp = temp_dir.join(".tmp_12345_clip.mp4.tmp_json");

    std::fs::write(&stale_tmp, b"stale data").unwrap();
    std::fs::write(&valid_media, b"valid published media").unwrap();
    std::fs::write(&sidecar_tmp, b"{}").unwrap();

    // 0 max_age_secs cleans up all .tmp_* files immediately
    let cleaned = cleanup_files(&temp_dir, 0);
    assert_eq!(cleaned, 2);
    assert!(!stale_tmp.exists());
    assert!(!sidecar_tmp.exists());
    assert!(
        valid_media.exists(),
        "Valid published files must NEVER be cleaned"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

fn cleanup_files(target_dir: &PathBuf, max_age_secs: u64) -> usize {
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(target_dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                if filename.starts_with(".tmp_") || filename.ends_with(".tmp_json") {
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(age) = now.duration_since(modified) {
                                if age.as_secs() >= max_age_secs
                                    && std::fs::remove_file(&path).is_ok()
                                {
                                    removed += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    removed
}

#[tokio::test]
async fn test_chaos_strict_concurrency_bounding() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    let max_concurrency = 4;
    let sem = Arc::new(Semaphore::new(max_concurrency));
    let current_active = Arc::new(AtomicUsize::new(0));
    let peak_active = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..20 {
        let s = sem.clone();
        let cur = current_active.clone();
        let peak = peak_active.clone();

        let handle = tokio::spawn(async move {
            let permit = s.acquire_owned().await.unwrap();
            let _held_permit = permit;

            let count = cur.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);

            // Simulate work
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;

            cur.fetch_sub(1, Ordering::SeqCst);
        });
        handles.push(handle);
    }

    for h in handles {
        h.await.unwrap();
    }

    let final_peak = peak_active.load(Ordering::SeqCst);
    assert!(
        final_peak <= max_concurrency,
        "Peak concurrent tasks ({}) exceeded max_concurrency ({})",
        final_peak,
        max_concurrency
    );
    assert_eq!(final_peak, max_concurrency);
    assert_eq!(sem.available_permits(), max_concurrency);
}

#[test]
fn test_chaos_windows_priority_flags() {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
    let combined = CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS;

    assert_eq!(combined, 0x08004000);
    assert_eq!(combined & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
    assert_eq!(combined & BELOW_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS);
}

