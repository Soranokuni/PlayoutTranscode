//! Registry backups (T2-13).
//!
//! The asset registry holds every uuid, virtual folder, rating, trim window and
//! compliance flag an operator has ever set, and none of it can be
//! reconstructed from the media files. Before this there was no copy of it at
//! all: a corrupt file meant re-ingesting the library and re-entering the
//! metadata by hand.

mod common;

use playout_transcode::db;

#[tokio::test]
async fn a_backup_is_a_readable_database_with_the_same_rows() {
    let s = common::spawn_test_server().await;

    db::insert_processing(
        &s.pool,
        "11111111-1111-4111-8111-111111111111",
        42,
        Some("abc"),
        "D:/w/a.mxf",
        "Programme A",
    )
    .await
    .unwrap();

    let dir = s.root.join("backup-target");
    let path = db::backup_now(&s.pool, &dir).await.expect("backup");

    assert!(path.exists(), "the snapshot must actually be on disk");
    assert!(
        std::fs::metadata(&path).unwrap().len() > 0,
        "an empty file is not a backup"
    );

    // The real assertion: it opens as a database and the row survived. A
    // snapshot that cannot be read back is worse than no snapshot, because it
    // looks like protection.
    let restored = db::init_pool(&path).await.expect("the snapshot must open");
    let found = db::find_by_fingerprint(&restored, 42)
        .await
        .unwrap()
        .expect("the asset must be present in the snapshot");
    assert_eq!(found.display_name, "Programme A");
    assert_eq!(found.source_sha256.as_deref(), Some("abc"));
}

#[tokio::test]
async fn a_second_backup_on_the_same_day_replaces_the_first() {
    let s = common::spawn_test_server().await;
    let dir = s.root.join("same-day");

    let first = db::backup_now(&s.pool, &dir).await.unwrap();
    // Dated, not timestamped, so an hourly trigger cannot fill the volume.
    let second = db::backup_now(&s.pool, &dir).await.unwrap();
    assert_eq!(first, second);

    assert_eq!(
        db::list_backups(&dir).len(),
        1,
        "one file per day, not one per call"
    );
}

#[tokio::test]
async fn backups_are_listed_newest_first() {
    let s = common::spawn_test_server().await;
    let dir = s.root.join("listing");
    db::backup_now(&s.pool, &dir).await.unwrap();

    // Two older snapshots, placed by hand since the real ones are date-stamped.
    let bd = db::backup_dir(&dir);
    for date in ["2020-01-01", "2021-06-15"] {
        std::fs::write(bd.join(db::backup_file_name(date)), b"stub").unwrap();
    }

    let listed = db::list_backups(&dir);
    assert_eq!(listed.len(), 3);
    let names: Vec<&str> = listed.iter().map(|b| b.file_name.as_str()).collect();
    assert!(
        names[0] > names[1] && names[1] > names[2],
        "newest first: {:?}",
        names
    );
    assert_eq!(names[2], "media_assets-2020-01-01.db");
}

#[test]
fn pruning_keeps_the_newest_and_removes_the_rest() {
    let dir = std::env::temp_dir().join(format!("pt-prune-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    for day in 1..=10 {
        std::fs::write(
            dir.join(db::backup_file_name(&format!("2026-01-{:02}", day))),
            b"x",
        )
        .unwrap();
    }
    // Something else in the directory, which must survive.
    std::fs::write(dir.join("notes.txt"), b"keep me").unwrap();

    let removed = db::prune_backups(&dir, db::BACKUP_RETENTION);
    assert_eq!(removed, 3, "10 snapshots minus a retention of 7");

    let mut left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();

    assert!(left.contains(&"notes.txt".to_string()), "{:?}", left);
    assert!(
        left.contains(&"media_assets-2026-01-10.db".to_string()),
        "the newest must survive: {:?}",
        left
    );
    assert!(
        !left.contains(&"media_assets-2026-01-01.db".to_string()),
        "the oldest must go: {:?}",
        left
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pruning_a_directory_that_does_not_exist_is_not_an_error() {
    let missing = std::env::temp_dir().join("pt-prune-definitely-absent");
    assert_eq!(db::prune_backups(&missing, 7), 0);
    assert!(db::list_backups(&missing).is_empty());
}

#[tokio::test]
async fn the_backup_endpoint_reports_the_file_it_wrote() {
    let s = common::spawn_test_server().await;

    let r = s.post_json("/api/db/backup", serde_json::json!({})).await;
    assert_eq!(
        r.status(),
        200,
        "taking a backup must not need X-Confirm-Destructive: it creates a \
         file and deletes nothing an operator cares about"
    );

    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["success"], true);
    let name = body["file_name"].as_str().expect("file_name");
    assert!(name.starts_with("media_assets-") && name.ends_with(".db"), "{}", name);
    assert!(body["size_bytes"].as_u64().unwrap() > 0);

    // Never a filesystem path in a response body (T1-4).
    let raw = body.to_string();
    assert!(!raw.contains('\\') && !raw.contains(":/"), "leaked a path: {}", raw);
}

#[tokio::test]
async fn the_db_overview_lists_backups() {
    let s = common::spawn_test_server().await;
    s.post_json("/api/db/backup", serde_json::json!({})).await;

    let overview = s.get_json("/api/db/overview").await;
    let backups = overview["backups"]
        .as_array()
        .expect("db overview must carry the backup list");
    assert!(
        !backups.is_empty(),
        "an operator asking when this was last backed up has nowhere else to look"
    );
    assert!(backups[0]["file_name"].is_string());
    assert!(backups[0]["size_bytes"].is_number());
}
