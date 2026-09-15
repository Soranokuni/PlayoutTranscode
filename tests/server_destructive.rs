//! Destructive operations require an explicit confirmation header (T1-5,
//! PlayOut handoff §3.7).

mod common;

use common::spawn_test_server;
use serde_json::json;

const VALID_UUID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

#[tokio::test]
async fn destructive_routes_return_428_without_confirmation() {
    let s = spawn_test_server().await;

    let cases: Vec<(reqwest::Method, String)> = vec![
        (
            reqwest::Method::DELETE,
            format!("/api/assets/{}/purge", VALID_UUID),
        ),
        (reqwest::Method::DELETE, "/api/folders/purge".into()),
        (reqwest::Method::DELETE, "/api/recycle-bin/purge".into()),
        (reqwest::Method::POST, "/api/recycle-bin/auto-purge".into()),
        (reqwest::Method::POST, "/api/folders/trash".into()),
        (reqwest::Method::POST, "/api/jobs/retry-failed".into()),
        (reqwest::Method::POST, "/api/service/stop".into()),
        (reqwest::Method::PUT, "/api/config".into()),
        // The same table must cover v2.
        (reqwest::Method::DELETE, "/api/v2/folders/purge".into()),
        (reqwest::Method::DELETE, "/api/v2/recycle-bin/purge".into()),
        (
            reqwest::Method::DELETE,
            format!("/api/v2/assets/{}/purge", VALID_UUID),
        ),
        (reqwest::Method::PUT, "/api/v2/config".into()),
    ];

    for (method, path) in cases {
        let r = s
            .client()
            .request(method.clone(), s.url(&path))
            .json(&json!({ "folder_path": "/" }))
            .send()
            .await
            .expect("request");
        assert_eq!(
            r.status(),
            428,
            "{} {} must require confirmation",
            method,
            path
        );
        let body: serde_json::Value = r.json().await.expect("json");
        assert_eq!(body["error"], "confirmation_required");
    }
}

#[tokio::test]
async fn non_destructive_routes_need_no_confirmation() {
    let s = spawn_test_server().await;

    // Reads, and writes that only move metadata around, are unaffected.
    for path in ["/api/health", "/api/config", "/api/jobs", "/api/recycle-bin"] {
        let r = s.get(path).await;
        assert_eq!(r.status(), 200, "{} must not require confirmation", path);
    }

    // Restore is the inverse of trash and is not destructive.
    let r = s
        .post_json("/api/folders/restore", json!({ "folder_path": "/Shows" }))
        .await;
    assert_eq!(r.status(), 200);

    // A single-asset trash is reversible via the recycle bin.
    let r = s
        .post_json(&format!("/api/assets/{}/trash", VALID_UUID), json!({}))
        .await;
    assert_eq!(r.status(), 404, "reached the handler, not the 428 gate");
}

#[tokio::test]
async fn confirmation_header_arms_the_operation() {
    let s = spawn_test_server().await;

    let r = s
        .post_json_confirmed("/api/folders/trash", json!({ "folder_path": "/Shows" }))
        .await;
    assert_eq!(r.status(), 200);

    let r = s
        .delete_json_confirmed("/api/recycle-bin/purge", json!({}))
        .await;
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn only_an_affirmative_header_counts() {
    let s = spawn_test_server().await;

    for value in ["", "no", "false", "0", "maybe"] {
        let r = s
            .client()
            .post(s.url("/api/folders/trash"))
            .header("X-Confirm-Destructive", value)
            .json(&json!({ "folder_path": "/Shows" }))
            .send()
            .await
            .expect("request");
        assert_eq!(r.status(), 428, "header value {:?} must not arm", value);
    }

    // Case-insensitive and whitespace-tolerant, since clients vary.
    for value in ["yes", "YES", " Yes "] {
        let r = s
            .client()
            .post(s.url("/api/folders/trash"))
            .header("X-Confirm-Destructive", value)
            .json(&json!({ "folder_path": "/Shows" }))
            .send()
            .await
            .expect("request");
        assert_eq!(r.status(), 200, "header value {:?} must arm", value);
    }
}

#[tokio::test]
async fn confirmation_does_not_bypass_authentication() {
    let s = common::spawn_test_server_with(common::TestServerOptions {
        config: Box::new(|c| {
            c.server.api_token = "test-token-abcdefghijklmnopqrstuvwxyz0123456789".into()
        }),
        ..Default::default()
    })
    .await;

    // The token check runs first: a confirmed but unauthenticated purge is 401,
    // never 428 and never executed.
    let r = s
        .delete_json_confirmed("/api/recycle-bin/purge", json!({}))
        .await;
    assert_eq!(r.status(), 401);
}
