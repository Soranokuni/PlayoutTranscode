//! The operator's housekeeping surface: dismissing job records, removing and
//! deleting assets, and overriding a recorded QC verdict.
//!
//! The distinction these tests defend is the one an operator actually cares
//! about — what survives each kind of "delete":
//!
//! | Action                  | job record | registry row | media file |
//! |-------------------------|------------|--------------|------------|
//! | dismiss job             | gone       | kept         | kept       |
//! | remove from library     | kept       | recycle bin  | kept       |
//! | delete, keep media      | kept       | gone         | kept       |
//! | delete, and the media   | kept       | gone         | gone       |

mod common;

use common::spawn_test_server;
use playout_transcode::db;

/// A published, airable asset with its mezzanine on disk.
async fn ready_asset(s: &common::TestServer, uuid: &str, sha: &str, file: &std::path::Path) {
    std::fs::write(file, b"not really an mp4, but it is a file").unwrap();
    db::insert_processing(&s.pool, uuid, 4242, Some(sha), "D:/w/src.mxf", uuid)
        .await
        .unwrap();
    db::mark_ready(
        &s.pool,
        uuid,
        &file.to_string_lossy(),
        1_000,
        true,
        25.0,
        25,
        1,
        25,
        50,
        0,
        &[],
        "[0]",
        None,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn dismissing_a_job_clears_the_record_and_nothing_else() {
    let s = spawn_test_server().await;

    // A finished job the operator wants off their screen.
    let mut job = playout_transcode::jobs::JobRecord::new("D:/w/bad.mxf", "ProfileA");
    let _ = job.transition_to(playout_transcode::jobs::JobPhase::Probing, None);
    let _ = job.transition_to(playout_transcode::jobs::JobPhase::Failed, None);
    let id = job.id.clone();
    s.jobs.push(job);

    let r = s
        .client()
        .delete(s.url(&format!("/api/jobs/{}", id)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "no confirmation header: an × must just work");

    let listed: Vec<serde_json::Value> = s
        .client()
        .get(s.url("/api/jobs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !listed.iter().any(|j| j["id"] == id.as_str()),
        "the record is gone from the queue"
    );

    // Gone for good: it must not come back from the durable table on restart.
    let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM transcode_jobs WHERE id = ?1")
        .bind(&id)
        .fetch_one(&*s.pool)
        .await
        .unwrap();
    assert_eq!(rows.0, 0);

    // And a second dismiss of the same id is a clean 404, not a 500.
    let again = s
        .client()
        .delete(s.url(&format!("/api/jobs/{}", id)))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
}

/// Dismissing a running job would leave an encoder with nothing tracking it.
#[tokio::test]
async fn a_running_job_cannot_be_dismissed_over_the_wire() {
    let s = spawn_test_server().await;

    let mut job = playout_transcode::jobs::JobRecord::new("D:/w/live.mxf", "ProfileA");
    let _ = job.transition_to(playout_transcode::jobs::JobPhase::Probing, None);
    let _ = job.transition_to(playout_transcode::jobs::JobPhase::Planned, None);
    let _ = job.transition_to(playout_transcode::jobs::JobPhase::Encoding, None);
    let id = job.id.clone();
    s.jobs.push(job);

    let r = s
        .client()
        .delete(s.url(&format!("/api/jobs/{}", id)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    let body: serde_json::Value = r.json().await.unwrap();
    assert!(
        body["detail"].as_str().unwrap_or("").contains("Cancel it first"),
        "the refusal must say what to do instead: {}",
        body
    );
    assert!(s.jobs.get(&id).is_some(), "and the job stays on the queue");
}

#[tokio::test]
async fn clear_all_takes_the_failed_ones_and_spares_the_rest() {
    let s = spawn_test_server().await;

    for _ in 0..3 {
        let mut j = playout_transcode::jobs::JobRecord::new("D:/w/bad.mxf", "ProfileA");
        let _ = j.transition_to(playout_transcode::jobs::JobPhase::Probing, None);
        let _ = j.transition_to(playout_transcode::jobs::JobPhase::Failed, None);
        s.jobs.push(j);
    }
    let mut running = playout_transcode::jobs::JobRecord::new("D:/w/live.mxf", "ProfileA");
    let _ = running.transition_to(playout_transcode::jobs::JobPhase::Probing, None);
    let _ = running.transition_to(playout_transcode::jobs::JobPhase::Planned, None);
    let _ = running.transition_to(playout_transcode::jobs::JobPhase::Encoding, None);
    let running_id = running.id.clone();
    s.jobs.push(running);

    let r = s
        .client()
        .delete(s.url("/api/jobs/finished?state=failed"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["dismissed"], 3);

    assert!(
        s.jobs.get(&running_id).is_some(),
        "a bulk clear must never take a running encode"
    );

    // An unknown state is a 422, not a silent no-op that looks like success.
    let bad = s
        .client()
        .delete(s.url("/api/jobs/finished?state=banana"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 422);
}

#[tokio::test]
async fn delete_can_keep_the_media_file_or_take_it() {
    let s = spawn_test_server().await;
    let media = s.target_dir.join("videos");
    std::fs::create_dir_all(&media).unwrap();

    // 1. Delete the row, keep the file.
    let keep = media.join("keep.mp4");
    ready_asset(&s, "11111111-1111-4111-8111-111111111111", "aa", &keep).await;
    let r = s
        .client()
        .delete(s.url("/api/assets/11111111-1111-4111-8111-111111111111/purge?delete_file=false"))
        .header("X-Confirm-Destructive", "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        db::find_by_uuid_raw(&s.pool, "11111111-1111-4111-8111-111111111111")
            .await
            .unwrap()
            .is_none(),
        "the row goes"
    );
    assert!(keep.exists(), "and the media file stays");

    // 2. Delete the row and the file.
    let nuke = media.join("nuke.mp4");
    ready_asset(&s, "22222222-2222-4222-8222-222222222222", "bb", &nuke).await;
    let r = s
        .client()
        .delete(s.url("/api/assets/22222222-2222-4222-8222-222222222222/purge?delete_file=true"))
        .header("X-Confirm-Destructive", "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["media_removed"], true);
    assert!(!nuke.exists(), "the media file goes too");
}

/// The reference count is not the caller's to override. A file a sub-clip still
/// plays survives `delete_file=true`, and the response says why rather than
/// reporting a deletion that did not happen.
#[tokio::test]
async fn a_file_a_subclip_still_plays_is_never_deleted() {
    let s = spawn_test_server().await;
    let media = s.target_dir.join("videos");
    std::fs::create_dir_all(&media).unwrap();
    let shared = media.join("shared.mp4");

    let parent = "33333333-3333-4333-8333-333333333333";
    ready_asset(&s, parent, "cc", &shared).await;
    db::create_subclip(&s.pool, "44444444-4444-4444-8444-444444444444", parent, "Cut", 100, 500, true, "[]")
        .await
        .unwrap()
        .expect("sub-clip");

    let r = s
        .client()
        .delete(s.url(&format!("/api/assets/{}/purge?delete_file=true", parent)))
        .header("X-Confirm-Destructive", "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["media_removed"], false);
    assert!(shared.exists(), "the sub-clip would have nothing to play");
    let warnings = body["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap_or("").contains("reference")),
        "the response must say why the file was kept: {}",
        body
    );
}

/// Purge is destructive and stays behind the confirmation header, whatever the
/// new query parameter says.
#[tokio::test]
async fn the_delete_file_parameter_does_not_bypass_confirmation() {
    let s = spawn_test_server().await;
    let r = s
        .client()
        .delete(s.url("/api/assets/55555555-5555-4555-8555-555555555555/purge?delete_file=false"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 428);
}

#[tokio::test]
async fn clearing_a_verdict_lets_the_service_reconsider_the_media() {
    let s = spawn_test_server().await;
    let uuid = "66666666-6666-4666-8666-666666666666";

    db::insert_processing(&s.pool, uuid, 99, Some("dd"), "D:/w/bad.ts", "Bad")
        .await
        .unwrap();
    db::mark_ready(
        &s.pool,
        uuid,
        "D:/media/bad.mp4",
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
        Some("settings-now"),
    )
    .await
    .unwrap();

    // While the verdict stands, the service skips this media.
    assert!(
        db::find_reproducible_qc_failure(&s.pool, "settings-now", "dd")
            .await
            .unwrap()
            .is_some()
    );
    let held = |d: serde_json::Value| d["metrics"]["permanently_failed_assets"].clone();
    assert_eq!(held(s.get_json("/api/v2/diagnostics").await), 1);

    let r = s
        .client()
        .post(s.url(&format!("/api/assets/{}/clear-verdict", uuid)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["cleared"], true);

    // Now it will be examined again.
    assert!(
        db::find_reproducible_qc_failure(&s.pool, "settings-now", "dd")
            .await
            .unwrap()
            .is_none(),
        "clearing the verdict must actually release the media"
    );
    // W-5. And diagnostics stops counting it as held back.
    assert_eq!(held(s.get_json("/api/v2/diagnostics").await), 0);

    // Clearing twice is honest about having done nothing the second time.
    let again = s
        .client()
        .post(s.url(&format!("/api/assets/{}/clear-verdict", uuid)))
        .send()
        .await
        .unwrap();
    assert_eq!(again.json::<serde_json::Value>().await.unwrap()["cleared"], false);
}

/// W-5. A `ready` asset with no keyframe evidence stays `ready` -- demoting on
/// an environmental scan failure would pull it off air -- but it is counted.
#[tokio::test]
async fn diagnostics_counts_ready_assets_with_no_keyframe_evidence() {
    let s = spawn_test_server().await;
    sqlx::query(
        "INSERT INTO media_assets
           (uuid, fingerprint, current_path, duration_ms, status, mezzanine_ok,
            keyframe_offsets_json)
         VALUES
           ('77777777-7777-4777-8777-777777777771', 1, 'D:/m/a.mp4', 400, 'ready', 1, '[]'),
           ('77777777-7777-4777-8777-777777777772', 2, 'D:/m/b.mp4', 400, 'ready', 1, '[0]'),
           ('77777777-7777-4777-8777-777777777773', 3, 'D:/m/c.mp4', 400, 'error', 0, '[]')",
    )
    .execute(&*s.pool)
    .await
    .unwrap();

    let diag = s.get_json("/api/v2/diagnostics").await;
    assert_eq!(
        diag["metrics"]["unverified_keyframe_assets"], 1,
        "only the ready one with an empty list: {}",
        diag["metrics"]
    );
}
