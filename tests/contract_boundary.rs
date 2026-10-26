//! The PlayOut boundary, tested against what this crate really serves (C-19).
//!
//! These tests used to define their own `AssetResponse`, `hydrate_item` and
//! `compute_frame_trim` and test those, so they stayed green through any
//! rename or status change on the server. Now every asset is produced by the
//! real `db` functions and fetched through the real router, and decoded into a
//! mirror of the DTOs PlayOut deserialises (`src-tauri/src/ingestor_api.rs`):
//! its required fields are required here too, so a renamed or dropped field,
//! or a status PlayOut does not know, fails this file instead of a take.
//! PlayOut's own trim maths is tested in PlayOut.

// The shared test server, at the crate root: a `#[path]` inside an inline
// module resolves under a folder of the module's name, which only Windows
// forgives.
mod common;

mod playout_contract {
    use super::common;
    use playout_transcode::db;
    use serde::Deserialize;
    use std::collections::HashMap;

    /// PlayOut's `AssetResponse` / `V2AssetDto`. No `deny_unknown_fields`:
    /// PlayOut ignores fields it does not know, so additive changes are fine.
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)] // fields that only have to decode
    struct PlayoutAsset {
        uuid: String,
        #[serde(default)]
        playoutvue_id: Option<String>,
        current_path: String,
        duration_ms: i64,
        trim_in_ms: i64,
        trim_out_ms: i64,
        rating: String,
        tp: String,
        status: String,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        virtual_folder: Option<String>,
        #[serde(default)]
        mezzanine_ok: Option<bool>,
        #[serde(default)]
        fps_num: Option<i64>,
        #[serde(default)]
        fps_den: Option<i64>,
        #[serde(default)]
        total_frames: Option<i64>,
        #[serde(default)]
        gop_frames: Option<i64>,
        #[serde(default)]
        keyframe_safe_start_ms: Option<i64>,
        #[serde(default)]
        warnings: Option<Vec<String>>,
        #[serde(default)]
        parent_uuid: Option<String>,
        #[serde(default)]
        qc_report: Option<PlayoutQcReport>,
        #[serde(default)]
        loudness: Option<PlayoutLoudness>,
    }

    /// PlayOut's `V2QcReportDto` and `V2QcFindingDto`.
    #[derive(Debug, Deserialize)]
    struct PlayoutQcReport {
        passed: bool,
        #[serde(default)]
        blocking_errors: usize,
        #[serde(default)]
        findings: Vec<PlayoutQcFinding>,
    }

    #[derive(Debug, Deserialize)]
    struct PlayoutQcFinding {
        severity: String,
        code: String,
        #[allow(dead_code)]
        message: String,
    }

    /// PlayOut's `V2LoudnessDto`.
    #[derive(Debug, Deserialize)]
    struct PlayoutLoudness {
        #[serde(default)]
        integrated_lufs: Option<f64>,
        #[serde(default)]
        lra_lu: Option<f64>,
        #[serde(default)]
        mode: Option<String>,
    }

    /// The `IngestorStatus` values PlayOut has a branch for.
    const PLAYOUT_STATUSES: [&str; 4] = ["processing", "ready", "error", "missing"];

    async fn publish(
        s: &common::TestServer,
        uuid: &str,
        file: &str,
        duration_ms: i64,
        fps_num: i64,
        fps_den: i64,
        mezzanine_ok: bool,
    ) -> String {
        let path = s.target_dir.join(file).to_string_lossy().to_string();
        db::insert_processing(&s.pool, uuid, duration_ms, None, &path, file)
            .await
            .expect("insert");
        let fps = fps_num as f64 / fps_den as f64;
        let total_frames = (duration_ms * fps_num) / (1000 * fps_den);
        db::mark_ready(
            &s.pool,
            uuid,
            &path,
            duration_ms,
            mezzanine_ok,
            fps,
            fps_num,
            fps_den,
            total_frames,
            (fps * 2.0).round() as i64,
            0,
            &[],
            "[0,2000,4000,6000,8000]",
            None,
        )
        .await
        .expect("mark_ready");
        path
    }

    /// The asset through both single-asset routes PlayOut calls, which must agree.
    async fn fetch(s: &common::TestServer, uuid: &str) -> PlayoutAsset {
        let v2 = s.get(&format!("/api/v2/assets/{uuid}")).await;
        assert_eq!(v2.status(), 200, "v2 asset {uuid}");
        let v2_text = v2.text().await.unwrap();
        let v1 = s.get(&format!("/api/assets/{uuid}")).await;
        assert_eq!(v1.status(), 200, "v1 asset {uuid}");
        let v1_text = v1.text().await.unwrap();
        let asset: PlayoutAsset = serde_json::from_str(&v2_text)
            .unwrap_or_else(|e| panic!("PlayOut cannot decode {uuid} ({e}): {v2_text}"));
        let _: PlayoutAsset = serde_json::from_str(&v1_text)
            .unwrap_or_else(|e| panic!("PlayOut cannot decode v1 {uuid} ({e}): {v1_text}"));
        assert!(
            PLAYOUT_STATUSES.contains(&asset.status.as_str()),
            "status {}",
            asset.status
        );
        asset
    }

    #[tokio::test]
    async fn a_ready_asset_meets_what_playout_relies_on() {
        let s = common::spawn_test_server().await;
        let uuid = "a1b2c3d4-e5f6-4890-abcd-ef1234567890";
        let path = publish(&s, uuid, "clip.mp4", 10_000, 25, 1, true).await;

        let a = fetch(&s, uuid).await;
        assert_eq!(a.status, "ready");
        assert_eq!(
            a.mezzanine_ok,
            Some(true),
            "a ready asset is always mezzanine_ok"
        );
        assert_eq!(a.uuid, uuid);
        assert_eq!(
            a.playoutvue_id.as_deref(),
            Some(uuid),
            "uuid and playoutvue_id are one identity"
        );
        assert_eq!(a.current_path, path, "the final playable path");
        assert_eq!(
            (a.duration_ms, a.trim_in_ms, a.trim_out_ms),
            (10_000, 0, 10_000)
        );
        assert_eq!((a.fps_num, a.fps_den), (Some(25), Some(1)));
        assert!(a.parent_uuid.is_none(), "a full-length asset has no parent");
    }

    #[tokio::test]
    async fn an_ntsc_rate_is_served_as_the_exact_rational() {
        let s = common::spawn_test_server().await;
        let uuid = "b2c3d4e5-f6a7-4901-bcde-f12345678901";
        publish(&s, uuid, "ntsc.mp4", 10_010, 30_000, 1001, true).await;

        let a = fetch(&s, uuid).await;
        assert_eq!(
            (a.fps_num, a.fps_den),
            (Some(30_000), Some(1001)),
            "never 29970/1000"
        );
    }

    #[tokio::test]
    async fn a_qc_failed_mezzanine_is_error_never_ready() {
        let s = common::spawn_test_server().await;
        let uuid = "c3d4e5f6-a7b8-4012-8def-123456789012";
        publish(&s, uuid, "bad.mp4", 10_000, 25, 1, false).await;

        let a = fetch(&s, uuid).await;
        assert_eq!(a.status, "error");
        assert_eq!(a.mezzanine_ok, Some(false));
    }

    #[tokio::test]
    async fn a_subclip_names_its_parent_and_plays_its_file_with_its_own_window() {
        let s = common::spawn_test_server().await;
        let parent = "d4e5f6a7-b8c9-4123-9efa-234567890123";
        let sub = "e5f6a7b8-c9d0-4234-afab-345678901234";
        let path = publish(&s, parent, "parent.mp4", 60_000, 25, 1, true).await;
        db::create_subclip(&s.pool, sub, parent, "Sub", 4_000, 14_000, true, "[]")
            .await
            .expect("create")
            .expect("parent exists");

        let a = fetch(&s, sub).await;
        assert_eq!(a.parent_uuid.as_deref(), Some(parent));
        assert_eq!(a.current_path, path, "a sub-clip plays its parent's file");
        assert_eq!(
            (a.duration_ms, a.trim_in_ms, a.trim_out_ms),
            (60_000, 4_000, 14_000)
        );
        assert_eq!(a.status, "ready");

        // The batch route PlayOut resolves a rundown with: a map by uuid.
        let r = s
            .post_json("/api/assets/batch", serde_json::json!([parent, sub]))
            .await;
        assert_eq!(r.status(), 200);
        let batch: HashMap<String, PlayoutAsset> =
            r.json().await.expect("PlayOut decodes the batch");
        assert_eq!(batch[sub].parent_uuid.as_deref(), Some(parent));
        assert!(batch[parent].parent_uuid.is_none());
    }

    #[tokio::test]
    async fn the_qc_report_and_loudness_decode_as_playout_reads_them() {
        let s = common::spawn_test_server().await;
        let uuid = "f6a7b8c9-d0e1-4345-babc-456789012345";
        publish(&s, uuid, "loud.mp4", 10_000, 25, 1, true).await;
        let qc = serde_json::json!({
            "passed": true, "blocking_errors": 0, "warnings_count": 1,
            "findings": [{ "severity": "warning", "code": "loudness_dynamic_mode", "message": "dynamic" }]
        });
        let loudness = serde_json::to_string(&db::AssetLoudness {
            integrated_lufs: -23.0,
            true_peak_dbtp: -1.5,
            lra_lu: 7.0,
            mode: "ebu_r128".into(),
        })
        .unwrap();
        db::record_qc_and_loudness(&s.pool, uuid, Some(&qc.to_string()), Some(&loudness))
            .await
            .unwrap();

        let a = fetch(&s, uuid).await;
        let report = a.qc_report.expect("qc_report served");
        assert!(report.passed);
        assert_eq!(report.blocking_errors, 0, "PlayOut's QC gate reads this");
        assert_eq!(report.findings[0].severity, "warning");
        assert_eq!(report.findings[0].code, "loudness_dynamic_mode");
        let l = a.loudness.expect("loudness served");
        assert_eq!(
            (l.integrated_lufs, l.lra_lu, l.mode.as_deref()),
            (Some(-23.0), Some(7.0), Some("ebu_r128"))
        );
    }

    /// C-14. PlayOut's v2 path reads `trim_out_ms = 0` as invalid bounds and
    /// its v1/batch path as the whole file. After the startup migration no
    /// published asset is served with it, on any route.
    #[tokio::test]
    async fn a_legacy_unset_out_point_is_served_as_the_whole_file() {
        let s = common::spawn_test_server().await;
        let uuid = "a7b8c9d0-e1f2-4456-8bcd-567890123456";
        let path = publish(&s, uuid, "legacy.mp4", 20_000, 25, 1, true).await;
        // As a pre-`mark_ready`-OUT version left it.
        sqlx::query("UPDATE media_assets SET trim_out_ms = 0 WHERE uuid = ?1")
            .bind(uuid)
            .execute(&*s.pool)
            .await
            .unwrap();

        db::run_data_migrations(&s.pool, &s.root.join("media_assets.db"))
            .await
            .unwrap();

        let a = fetch(&s, uuid).await;
        assert_eq!((a.status.as_str(), a.trim_in_ms, a.trim_out_ms), ("ready", 0, 20_000));
        assert_eq!(a.current_path, path);
        let r = s.post_json("/api/assets/batch", serde_json::json!([uuid])).await;
        let batch: HashMap<String, PlayoutAsset> = r.json().await.expect("PlayOut decodes the batch");
        assert_eq!(batch[uuid].trim_out_ms, 20_000);
    }
}

/// Contract C1: `GET /api/v2/media-root`, against the real router.
///
/// PlayOut deserialises exactly this shape to verify that the folder it and
/// CasparCG see is this registry's target folder. `deny_unknown_fields` is
/// deliberate: a new key is a contract change and must be made in both repos.
mod media_root_contract {
    use super::common;

    use playout_transcode::{db, media_root};
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MediaRootLayout {
        videos: String,
        sidecars: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MediaRootResponse {
        registry_id: String,
        target_folder: String,
        host: String,
        layout: MediaRootLayout,
        marker_file: String,
        claimed: bool,
    }

    async fn fetch(s: &common::TestServer) -> MediaRootResponse {
        let r = s.get("/api/v2/media-root").await;
        assert_eq!(r.status(), 200);
        r.json().await.expect("C1 shape")
    }

    #[tokio::test]
    async fn media_root_reports_a_claimed_folder() {
        let s = common::spawn_test_server().await;
        let ours = db::registry_id(&s.pool).await.unwrap();
        media_root::claim(&s.target_dir, &ours).unwrap();

        let body = fetch(&s).await;
        assert_eq!(body.registry_id, ours);
        assert_eq!(body.target_folder, s.target_dir.to_string_lossy());
        assert!(!body.host.is_empty());
        assert_eq!(body.layout.videos, "videos");
        assert_eq!(body.layout.sidecars, "sidecars");
        assert_eq!(body.marker_file, ".playouttranscode-registry.json");
        assert_eq!(body.marker_file, media_root::MARKER_FILE_NAME);
        assert!(body.claimed);
    }

    #[tokio::test]
    async fn media_root_is_unclaimed_without_our_marker() {
        let s = common::spawn_test_server().await;
        let ours = db::registry_id(&s.pool).await.unwrap();

        // No marker yet: reading must not create one.
        assert!(!fetch(&s).await.claimed);
        assert!(!s.target_dir.join(media_root::MARKER_FILE_NAME).exists());

        // Another registry's marker.
        media_root::claim(&s.target_dir, "some-other-registry").unwrap();
        let body = fetch(&s).await;
        assert_eq!(body.registry_id, ours);
        assert!(!body.claimed);
    }
}
