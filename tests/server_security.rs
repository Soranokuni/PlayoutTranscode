//! End-to-end security tests against the real router: CORS (T0-2), the Host
//! guard (T0-2), folder validation (T0-3), the removed privileged endpoints
//! (T0-5) and config validate-before-persist (T0-6).

mod common;

use common::{spawn_test_server, status_line};
use serde_json::json;

#[tokio::test]
async fn health_is_cheap_and_reachable() {
    let s = spawn_test_server().await;
    let body = s.get_json("/api/health").await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "PlayoutTranscode");
}

// ---------------------------------------------------------------- CORS (F-02)

#[tokio::test]
async fn preflight_from_a_foreign_origin_gets_no_cors_headers() {
    let s = spawn_test_server().await;

    let r = s
        .client()
        .request(reqwest::Method::OPTIONS, s.url("/api/config"))
        .header("Origin", "https://evil.example")
        .header("Access-Control-Request-Method", "PUT")
        .header("Access-Control-Request-Headers", "content-type")
        .send()
        .await
        .expect("preflight");

    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "evil.example must not be granted CORS access"
    );
}

#[tokio::test]
async fn preflight_from_a_loopback_origin_is_allowed() {
    let s = spawn_test_server().await;
    let origin = format!("http://127.0.0.1:{}", s.port);

    let r = s
        .client()
        .request(reqwest::Method::OPTIONS, s.url("/api/config"))
        .header("Origin", &origin)
        .header("Access-Control-Request-Method", "PUT")
        .header("Access-Control-Request-Headers", "content-type")
        .send()
        .await
        .expect("preflight");

    assert_eq!(
        r.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some(origin.as_str())
    );
}

#[tokio::test]
async fn configured_extra_origin_is_allowed() {
    let s = common::spawn_test_server_with(common::TestServerOptions {
        config: Box::new(|c| {
            c.server.allowed_origins = vec!["http://localhost:5173".into()];
        }),
        ..Default::default()
    })
    .await;

    let r = s
        .client()
        .request(reqwest::Method::OPTIONS, s.url("/api/config"))
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "PUT")
        .send()
        .await
        .expect("preflight");

    assert_eq!(
        r.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
}

// ---------------------------------------------------- Host guard / rebinding

#[tokio::test]
async fn rebinding_host_header_is_rejected_with_421() {
    let s = spawn_test_server().await;

    let resp = s
        .raw_request("GET /api/health HTTP/1.1", "evil.localtest.me")
        .await;
    assert!(
        status_line(&resp).contains("421"),
        "expected 421 Misdirected Request, got: {}",
        status_line(&resp)
    );
}

#[tokio::test]
async fn loopback_host_headers_are_accepted() {
    let s = spawn_test_server().await;

    for host in [
        format!("127.0.0.1:{}", s.port),
        format!("localhost:{}", s.port),
        format!("[::1]:{}", s.port),
    ] {
        let resp = s.raw_request("GET /api/health HTTP/1.1", &host).await;
        assert!(
            status_line(&resp).contains("200"),
            "host {} should be accepted, got: {}",
            host,
            status_line(&resp)
        );
    }
}

#[tokio::test]
async fn host_header_with_the_wrong_port_is_rejected() {
    let s = spawn_test_server().await;
    let resp = s
        .raw_request("GET /api/health HTTP/1.1", "127.0.0.1:9")
        .await;
    assert!(
        status_line(&resp).contains("421"),
        "wrong port should be rejected, got: {}",
        status_line(&resp)
    );
}

// ------------------------------------------- removed privileged routes (F-04)

#[tokio::test]
async fn service_install_and_uninstall_are_gone() {
    let s = spawn_test_server().await;

    for path in ["/api/service/install", "/api/service/uninstall"] {
        let r = s.post_json(path, json!({})).await;
        assert_eq!(r.status(), 410, "{}", path);
        let body: serde_json::Value = r.json().await.expect("json");
        assert_eq!(body["error"], "removed; use installer");
    }
}

// ------------------------------------------------ folder validation (F-05)

#[tokio::test]
async fn wildcard_folder_paths_are_rejected() {
    let s = spawn_test_server().await;

    for path in ["/api/folders/trash", "/api/folders/restore"] {
        let r = s.post_json(path, json!({ "folder_path": "/%" })).await;
        assert_eq!(r.status(), 422, "{} must reject /%", path);
    }

    let r = s
        .delete_json("/api/folders/purge", json!({ "folder_path": "/%" }))
        .await;
    assert_eq!(r.status(), 422, "purge must reject /%");
}

#[tokio::test]
async fn malformed_folder_paths_are_rejected() {
    let s = spawn_test_server().await;

    for bad in [
        json!("relative/no/slash"),
        json!("/trailing/"),
        json!("/a/../b"),
        json!("/double//slash"),
        json!("/trailing "),
    ] {
        let r = s
            .post_json("/api/folders/trash", json!({ "folder_path": bad }))
            .await;
        assert_eq!(r.status(), 422, "folder_path {} must be rejected", bad);
    }
}

#[tokio::test]
async fn ordinary_folder_paths_are_accepted() {
    let s = spawn_test_server().await;

    // No matching assets, but the request itself must be well-formed.
    let r = s
        .post_json(
            "/api/folders/trash",
            json!({ "folder_path": "/Shows/Season 1_2026" }),
        )
        .await;
    assert_eq!(r.status(), 200);
}

// ------------------------------------------------- config validation (F-03)

#[tokio::test]
async fn invalid_config_patch_is_rejected_and_nothing_changes() {
    let s = spawn_test_server().await;

    let before = s.get_json("/api/config").await;

    let r = s
        .put_json(
            "/api/config",
            json!({ "encoding": { "tune": "not-a-tune" } }),
        )
        .await;
    assert_eq!(r.status(), 422);

    let after = s.get_json("/api/config").await;
    assert_eq!(
        before["encoding"]["tune"], after["encoding"]["tune"],
        "a rejected patch must leave the running config untouched"
    );
}

#[tokio::test]
async fn config_patch_pointing_target_inside_watch_is_rejected() {
    let s = spawn_test_server().await;

    let nested = s.watch_dir.join("published");
    let r = s
        .put_json(
            "/api/config",
            json!({ "paths": { "target_folder": nested.to_string_lossy() } }),
        )
        .await;
    assert_eq!(r.status(), 422, "target inside watch must be rejected");

    let body: serde_json::Value = r.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap_or("").contains("overlap"),
        "unexpected error: {}",
        body["error"]
    );
    assert!(
        !nested.exists(),
        "a rejected config must not create directories"
    );
}

#[tokio::test]
async fn config_patch_pointing_at_a_system_root_is_rejected() {
    let s = spawn_test_server().await;

    let Some(profile) = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
    else {
        return;
    };
    let container = match std::path::Path::new(&profile).parent() {
        Some(p) => p.to_string_lossy().to_string(),
        None => return,
    };

    let r = s
        .put_json(
            "/api/config",
            json!({ "paths": { "watch_folder": container } }),
        )
        .await;
    assert_eq!(
        r.status(),
        422,
        "the profile container (C:\\Users) must be rejected as a watch folder"
    );
}

#[tokio::test]
async fn valid_config_patch_is_applied() {
    let s = spawn_test_server().await;

    let r = s
        .put_json(
            "/api/config",
            json!({ "encoding": { "preset": "veryfast" }, "ingestion": { "max_concurrency": 3 } }),
        )
        .await;
    assert_eq!(r.status(), 200);

    let after = s.get_json("/api/config").await;
    assert_eq!(after["encoding"]["preset"], "veryfast");
    assert_eq!(after["ingestion"]["max_concurrency"], 3);
}
