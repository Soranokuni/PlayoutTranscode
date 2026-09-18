//! `GET /api/assets` is bounded (T2-7, F-06).
//!
//! The listing used to fetch every row and serialise all of it, including the
//! `keyframe_offsets` array on each — tens of kilobytes per asset. A library of
//! a few thousand produced a response that PlayOut's own 16 MiB cap rejected,
//! so the client simply failed to load the library it was pointed at.

mod common;

use playout_transcode::db;

/// Insert `n` live assets with a keyframe array big enough to matter.
async fn seed(pool: &sqlx::SqlitePool, n: usize) {
    // 400 offsets is a realistic half-hour programme at a 4 s GOP.
    let offsets: Vec<i64> = (0..400).map(|i| i * 4000).collect();
    let offsets_json = serde_json::to_string(&offsets).unwrap();

    for i in 0..n {
        let uuid = format!("{:08x}-0000-4000-8000-000000000000", i);
        db::insert_processing(pool, &uuid, i as i64, None, "D:/w/x.mxf", "Clip")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE media_assets SET status = 'ready', mezzanine_ok = 1,
             current_path = ?1, duration_ms = 1000, trim_out_ms = 1000,
             keyframe_offsets_json = ?2 WHERE uuid = ?3",
        )
        .bind(format!("D:/target/videos/{}.mp4", i))
        .bind(&offsets_json)
        .bind(&uuid)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn the_default_listing_is_capped_and_reports_the_true_total() {
    let s = common::spawn_test_server().await;
    // Comfortably over the 1 000 default, without making the test slow.
    seed(&s.pool, 1_200).await;

    let r = s.get("/api/assets").await;
    assert_eq!(r.status(), 200);

    let total = r
        .headers()
        .get("X-Total-Count")
        .expect("X-Total-Count tells the client whether to page")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(total, "1200");

    let body: Vec<serde_json::Value> = r.json().await.unwrap();
    assert_eq!(
        body.len(),
        1_000,
        "the default page is 1000, not the whole library"
    );
}

#[tokio::test]
async fn the_default_listing_omits_keyframe_offsets() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 3).await;

    let body: Vec<serde_json::Value> = s.get_json("/api/assets").await.as_array().unwrap().clone();
    assert_eq!(body.len(), 3);
    for a in &body {
        assert_eq!(
            a["keyframe_offsets"].as_array().unwrap().len(),
            0,
            "a listing must not carry the largest field on the row: {}",
            a
        );
    }

    // ...but the field is still present, so a client deserialising into a typed
    // struct with a non-optional Vec keeps working.
    assert!(body[0].get("keyframe_offsets").is_some());
}

#[tokio::test]
async fn fields_full_restores_the_offsets() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 2).await;

    let body = s.get_json("/api/assets?fields=full").await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(
        arr[0]["keyframe_offsets"].as_array().unwrap().len(),
        400,
        "?fields=full is the opt-in for clients that really do want them"
    );
}

#[tokio::test]
async fn single_asset_resolve_always_carries_the_offsets() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 1).await;

    // This is the call PlayOut's per-asset hydration makes, and it must be
    // unaffected by the listing slim-down or trimming breaks in the client.
    let uuid = "00000000-0000-4000-8000-000000000000";
    let asset = s.get_json(&format!("/api/assets/{}", uuid)).await;
    assert_eq!(
        asset["keyframe_offsets"].as_array().unwrap().len(),
        400,
        "single-asset resolve is the contract PlayOut trims against"
    );
}

#[tokio::test]
async fn limit_and_offset_page_through_the_library_without_gaps_or_repeats() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 25).await;

    let mut seen: Vec<String> = Vec::new();
    for offset in (0..25).step_by(10) {
        let page = s
            .get_json(&format!("/api/assets?limit=10&offset={}", offset))
            .await;
        for a in page.as_array().unwrap() {
            seen.push(a["uuid"].as_str().unwrap().to_string());
        }
    }

    assert_eq!(seen.len(), 25, "three pages must cover the library exactly");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 25, "no uuid may appear on two pages");
}

#[tokio::test]
async fn an_oversized_limit_is_clamped_rather_than_honoured() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 5).await;

    // A client asking for everything is the same unbounded response, just
    // spelled by the caller. It is clamped, not rejected, so the request still
    // succeeds and returns useful data.
    let r = s.get("/api/assets?limit=100000").await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers().get("X-Limit").unwrap().to_str().unwrap(),
        "5000",
        "X-Limit reports what was actually applied, not what was asked for"
    );
}

#[tokio::test]
async fn nonsense_paging_parameters_do_not_error() {
    let s = common::spawn_test_server().await;
    seed(&s.pool, 3).await;

    for q in [
        "?limit=0",
        "?limit=-5",
        "?offset=-1",
        "?limit=abc",
        "?offset=abc",
    ] {
        let r = s.get(&format!("/api/assets{}", q)).await;
        assert_eq!(r.status(), 200, "{} should degrade to a default, not 500", q);
    }
}

#[test]
fn the_limit_clamp_is_total() {
    assert_eq!(db::clamp_asset_limit(None), db::ASSETS_DEFAULT_LIMIT);
    assert_eq!(db::clamp_asset_limit(Some(0)), 1);
    assert_eq!(db::clamp_asset_limit(Some(-100)), 1);
    assert_eq!(db::clamp_asset_limit(Some(50)), 50);
    assert_eq!(db::clamp_asset_limit(Some(i64::MAX)), db::ASSETS_MAX_LIMIT);
}
