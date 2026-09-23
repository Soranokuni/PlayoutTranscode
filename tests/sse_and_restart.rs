//! SSE robustness (T2-10, F-20) and the `restart_required` flag (T2-12, F-23).

mod common;

use std::time::Duration;

/// Read SSE frames off the wire until `want` events have been seen or the
/// deadline passes. Returns `(event_name, data)` pairs in order.
async fn read_events(
    response: reqwest::Response,
    want: usize,
    timeout: Duration,
) -> Vec<(String, String)> {
    let mut response = response;
    let mut out = Vec::new();
    let mut buf = String::new();

    // `chunk()` rather than `bytes_stream()`, so the harness needs neither the
    // reqwest `stream` feature nor a futures dependency.
    let deadline = tokio::time::Instant::now() + timeout;
    while out.len() < want {
        let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(Some(c))) => c,
            _ => break,
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // SSE frames are separated by a blank line.
        while let Some(idx) = buf.find("\n\n") {
            let frame = buf[..idx].to_string();
            buf = buf[idx + 2..].to_string();

            let mut name = String::new();
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data = v.trim().to_string();
                }
            }
            if !name.is_empty() {
                out.push((name, data));
            }
        }
    }
    out
}

#[tokio::test]
async fn the_first_event_on_every_stream_is_connected() {
    let s = common::spawn_test_server().await;

    let r = s.get("/api/events").await;
    assert_eq!(r.status(), 200);

    let events = read_events(r, 1, Duration::from_secs(5)).await;
    assert!(!events.is_empty(), "the stream must open with an event");
    assert_eq!(
        events[0].0, "connected",
        "a client resynchronises on this; it cannot be conditional"
    );

    let data: serde_json::Value = serde_json::from_str(&events[0].1).unwrap();
    assert!(
        data["server_time"].is_string(),
        "server_time distinguishes a fresh connection from a replayed one: {}",
        data
    );
}

#[tokio::test]
async fn a_broadcast_after_connect_arrives_on_the_stream() {
    let s = common::spawn_test_server().await;

    let r = s.get("/api/events").await;
    let jobs = s.jobs.clone();

    // Emit once the subscriber is established. The `connected` frame is
    // generated per-stream, so seeing it means the subscription exists.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        jobs.broadcast("progress", r#"{"id":"j1","percent":42.0}"#);
    });

    let events = read_events(r, 2, Duration::from_secs(5)).await;
    assert_eq!(events[0].0, "connected");
    assert_eq!(events[1].0, "progress");
    let data: serde_json::Value = serde_json::from_str(&events[1].1).unwrap();
    assert_eq!(data["id"], "j1");
    // SB-05: the frame now carries the producer's strings verbatim instead of
    // round-tripping them through serde_json::Value, so the body on the wire
    // is byte-for-byte what the caller passed.
    assert_eq!(events[1].1, r#"{"id":"j1","percent":42.0}"#);
}

#[tokio::test]
async fn a_subscriber_that_falls_behind_is_told_to_resync() {
    // The whole point of F-20: a slow consumer used to lose events silently and
    // then display a stale job list forever. The channel holds 1024, so
    // overflowing it deliberately is the only way to observe the recovery.
    let s = common::spawn_test_server().await;

    let r = s.get("/api/events").await;

    // Let the stream open and emit its `connected` frame, then flood past the
    // buffer without reading. The HTTP body is not being consumed yet, so the
    // broadcast receiver backs up and lags.
    tokio::time::sleep(Duration::from_millis(300)).await;
    for i in 0..3000 {
        s.jobs
            .broadcast("progress", &format!(r#"{{"id":"j{}","percent":1.0}}"#, i));
    }

    let events = read_events(r, 200, Duration::from_secs(10)).await;

    let resync = events.iter().find(|(name, _)| name == "resync");
    let resync = resync.unwrap_or_else(|| {
        panic!(
            "expected a resync event after overflowing the channel; saw {:?}",
            events.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
        )
    });

    let data: serde_json::Value = serde_json::from_str(&resync.1).unwrap();
    let dropped = data["dropped"].as_u64().expect("resync carries a count");
    assert!(dropped > 0, "a resync with nothing dropped is meaningless");

    // And the stream keeps working afterwards -- a lag is recoverable, not fatal.
    assert!(
        events.iter().any(|(name, _)| name == "progress"),
        "the subscription must survive the lag"
    );
}

#[tokio::test]
async fn restart_required_is_false_while_the_service_is_stopped() {
    let s = common::spawn_test_server().await;

    let status = s.get_json("/api/service/status").await;
    assert_eq!(status["running"], false);
    assert_eq!(
        status["restart_required"], false,
        "there is nothing to restart, so the flag must not nag"
    );
}

#[test]
fn the_runtime_hash_covers_what_the_loop_captures_and_nothing_else() {
    use playout_transcode::service_handle::runtime_config_hash;

    let base = playout_transcode::config::AppConfig::default();
    let baseline = runtime_config_hash(&base);
    assert_eq!(runtime_config_hash(&base), baseline, "must be stable");

    // Fields the processing loop copies by value at start: changing one means
    // the running loop is stale until it is restarted.
    let mut c = base.clone();
    c.ingestion.max_concurrency += 1;
    assert_ne!(runtime_config_hash(&c), baseline, "max_concurrency");

    let mut c = base.clone();
    c.paths.watch_folder = "D:/somewhere/else".into();
    assert_ne!(runtime_config_hash(&c), baseline, "watch_folder");

    let mut c = base.clone();
    c.ingestion.settle_secs += 1;
    assert_ne!(runtime_config_hash(&c), baseline, "settle_secs");

    let mut c = base.clone();
    c.encoding.cpu_cores += 1;
    assert_ne!(runtime_config_hash(&c), baseline, "cpu_cores");

    // Fields read per request or per job, which take effect immediately.
    // Hashing these would report a restart as needed for a change that has
    // already applied -- a flag that cries wolf is worse than no flag.
    let mut c = base.clone();
    c.encoding.preset = "veryslow".into();
    assert_eq!(
        runtime_config_hash(&c),
        baseline,
        "preset is read per encode and applies immediately"
    );

    let mut c = base.clone();
    c.logging.level = "debug".into();
    assert_eq!(runtime_config_hash(&c), baseline, "logging level");
}

/// Wait for the first event called `name` whose data satisfies `pred`.
async fn wait_for_event(
    response: reqwest::Response,
    name: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    let events = read_events(response, 20, Duration::from_secs(5)).await;
    events
        .into_iter()
        .filter(|(n, _)| n == name)
        .filter_map(|(_, d)| serde_json::from_str::<serde_json::Value>(&d).ok())
        .find(|d| pred(d))
}

/// UI-01. Pressing cancel used to change the job's phase without telling
/// anyone, so the queue row sat unchanged until the UI's 15 s poll. The
/// `job_update` must carry the whole record so the client can apply it.
#[tokio::test]
async fn a_cancel_request_is_announced_with_the_new_record() {
    use playout_transcode::jobs::{JobPhase, JobRecord};
    let s = common::spawn_test_server().await;

    let mut job = JobRecord::new("D:/w/live.mxf", "ProfileA");
    for p in [JobPhase::Probing, JobPhase::Planned, JobPhase::Encoding] {
        job.transition_to(p, None).unwrap();
    }
    let id = job.id.clone();
    s.jobs.push(job);

    let r = s.get("/api/events").await;
    let base = s.base_url.clone();
    let cancel_id = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = reqwest::Client::new()
            .post(format!("{}/api/jobs/{}/cancel", base, cancel_id))
            .json(&serde_json::json!({}))
            .send()
            .await;
    });

    let update = wait_for_event(r, "job_update", |d| d["id"] == id.as_str())
        .await
        .expect("cancel must emit a job_update");
    assert_eq!(update["phase"], "cancel_requested", "{}", update);
    assert_eq!(update["job"]["id"], id.as_str());
    assert_eq!(update["job"]["cancel_requested"], true);
    // The documented v1 fields stay.
    assert!(update["stage"].is_string());
}

/// UI-01. A job that is still queued goes straight to `Cancelled`, and that
/// terminal state is announced too.
#[tokio::test]
async fn cancelling_a_queued_job_announces_it_cancelled() {
    use playout_transcode::jobs::JobRecord;
    let s = common::spawn_test_server().await;

    let job = JobRecord::new("D:/w/queued.mxf", "ProfileA");
    let id = job.id.clone();
    s.jobs.push(job);

    let r = s.get("/api/events").await;
    let base = s.base_url.clone();
    let cancel_id = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = reqwest::Client::new()
            .post(format!("{}/api/jobs/{}/cancel", base, cancel_id))
            .json(&serde_json::json!({}))
            .send()
            .await;
    });

    let update = wait_for_event(r, "job_update", |d| d["id"] == id.as_str())
        .await
        .expect("cancel must emit a job_update");
    assert_eq!(update["state"], "Cancelled", "{}", update);
}

/// UI-02. A library change made over the API -- by PlayOut, or by another
/// tab -- is announced, so an open UI does not keep showing the old row.
#[tokio::test]
async fn an_asset_mutation_is_announced_with_its_uuid() {
    let s = common::spawn_test_server().await;
    let uuid = "0b7e3c1a-5d2f-4e8b-9a61-3c4d5e6f7a8b";
    let file = s.target_dir.join("a.mp4");
    std::fs::write(&file, b"x").unwrap();
    common::insert_ready_asset(&s.pool, uuid, 7, &file, "a").await;

    let r = s.get("/api/events").await;
    let base = s.base_url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = reqwest::Client::new()
            .post(format!("{}/api/assets/{}/trash", base, uuid))
            .header("X-Confirm-Destructive", "yes")
            .send()
            .await;
    });

    let changed = wait_for_event(r, "assets_changed", |_| true)
        .await
        .expect("a trash must emit assets_changed");
    assert_eq!(changed["uuid"], uuid);
}

/// UI-02. A refused mutation changed nothing, so it must not be announced.
#[tokio::test]
async fn a_failed_asset_mutation_is_not_announced() {
    let s = common::spawn_test_server().await;
    let missing = "11111111-2222-4333-8444-555555555555";

    let r = s.get("/api/events").await;
    let base = s.base_url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = reqwest::Client::new()
            .post(format!("{}/api/assets/{}/trash", base, missing))
            .header("X-Confirm-Destructive", "yes")
            .send()
            .await;
    });

    let events = read_events(r, 3, Duration::from_secs(2)).await;
    assert!(
        !events.iter().any(|(n, _)| n == "assets_changed"),
        "a 404 must not be announced: {:?}",
        events
    );
}
