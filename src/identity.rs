use crate::probe::ProbeData;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarPayload {
    pub playoutvue_id: String,
    pub filename: String,
    pub filepath: String,
    pub transcoded_at: String,
    pub profile_used: String,
    pub original_source: SourceInfo,
    pub output_media: OutputInfo,
    #[serde(default)]
    pub mezzanine_ok: bool,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub total_frames: i64,
    #[serde(default)]
    pub gop_frames: i64,
    #[serde(default)]
    pub keyframe_safe_start_ms: i64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    pub codec: String,
    pub duration_secs: f64,
    pub frame_count: i64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub field_order: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub duration_secs: f64,
    pub frame_count: i64,
    pub width: i64,
    pub height: i64,
    pub codec: String,
    pub audio_codec: String,
    pub fps_num: i64,
    pub fps_den: i64,
}

pub fn sidecar_path_for(media_path: &Path) -> PathBuf {
    media_path.with_extension("uuid.json")
}

pub fn write_sidecar_next_to_video(
    output_path: &Path,
    uuid: &str,
    source_probe: &ProbeData,
    output_probe: &ProbeData,
    profile_name: &str,
    target_codec: &str,
    target_audio_codec: &str,
    mezzanine_ok: bool,
    fps: f64,
    total_frames: i64,
    gop_frames: i64,
    keyframe_safe_start_ms: i64,
    warnings: &[String],
) -> Result<PathBuf, String> {
    let sidecar_path = sidecar_path_for(output_path);
    let filename = output_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let payload = SidecarPayload {
        playoutvue_id: uuid.to_string(),
        filename: filename.clone(),
        filepath: output_path.to_string_lossy().into_owned(),
        transcoded_at: Utc::now().to_rfc3339(),
        profile_used: profile_name.to_string(),
        original_source: SourceInfo {
            path: source_probe.input_path.clone(),
            codec: source_probe.video_codec.clone(),
            duration_secs: source_probe.duration_secs,
            frame_count: source_probe.frame_count,
            width: source_probe.width,
            height: source_probe.height,
            fps: source_probe.fps(),
            fps_num: source_probe.fps_num,
            fps_den: source_probe.fps_den,
            field_order: source_probe.field_order.clone(),
        },
        output_media: OutputInfo {
            duration_secs: output_probe.duration_secs,
            frame_count: output_probe.frame_count,
            width: output_probe.width,
            height: output_probe.height,
            codec: target_codec.to_string(),
            audio_codec: target_audio_codec.to_string(),
            fps_num: output_probe.fps_num,
            fps_den: output_probe.fps_den,
        },
        mezzanine_ok,
        fps,
        total_frames,
        gop_frames,
        keyframe_safe_start_ms,
        warnings: warnings.to_vec(),
    };

    let json = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize sidecar: {}", e))?;

    fs::write(&sidecar_path, json)
        .map_err(|e| format!("Failed to write sidecar '{}': {}", sidecar_path.display(), e))?;

    tracing::info!("Written UUID sidecar: {} (id={})", sidecar_path.display(), uuid);
    Ok(sidecar_path)
}

pub fn sanitize_filename(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else if c.is_ascii_whitespace() {
                '_'
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_lowercase()
}
