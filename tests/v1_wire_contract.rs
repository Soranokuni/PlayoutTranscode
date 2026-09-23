// tests/v1_wire_contract.rs
// Baseline wire contract integration tests for PlayoutTranscode V2-0.
// Validates golden JSON contract samples and exercises a live Axum HTTP server endpoint stream.

mod common;

use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("contracts")
}

fn read_contract_json(filename: &str) -> Value {
    let path = contracts_dir().join(filename);
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read contract sample {}: {}", path.display(), e));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse JSON sample {}: {}", path.display(), e))
}

#[test]
fn test_golden_asset_response_contract() {
    let json = read_contract_json("asset-response.example.json");

    assert_eq!(
        json["uuid"], json["playoutvue_id"],
        "uuid and playoutvue_id must match"
    );
    assert!(
        json["duration_ms"].as_i64().unwrap() > 0,
        "duration_ms must be > 0 on ready"
    );
    assert!(
        json["trim_in_ms"].as_i64().unwrap() >= 0,
        "trim_in_ms must be >= 0"
    );
    assert!(
        json["trim_out_ms"].as_i64().unwrap() > json["trim_in_ms"].as_i64().unwrap(),
        "trim_out_ms must be > trim_in_ms"
    );
    assert!(
        json["trim_out_ms"].as_i64().unwrap() <= json["duration_ms"].as_i64().unwrap(),
        "trim_out_ms must be <= duration_ms"
    );
    assert!(json["fps_num"].as_i64().unwrap() > 0, "fps_num must be > 0");
    assert!(json["fps_den"].as_i64().unwrap() > 0, "fps_den must be > 0");
    assert_eq!(json["status"].as_str().unwrap(), "ready");
    assert!(json["warnings"].is_array(), "warnings must be a JSON array");
    assert!(
        json["keyframe_offsets"].is_array(),
        "keyframe_offsets must be a JSON array"
    );
}

#[test]
fn test_golden_asset_sidecar_contract() {
    let json = read_contract_json("asset-sidecar.example.json");

    assert_eq!(
        json["playoutvue_id"], json["id"],
        "playoutvue_id and id must match"
    );
    assert_eq!(
        json["filepath"], json["path"],
        "filepath and path must match"
    );
    assert!(json["duration_ms"].as_i64().unwrap() > 0);
    assert_eq!(
        json["output_media"]["audio_sample_rate"].as_i64().unwrap(),
        48000
    );
    assert_eq!(json["output_media"]["audio_channels"].as_i64().unwrap(), 2);
}

#[test]
fn test_golden_config_contract() {
    let json = read_contract_json("config.example.json");

    assert!(json["paths"]["watch_folder"].is_string());
    assert!(json["paths"]["target_folder"].is_string());
    assert_eq!(json["server"]["web_port"].as_i64().unwrap(), 4353);
    assert_eq!(json["encoding"]["preset"].as_str().unwrap(), "medium");
    assert_eq!(json["encoding"]["audio_codec"].as_str().unwrap(), "aac");
    assert_eq!(json["encoding"]["audio_bitrate"].as_str().unwrap(), "320k");
    assert_eq!(json["profiles"]["a"]["enabled"].as_bool().unwrap(), true);
    assert_eq!(json["profiles"]["b"]["enabled"].as_bool().unwrap(), true);
    assert_eq!(json["profiles"]["c"]["enabled"].as_bool().unwrap(), true);
    assert_eq!(json["initialized"].as_bool().unwrap(), true);
}

#[test]
fn test_golden_health_contract() {
    let json = read_contract_json("health.example.json");

    assert_eq!(json["status"].as_str().unwrap(), "ok");
    assert_eq!(json["service"].as_str().unwrap(), "PlayoutTranscode");
    assert!(json["version"].is_string());
    assert!(json["toolchain_ready"].is_boolean());
}

#[test]
fn test_golden_job_record_contract() {
    let json = read_contract_json("job-record.example.json");

    assert!(json["id"].is_string());
    assert!(json["input_path"].is_string());
    assert_eq!(json["state"].as_str().unwrap(), "Completed");
    assert_eq!(json["current_stage"].as_str().unwrap(), "Completed");
    assert_eq!(json["progress"].as_f64().unwrap(), 100.0);
}

#[test]
fn test_golden_sse_event_envelope_contract() {
    let json = read_contract_json("sse-event-envelope.example.json");
    let arr = json.as_array().expect("SSE envelope sample must be array");

    let event_names: Vec<&str> = arr.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert!(event_names.contains(&"job_update"));
    assert!(event_names.contains(&"progress"));
    assert!(event_names.contains(&"completed"));
    assert!(event_names.contains(&"failed"));
}

#[test]
fn test_golden_stats_contract() {
    let json = read_contract_json("stats.example.json");

    assert!(json["pending"].is_number());
    assert!(json["active"].is_number());
    assert!(json["completed"].is_number());
    assert!(json["failed"].is_number());
    assert!(json["total"].is_number());
}

#[test]
fn test_golden_watchfolder_contract() {
    let json = read_contract_json("watchfolder.example.json");

    assert!(json["watch_folder"].is_string());
    assert!(json["target_folder"].is_string());
    assert!(json["settle_secs"].is_number());
    assert!(json["max_concurrency"].is_number());
}

/// Every key present in `sample` must also be present in `actual`, recursively
/// for objects. Values are not compared — the golden-sample tests above already
/// pin the contract's values; this pins that the *real* handlers still emit the
/// contract's shape.
fn assert_covers_contract(actual: &Value, sample: &Value, path: &str) {
    match sample {
        Value::Object(fields) => {
            let actual_obj = actual.as_object().unwrap_or_else(|| {
                panic!("{} should be an object, got {}", path, actual)
            });
            for (key, sub) in fields {
                let child = actual_obj
                    .get(key)
                    .unwrap_or_else(|| panic!("{}.{} missing from live response", path, key));
                assert_covers_contract(child, sub, &format!("{}.{}", path, key));
            }
        }
        Value::Array(_) => {
            assert!(actual.is_array(), "{} should be an array, got {}", path, actual);
        }
        _ => {}
    }
}

// Live Axum HTTP Server Wire Contract Test
//
// These drive the real router, real handlers and real state via the shared
// harness in tests/common. They used to build stub routers returning the golden
// JSON verbatim, which proved only that axum can serve a constant (F-31).
#[tokio::test]
async fn test_live_axum_wire_contract_endpoints() {
    let s = common::spawn_test_server().await;

    // 1. GET /api/health
    let health = s.get_json("/api/health").await;
    assert_covers_contract(&health, &read_contract_json("health.example.json"), "health");
    assert_eq!(health["status"], "ok");
    assert_eq!(health["service"], "PlayoutTranscode");

    // 2. GET /api/config
    let config = s.get_json("/api/config").await;
    assert_covers_contract(&config, &read_contract_json("config.example.json"), "config");
    assert_eq!(config["server"]["web_port"], 4353);

    // 3. GET /api/stats
    let stats = s.get_json("/api/stats").await;
    assert_covers_contract(&stats, &read_contract_json("stats.example.json"), "stats");
    // A fresh registry is empty, so every counter must be zero and consistent.
    assert_eq!(stats["total"], 0);

    // 4. GET /api/watchfolder
    let wf = s.get_json("/api/watchfolder").await;
    assert_covers_contract(
        &wf,
        &read_contract_json("watchfolder.example.json"),
        "watchfolder",
    );
    assert_eq!(wf["settle_secs"], 5);

    // 5. GET /api/assets
    let assets = s.get_json("/api/assets").await;
    assert!(assets.is_array());
    assert_eq!(assets.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_live_axum_v2_wire_contract_endpoints() {
    let s = common::spawn_test_server().await;

    // 1. GET /api/v2/health
    let health = s.get_json("/api/v2/health").await;
    assert_eq!(health["status"], "ok");
    assert_eq!(health["service"], "PlayoutTranscode");
    assert_eq!(health["api_version"], "2.0.0");
    assert!(health["uptime_secs"].is_number());

    // 2. GET /api/v2/profiles
    let profiles = s.get_json("/api/v2/profiles").await;
    assert!(profiles.is_array());
    let names: Vec<&str> = profiles
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(
        names.contains(&"playoutvue-h264-1080p25"),
        "profile list must still advertise the 1080p25 mezzanine profile, got {:?}",
        names
    );
    for p in profiles.as_array().unwrap() {
        assert!(p["width"].is_number(), "profile width: {}", p);
        assert!(p["height"].is_number(), "profile height: {}", p);
        assert!(p["fps_num"].is_number(), "profile fps_num: {}", p);
        assert!(p["fps_den"].is_number(), "profile fps_den: {}", p);
    }

    // 3. GET /api/v2/metrics
    let metrics = s.get_json("/api/v2/metrics").await;
    for key in ["pending", "active", "completed", "failed", "total"] {
        assert!(metrics["jobs"][key].is_number(), "metrics.jobs.{}", key);
    }
    assert_eq!(metrics["jobs"]["total"], 0);
    assert!(metrics["system"]["uptime_secs"].is_number());
    assert!(metrics["system"]["active_pids"].is_number());
    assert_eq!(metrics["system"]["service_running"], false);

    // 4. GET /api/v2/diagnostics
    let diag = s.get_json("/api/v2/diagnostics").await;
    assert_eq!(diag["service"]["name"], "PlayoutTranscode");
    assert_eq!(diag["service"]["api_version"], "2.0.0");
    assert_eq!(
        diag["database"]["integrity"], "ok",
        "a freshly created registry must pass its integrity check"
    );
}
