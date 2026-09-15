//! HTTP hardening and caching (T1-6 / F-06, F-07, F-10).

mod common;

use common::spawn_test_server;
use serde_json::json;

#[tokio::test]
async fn toolchain_status_is_served_from_cache() {
    let s = spawn_test_server().await;

    // The bundled UI polls this every 2 s. It used to spawn `ffmpeg -version`
    // and `ffprobe -version` on every call — two process spawns per second per
    // open tab (F-06). Repeated calls must be cheap and consistent.
    let started = std::time::Instant::now();
    let first = s.get_json("/api/toolchain").await;
    for _ in 0..25 {
        let next = s.get_json("/api/toolchain").await;
        assert_eq!(next, first, "cached toolchain status must not vary");
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "26 toolchain reads took {:?}; the cache is not working",
        started.elapsed()
    );
}

#[tokio::test]
async fn health_stays_cheap_under_repeated_polling() {
    let s = spawn_test_server().await;

    // PlayOut polls health every 5 s and treats it as the liveness signal
    // (handoff §3.5), so it must never do process or disk work.
    let started = std::time::Instant::now();
    for _ in 0..100 {
        let r = s.get("/api/health").await;
        assert_eq!(r.status(), 200);
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "100 health polls took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn diagnostics_reports_database_health_from_quick_check() {
    let s = spawn_test_server().await;

    let first = s.get_json("/api/diagnostics").await;
    assert_eq!(first["database"]["integrity"], "ok");

    // Cached for ten minutes: a second call must not re-run the check.
    let started = std::time::Instant::now();
    let second = s.get_json("/api/diagnostics").await;
    assert_eq!(second["database"]["integrity"], "ok");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn oversized_bodies_are_rejected() {
    let s = spawn_test_server().await;

    // Explicit 1 MiB cap rather than relying on axum's implicit default.
    // The limiter rejects on Content-Length and stops reading, so the client
    // sees either 413/400 or a connection abort while it is still writing.
    // Both mean the body was refused; what must never happen is a 2xx.
    let big = "x".repeat(2 * 1024 * 1024);
    let result = s
        .client()
        .post(s.url("/api/assets/batch"))
        .header("content-type", "application/json")
        .body(format!("[\"{}\"]", big))
        .send()
        .await;
    match result {
        Ok(r) => assert!(
            r.status() == 413 || r.status() == 400,
            "a 2 MiB body should be refused, got {}",
            r.status()
        ),
        Err(e) => assert!(
            e.is_request() || e.is_body(),
            "unexpected transport error: {}",
            e
        ),
    }

    // A normal body still works.
    let r = s
        .post_json(
            "/api/assets/batch",
            json!(["3f2504e0-4f89-41d3-9a0c-0305e82c3301"]),
        )
        .await;
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn sse_is_not_subject_to_the_request_timeout() {
    let s = spawn_test_server().await;

    // `/api/events` streams indefinitely; a 30 s request timeout would cut
    // every client. It is mounted outside the timed router, so the stream must
    // open and stay open.
    let r = s
        .client()
        .get(s.url("/api/events"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .expect("GET /api/events");
    assert_eq!(r.status(), 200);
    assert!(r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("text/event-stream"));

    // The v2 alias is mounted the same way.
    let r = s
        .client()
        .get(s.url("/api/v2/events"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .expect("GET /api/v2/events");
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn concurrent_requests_all_complete() {
    let s = spawn_test_server().await;

    // The concurrency limit queues rather than rejects, so well past the limit
    // everything must still succeed — and nothing may deadlock, which is what
    // a blocking call left on a Tokio worker would cause (F-07).
    let mut handles = Vec::new();
    for _ in 0..128 {
        let client = s.client().clone();
        let url = s.url("/api/health");
        handles.push(tokio::spawn(async move {
            client.get(url).send().await.map(|r| r.status().as_u16())
        }));
    }
    for h in handles {
        let status = h.await.expect("join").expect("request");
        assert_eq!(status, 200);
    }
}
