//! End-to-end tests for API token authentication (T1-1 / F-02).

mod common;

use common::{spawn_test_server_with, TestServerOptions};
use serde_json::json;

const TOKEN: &str = "test-token-abcdefghijklmnopqrstuvwxyz0123456789";

async fn authed_server() -> common::TestServer {
    spawn_test_server_with(TestServerOptions {
        config: Box::new(|c| c.server.api_token = TOKEN.to_string()),
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn health_is_reachable_without_a_token() {
    let s = authed_server().await;

    // PlayOut polls this every 5 s as its liveness signal and the web UI needs
    // it before the operator has typed the token.
    for path in ["/api/health", "/api/v2/health"] {
        let r = s.get(path).await;
        assert_eq!(r.status(), 200, "{} must stay token-free", path);
    }
}

#[tokio::test]
async fn protected_routes_require_a_token() {
    let s = authed_server().await;

    for path in ["/api/jobs", "/api/config", "/api/assets", "/api/v2/assets"] {
        let r = s.get(path).await;
        assert_eq!(r.status(), 401, "{} must require a token", path);
        let body: serde_json::Value = r.json().await.expect("json");
        assert_eq!(body["error"], "unauthorized");
    }
}

#[tokio::test]
async fn mutating_routes_require_a_token() {
    let s = authed_server().await;

    let r = s.put_json("/api/config", json!({})).await;
    assert_eq!(r.status(), 401);

    let r = s
        .delete_json("/api/folders/purge", json!({ "folder_path": "/" }))
        .await;
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn header_token_is_accepted() {
    let s = authed_server().await;

    let r = s
        .client()
        .get(s.url("/api/jobs"))
        .header("X-Api-Token", TOKEN)
        .send()
        .await
        .expect("GET");
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn bearer_token_is_accepted() {
    let s = authed_server().await;

    let r = s
        .client()
        .get(s.url("/api/jobs"))
        .header("Authorization", format!("Bearer {}", TOKEN))
        .send()
        .await
        .expect("GET");
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn query_token_is_accepted_for_sse() {
    let s = authed_server().await;

    // EventSource cannot set headers, so the stream authenticates via query.
    let r = s
        .client()
        .get(format!("{}?token={}", s.url("/api/events"), TOKEN))
        .timeout(std::time::Duration::from_secs(2))
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
}

#[tokio::test]
async fn sse_without_a_token_is_rejected() {
    let s = authed_server().await;

    let r = s
        .client()
        .get(s.url("/api/events"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .expect("GET /api/events");
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn a_wrong_token_is_rejected() {
    let s = authed_server().await;

    for bad in [
        "wrong",
        // Same length as the real token, differing in the last byte: the
        // comparison must be constant-time and still reject.
        "test-token-abcdefghijklmnopqrstuvwxyz012345678X",
        "",
    ] {
        let r = s
            .client()
            .get(s.url("/api/jobs"))
            .header("X-Api-Token", bad)
            .send()
            .await
            .expect("GET");
        assert_eq!(r.status(), 401, "token {:?} must be rejected", bad);
    }
}

#[tokio::test]
async fn static_files_do_not_require_a_token() {
    let s = authed_server().await;

    let r = s.get("/").await;
    assert_eq!(r.status(), 200);
    let r = s.get("/assets/app.js").await;
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn config_never_echoes_the_token() {
    let s = authed_server().await;

    let r = s
        .client()
        .get(s.url("/api/config"))
        .header("X-Api-Token", TOKEN)
        .send()
        .await
        .expect("GET");
    assert_eq!(r.status(), 200);
    let body = r.text().await.expect("body");
    assert!(
        !body.contains(TOKEN),
        "GET /api/config must not leak the token: {}",
        body
    );

    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["server"]["api_token_set"], true);
    assert!(json["server"].get("api_token").is_none());
}

#[tokio::test]
async fn no_token_configured_means_open_loopback_access() {
    let s = common::spawn_test_server().await;

    let r = s.get("/api/jobs").await;
    assert_eq!(
        r.status(),
        200,
        "with no token configured, loopback access stays open"
    );

    let json = s.get_json("/api/config").await;
    assert_eq!(json["server"]["api_token_set"], false);
}
