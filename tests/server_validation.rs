//! End-to-end input validation tests (T1-3 / F-08, PlayOut handoff §3.1).

mod common;

use common::spawn_test_server;
use serde_json::json;

/// Not a real asset, but a well-formed id: these requests must get past
/// validation and fail with 404, proving the 422s below are about the *shape*
/// of the id and not about the asset being missing.
const VALID_UUID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

const BAD_IDS: &[&str] = &[
    "not-a-uuid",
    "3f2504e0-4f89-41d3-9a0c-0305e82c330",   // too short
    "3f2504e0-4f89-41d3-9a0c-0305e82c33011", // too long
    "3f2504e04f8941d39a0c0305e82c3301",      // unhyphenated
    "3f2504e0-4f89-41d3-9a0c-0305e82c330g",  // non-hex
    "1",
    "..%2fconfig.toml",
    "*",
];

#[tokio::test]
async fn asset_routes_reject_non_canonical_ids() {
    let s = spawn_test_server().await;

    for bad in BAD_IDS {
        let r = s.get(&format!("/api/assets/{}", bad)).await;
        assert_eq!(r.status(), 422, "GET /api/assets/{} must be 422", bad);
        let body: serde_json::Value = r.json().await.expect("json");
        assert_eq!(body["error"], "invalid asset id");
    }

    // A canonical id for a missing asset is a 404, not a 422.
    let r = s.get(&format!("/api/assets/{}", VALID_UUID)).await;
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn percent_encoded_dot_segments_fall_through_to_the_spa() {
    let s = spawn_test_server().await;

    // `/api/assets/%2e%2e` decodes to a `..` segment that matches no API
    // route, so it lands on the SPA fallback. Documented here so the 200 is
    // not mistaken for a validation hole: the body is index.html, and T0-1
    // guarantees the fallback can never read a file outside the SPA root.
    let r = s.get("/api/assets/%2e%2e").await;
    assert_eq!(r.status(), 200);
    let body = r.text().await.expect("body");
    assert!(
        body.contains("<title>spa</title>"),
        "expected the SPA index, got: {}",
        body
    );
}

#[tokio::test]
async fn every_uuid_route_is_guarded() {
    let s = spawn_test_server().await;
    let bad = "not-a-uuid";

    for path in [
        format!("/api/assets/{}", bad),
        format!("/api/v2/assets/{}", bad),
        format!("/api/db/assets/{}", bad),
        format!("/api/db/jobs/{}", bad),
        format!("/api/v2/jobs/{}", bad),
    ] {
        let r = s.get(&path).await;
        assert_eq!(r.status(), 422, "GET {} must be 422", path);
    }

    for path in [
        format!("/api/assets/{}/trash", bad),
        format!("/api/assets/{}/restore", bad),
        format!("/api/assets/{}/subclip", bad),
        format!("/api/assets/{}/regenerate-sidecar", bad),
        format!("/api/jobs/{}/retry", bad),
        format!("/api/jobs/{}/cancel", bad),
    ] {
        let r = s.post_json(&path, json!({})).await;
        assert_eq!(r.status(), 422, "POST {} must be 422", path);
    }

    for path in [
        format!("/api/assets/{}/trim", bad),
        format!("/api/assets/{}/rating", bad),
        format!("/api/assets/{}/tp", bad),
        format!("/api/assets/{}/rename", bad),
        format!("/api/assets/{}/move", bad),
    ] {
        let r = s.put_json(&path, json!({})).await;
        assert_eq!(r.status(), 422, "PUT {} must be 422", path);
    }

    let r = s
        .delete_json(&format!("/api/assets/{}/purge", bad), json!({}))
        .await;
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn batch_body_ids_are_validated() {
    let s = spawn_test_server().await;

    let r = s
        .post_json("/api/assets/batch", json!([VALID_UUID, "not-a-uuid"]))
        .await;
    assert_eq!(r.status(), 422);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["error"], "invalid asset id in batch request");

    // All-valid ids get through validation (and resolve to nothing).
    let r = s.post_json("/api/assets/batch", json!([VALID_UUID])).await;
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn tp_is_validated() {
    let s = spawn_test_server().await;
    let path = format!("/api/assets/{}/tp", VALID_UUID);

    for bad in [
        json!("<script>alert(1)</script>"),
        json!("drop\u{0}table"),
        json!("x".repeat(513)),
        json!("tp\nnewline"),
    ] {
        let r = s.put_json(&path, json!({ "tp": bad })).await;
        assert_eq!(r.status(), 422, "tp {} must be rejected", bad);
    }

    // A well-formed value gets past validation and 404s on the missing asset.
    let r = s.put_json(&path, json!({ "tp": "TP|18:00" })).await;
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn rating_payload_is_bounded_and_structured() {
    let s = spawn_test_server().await;
    let path = format!("/api/assets/{}/rating", VALID_UUID);

    for bad in [
        json!(format!("K|{}", "x".repeat(5000))), // over the 4 KiB cap
        json!("K|[{\"broken\": }"),               // tail claims JSON but is not
        json!("NOT-A-RATING"),
    ] {
        let r = s.put_json(&path, json!({ "rating": bad })).await;
        assert_eq!(r.status(), 422, "rating {} must be rejected", bad);
    }

    for good in [json!("K"), json!("12+"), json!("K|[\"a\",\"b\"]"), json!("")] {
        let r = s.put_json(&path, json!({ "rating": good })).await;
        assert_eq!(r.status(), 404, "rating {} must pass validation", good);
    }
}

#[tokio::test]
async fn folder_color_cannot_inject_css() {
    let s = spawn_test_server().await;

    for bad in [
        // DbViewer.vue renders this into a `style` binding.
        json!("red; background: url(http://evil.example/x)"),
        json!("expression(alert(1))"),
        json!("#12345"),
        json!("#gggggg"),
        json!("rgb(1,2,3)"),
    ] {
        let r = s
            .put_json(
                "/api/folders/colors",
                json!({ "virtual_folder": "/Shows", "color": bad }),
            )
            .await;
        assert_eq!(r.status(), 422, "color {} must be rejected", bad);
    }

    for good in [json!("#a1b2c3"), json!("blue"), json!("")] {
        let r = s
            .put_json(
                "/api/folders/colors",
                json!({ "virtual_folder": "/Shows", "color": good }),
            )
            .await;
        assert_eq!(r.status(), 200, "color {} must be accepted", good);
    }
}

#[tokio::test]
async fn display_names_reject_control_characters() {
    let s = spawn_test_server().await;
    let path = format!("/api/assets/{}/rename", VALID_UUID);

    for bad in [
        json!(""),
        json!("  padded  "),
        json!("line\nbreak"),
        json!("bell\u{7}"),
        json!("x".repeat(256)),
    ] {
        let r = s.put_json(&path, json!({ "display_name": bad })).await;
        assert_eq!(r.status(), 422, "display_name {} must be rejected", bad);
    }

    let r = s
        .put_json(&path, json!({ "display_name": "Promo 2026 - Final" }))
        .await;
    assert_eq!(r.status(), 404, "a valid name must pass validation");
}

#[tokio::test]
async fn retry_rejects_unc_and_out_of_watch_paths() {
    let s = spawn_test_server().await;

    // The job id must exist before input_path is looked at, so a missing job
    // is a 404 — that is the pre-validation path and is expected here.
    let r = s
        .post_json(
            &format!("/api/jobs/{}/retry", VALID_UUID),
            json!({ "input_path": "\\\\evil.example\\share\\x.mp4" }),
        )
        .await;
    assert_eq!(
        r.status(),
        404,
        "unknown job is rejected before the path is touched"
    );
}
