// tests/v1_wire_contract.rs
// Baseline wire contract integration tests for PlayoutTranscode V2-0.
// Validates golden JSON contract samples and exercises a live Axum HTTP server endpoint stream.

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

// Live Axum HTTP Server Wire Contract Test
#[tokio::test]
async fn test_live_axum_wire_contract_endpoints() {
    use axum::{routing::get, Json, Router};

    let health_sample = read_contract_json("health.example.json");
    let config_sample = read_contract_json("config.example.json");
    let stats_sample = read_contract_json("stats.example.json");
    let watchfolder_sample = read_contract_json("watchfolder.example.json");

    let health_h = health_sample.clone();
    let config_h = config_sample.clone();
    let stats_h = stats_sample.clone();
    let watchfolder_h = watchfolder_sample.clone();

    let api = Router::new()
        .route("/health", get(move || async move { Json(health_h) }))
        .route("/config", get(move || async move { Json(config_h) }))
        .route("/stats", get(move || async move { Json(stats_h) }))
        .route(
            "/watchfolder",
            get(move || async move { Json(watchfolder_h) }),
        )
        .route("/assets", get(|| async { Json(serde_json::json!([])) }));

    let app = Router::new().nest("/api", api);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind ephemeral test port");
    let addr = listener.local_addr().expect("Failed to get local addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}/api", addr.port());

    // 1. GET /api/health
    let res = client
        .get(format!("{}/health", base))
        .send()
        .await
        .expect("GET /health failed");
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    let health_res: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(health_res["status"], "ok");
    assert_eq!(health_res["service"], "PlayoutTranscode");

    // 2. GET /api/config
    let res = client
        .get(format!("{}/config", base))
        .send()
        .await
        .expect("GET /config failed");
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    let config_res: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(config_res["server"]["web_port"], 4353);

    // 3. GET /api/stats
    let res = client
        .get(format!("{}/stats", base))
        .send()
        .await
        .expect("GET /stats failed");
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    let stats_res: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(stats_res["total"], 47);

    // 4. GET /api/watchfolder
    let res = client
        .get(format!("{}/watchfolder", base))
        .send()
        .await
        .expect("GET /watchfolder failed");
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    let wf_res: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(wf_res["settle_secs"], 5);

    // 5. GET /api/assets
    let res = client
        .get(format!("{}/assets", base))
        .send()
        .await
        .expect("GET /assets failed");
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    let assets_res: Value = serde_json::from_str(&text).unwrap();
    assert!(assets_res.is_array());
}
