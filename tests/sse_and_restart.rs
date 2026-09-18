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
