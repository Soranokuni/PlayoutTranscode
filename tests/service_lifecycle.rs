//! The stop/start lifecycle as seen over HTTP (T2-5).
//!
//! The state machine itself is unit-tested in `src/service_handle.rs`; these
//! pin the wire shape PlayOut and the web UI read, because `state` is the field
//! that tells a client the difference between "stopped" and "still stopping" —
//! a distinction the old `running` boolean could not express.

mod common;

#[tokio::test]
async fn service_status_reports_the_lifecycle_state() {
    let s = common::spawn_test_server().await;

    let status = s.get_json("/api/service/status").await;

    // The pre-existing field is unchanged. Clients that only read it keep
    // working.
    assert_eq!(
        status["running"], false,
        "a freshly spawned harness has no processing loop"
    );

    // Additive.
    assert_eq!(status["state"], "stopped");
    assert!(
        status["generation"].is_number(),
        "generation identifies the run: {}",
        status
    );
    assert_eq!(
        status["generation"], 0,
        "no run has started, so no generation has been issued"
    );
}

#[tokio::test]
async fn diagnostics_carries_the_same_state_string() {
    let s = common::spawn_test_server().await;

    let diag = s.get_json("/api/diagnostics").await;
    assert_eq!(diag["service"]["running"], false);
    assert_eq!(diag["service"]["state"], "stopped");
}

#[tokio::test]
async fn stopping_an_already_stopped_service_is_a_no_op() {
    let s = common::spawn_test_server().await;

    let response = s
        .post_json_confirmed("/api/service/stop", serde_json::json!({}))
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["state"], "stopped");

    // And it must not have moved the state machine anywhere.
    let status = s.get_json("/api/service/status").await;
    assert_eq!(status["state"], "stopped");
    assert_eq!(status["generation"], 0);
}

#[tokio::test]
async fn a_start_that_cannot_run_reports_a_status_code_and_not_a_bare_200() {
    let s = common::spawn_test_server().await;

    let response = s.post_json("/api/service/start", serde_json::json!({})).await;
    // The harness has watch and target folders but no FFmpeg, so the start
    // fails at the toolchain check. Before T2-5 every one of these refusals was
    // a 200 carrying `success: false`, which a client could only distinguish by
    // parsing the body. Now the status code carries it: 503 for a missing
    // toolchain, 400 for unconfigured folders, 409 for a state-machine refusal.
    assert_eq!(response.status(), 503);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], false);
    assert!(
        body["error"].as_str().unwrap().contains("FFmpeg toolchain"),
        "error should name the toolchain: {}",
        body
    );

    // A refused start must leave the state machine where it found it -- no
    // generation is burned and nothing is left in `Starting`.
    let status = s.get_json("/api/service/status").await;
    assert_eq!(status["state"], "stopped");
    assert_eq!(status["generation"], 0);
}
