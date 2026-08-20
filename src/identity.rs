use crate::probe::ProbeData;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoudnessInfo {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub lra: f64,
    pub threshold: f64,
    pub target_lufs: f64,
    pub target_true_peak_dbtp: f64,
    pub normalization_mode: String,
    pub linear_applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationFinding {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
}

impl ValidationFinding {
    #[allow(dead_code)]
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            code: code.into(),
            message: message.into(),
            measured: None,
            expected: None,
        }
    }

    pub fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        measured: Option<String>,
        expected: Option<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.into(),
            message: message.into(),
            measured,
            expected,
        }
    }

    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        measured: Option<String>,
        expected: Option<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            measured,
            expected,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QcReport {
    pub passed: bool,
    pub blocking_errors: usize,
    pub warnings_count: usize,
    pub findings: Vec<ValidationFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationReport {
    pub mezzanine_ok: bool,
    pub duration_ms: i64,
    pub fps: f64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub audio_sample_rate: i64,
    pub audio_channels: i64,
    pub closed_gop: bool,
    pub faststart: bool,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<ValidationFinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc_report: Option<QcReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarPayload {
    pub playoutvue_id: String,
    pub id: String,
    pub path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub mezzanine_ok: bool,
    pub filename: String,
    pub filepath: String,
    pub transcoded_at: String,
    pub profile_used: String,
    pub original_source: SourceInfo,
    pub output_media: OutputInfo,
    pub fps: f64,
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<ValidationFinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc_report: Option<QcReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<LoudnessInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_report: Option<ValidationReport>,
}

impl SidecarPayload {
    pub fn new(
        uuid: &str,
        path: &str,
        source_probe: &ProbeData,
        output_probe: &ProbeData,
        profile_name: &str,
        target_codec: &str,
        target_audio_codec: &str,
        duration_ms: i64,
        mezzanine_ok: bool,
        fps: f64,
        fps_num: i64,
        fps_den: i64,
        total_frames: i64,
        gop_frames: i64,
        keyframe_safe_start_ms: i64,
        warnings: &[String],
        loudness: Option<LoudnessInfo>,
    ) -> Self {
        let p = Path::new(path);
        let filename = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        Self {
            playoutvue_id: uuid.to_string(),
            id: uuid.to_string(),
            path: path.to_string(),
            duration_ms,
            trim_in_ms: 0,
            trim_out_ms: duration_ms,
            fps_num,
            fps_den,
            mezzanine_ok,
            filename,
            filepath: path.to_string(),
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
                audio_sample_rate: output_probe.audio_sample_rate,
                audio_channels: output_probe.audio_channels,
                fps_num: output_probe.fps_num,
                fps_den: output_probe.fps_den,
            },
            fps,
            total_frames,
            gop_frames,
            keyframe_safe_start_ms,
            warnings: warnings.to_vec(),
            findings: None,
            qc_report: None,
            loudness,
            sha256: None,
            file_size_bytes: None,
            validation_report: None,
        }
    }

    pub fn with_validation(
        mut self,
        validation: ValidationReport,
        sha256: Option<String>,
        file_size_bytes: Option<u64>,
    ) -> Self {
        self.findings = validation.findings.clone();
        self.qc_report = validation.qc_report.clone();
        self.validation_report = Some(validation);
        self.sha256 = sha256;
        self.file_size_bytes = file_size_bytes;
        self
    }
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
    pub audio_sample_rate: i64,
    pub audio_channels: i64,
    pub fps_num: i64,
    pub fps_den: i64,
}

pub fn sidecar_path_for(media_path: &Path) -> PathBuf {
    let sidecar_filename = match media_path.file_stem() {
        Some(stem) => format!("{}.uuid.json", stem.to_string_lossy()),
        None => "metadata.uuid.json".to_string(),
    };

    if let Some(parent) = media_path.parent() {
        // 1. If media_path is in a "videos" directory: e.g. <root>/videos/clip.mp4 -> <root>/sidecars/clip.uuid.json
        if parent.file_name().map(|n| n == "videos").unwrap_or(false) {
            if let Some(grandparent) = parent.parent() {
                let sidecars_sibling = grandparent.join("sidecars").join(&sidecar_filename);
                if sidecars_sibling.exists() {
                    return sidecars_sibling;
                }
                let legacy_adjacent = media_path.with_extension("uuid.json");
                if legacy_adjacent.exists() {
                    return legacy_adjacent;
                }
                return sidecars_sibling;
            }
        }

        // 2. Check if subfolder <parent>/sidecars/<sidecar_filename> exists:
        let sidecars_subfolder = parent.join("sidecars").join(&sidecar_filename);
        if sidecars_subfolder.exists() {
            return sidecars_subfolder;
        }

        let legacy_adjacent = media_path.with_extension("uuid.json");
        return legacy_adjacent;
    }

    media_path.with_extension("uuid.json")
}

pub fn write_sidecar_payload(sidecar_path: &Path, payload: &SidecarPayload) -> Result<PathBuf, String> {
    if let Some(parent) = sidecar_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            let err = format!(
                "Failed to create sidecar directory '{}': {}",
                parent.display(),
                e
            );
            tracing::error!("{}", err);
            return Err(err);
        }
    }

    let json = serde_json::to_string_pretty(payload)
        .map_err(|e| format!("Failed to serialize sidecar: {}", e))?;

    let tmp_sidecar_path = if let Some(parent) = sidecar_path.parent() {
        let sidecar_name = sidecar_path.file_name().unwrap_or_default().to_string_lossy();
        parent.join(format!(".tmp_{}_{}.tmp_json", payload.id, sidecar_name))
    } else {
        sidecar_path.with_extension("tmp_json")
    };

    if let Err(e) = fs::write(&tmp_sidecar_path, &json) {
        let _ = fs::remove_file(&tmp_sidecar_path);
        let err = format!(
            "Failed to write temporary sidecar '{}': {}",
            tmp_sidecar_path.display(),
            e
        );
        tracing::error!("{}", err);
        return Err(err);
    }

    if let Err(e) = fs::rename(&tmp_sidecar_path, sidecar_path) {
        let _ = fs::remove_file(&tmp_sidecar_path);
        let err = format!(
            "Failed to rename temporary sidecar '{}' -> '{}': {}",
            tmp_sidecar_path.display(),
            sidecar_path.display(),
            e
        );
        tracing::error!("{}", err);
        return Err(err);
    }

    tracing::info!(
        "Written UUID sidecar atomically: {} (id={})",
        sidecar_path.display(),
        payload.id
    );
    Ok(sidecar_path.to_path_buf())
}

pub fn build_sidecar_from_db_asset(asset: &crate::db::MediaAsset) -> Result<PathBuf, String> {
    if asset.current_path.is_empty() {
        return Err("Asset current_path is empty".to_string());
    }
    let media_path = Path::new(&asset.current_path);
    let sidecar_path = sidecar_path_for(media_path);

    let filename = media_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let warnings: Vec<String> = serde_json::from_str(&asset.warnings).unwrap_or_default();
    let file_size_bytes = std::fs::metadata(media_path).ok().map(|m| m.len());
    let duration_secs = asset.duration_ms as f64 / 1000.0;

    let payload = SidecarPayload {
        playoutvue_id: asset.uuid.clone(),
        id: asset.uuid.clone(),
        path: asset.current_path.clone(),
        duration_ms: asset.duration_ms,
        trim_in_ms: asset.trim_in_ms,
        trim_out_ms: asset.trim_out_ms,
        fps_num: asset.fps_num,
        fps_den: asset.fps_den,
        mezzanine_ok: asset.mezzanine_ok,
        filename,
        filepath: asset.current_path.clone(),
        transcoded_at: Utc::now().to_rfc3339(),
        profile_used: "rebuilt_from_db".to_string(),
        original_source: SourceInfo {
            path: asset.current_path.clone(),
            codec: "h264".to_string(),
            duration_secs,
            frame_count: asset.total_frames,
            width: 1920,
            height: 1080,
            fps: asset.fps,
            fps_num: asset.fps_num,
            fps_den: asset.fps_den,
            field_order: "progressive".to_string(),
        },
        output_media: OutputInfo {
            duration_secs,
            frame_count: asset.total_frames,
            width: 1920,
            height: 1080,
            codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: asset.fps_num,
            fps_den: asset.fps_den,
        },
        fps: asset.fps,
        total_frames: asset.total_frames,
        gop_frames: asset.gop_frames,
        keyframe_safe_start_ms: asset.keyframe_safe_start_ms,
        warnings,
        findings: None,
        qc_report: None,
        loudness: None,
        sha256: None,
        file_size_bytes,
        validation_report: None,
    };

    write_sidecar_payload(&sidecar_path, &payload)
}

#[allow(dead_code)]
pub fn write_sidecar_next_to_video(
    output_path: &Path,
    uuid: &str,
    source_probe: &ProbeData,
    output_probe: &ProbeData,
    profile_name: &str,
    target_codec: &str,
    target_audio_codec: &str,
    duration_ms: i64,
    mezzanine_ok: bool,
    fps: f64,
    fps_num: i64,
    fps_den: i64,
    total_frames: i64,
    gop_frames: i64,
    keyframe_safe_start_ms: i64,
    warnings: &[String],
    loudness: Option<LoudnessInfo>,
) -> Result<PathBuf, String> {
    write_sidecar_next_to_video_with_validation(
        output_path,
        uuid,
        source_probe,
        output_probe,
        profile_name,
        target_codec,
        target_audio_codec,
        duration_ms,
        mezzanine_ok,
        fps,
        fps_num,
        fps_den,
        total_frames,
        gop_frames,
        keyframe_safe_start_ms,
        warnings,
        loudness,
        None,
        None,
        None,
    )
}

pub fn write_sidecar_next_to_video_with_validation(
    output_path: &Path,
    uuid: &str,
    source_probe: &ProbeData,
    output_probe: &ProbeData,
    profile_name: &str,
    target_codec: &str,
    target_audio_codec: &str,
    duration_ms: i64,
    mezzanine_ok: bool,
    fps: f64,
    fps_num: i64,
    fps_den: i64,
    total_frames: i64,
    gop_frames: i64,
    keyframe_safe_start_ms: i64,
    warnings: &[String],
    loudness: Option<LoudnessInfo>,
    validation: Option<ValidationReport>,
    sha256: Option<String>,
    file_size_bytes: Option<u64>,
) -> Result<PathBuf, String> {
    let sidecar_path = sidecar_path_for(output_path);
    let mut payload = SidecarPayload::new(
        uuid,
        &output_path.to_string_lossy(),
        source_probe,
        output_probe,
        profile_name,
        target_codec,
        target_audio_codec,
        duration_ms,
        mezzanine_ok,
        fps,
        fps_num,
        fps_den,
        total_frames,
        gop_frames,
        keyframe_safe_start_ms,
        warnings,
        loudness,
    );

    if let Some(val) = validation {
        payload = payload.with_validation(val, sha256, file_size_bytes);
    } else {
        payload.sha256 = sha256;
        payload.file_size_bytes = file_size_bytes;
    }

    write_sidecar_payload(&sidecar_path, &payload)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidecar_path_resolves_to_sibling_sidecars() {
        let media = Path::new("C:/media/target/videos/clip_abc.mp4");
        let sidecar = sidecar_path_for(media);
        let normalized = sidecar.to_string_lossy().replace('\\', "/");
        assert_eq!(normalized, "C:/media/target/sidecars/clip_abc.uuid.json");
    }

    #[test]
    fn test_sidecar_path_generic_folder_defaults_to_adjacent() {
        let media = Path::new("C:/media/custom_folder/clip_abc.mp4");
        let sidecar = sidecar_path_for(media);
        let normalized = sidecar.to_string_lossy().replace('\\', "/");
        assert_eq!(normalized, "C:/media/custom_folder/clip_abc.uuid.json");
    }

    #[test]
    fn test_build_sidecar_from_db_asset_creates_file() {
        let temp_dir = std::env::temp_dir().join(format!("pt_sidecar_test_{}", uuid::Uuid::new_v4()));
        let videos_dir = temp_dir.join("videos");
        std::fs::create_dir_all(&videos_dir).unwrap();
        let media_file = videos_dir.join("test_clip.mp4");
        std::fs::write(&media_file, b"fake video content").unwrap();

        let asset = crate::db::MediaAsset {
            uuid: "test-uuid-1234".to_string(),
            fingerprint: 12345,
            current_path: media_file.to_string_lossy().into_owned(),
            duration_ms: 10000,
            trim_in_ms: 0,
            trim_out_ms: 10000,
            rating: "12".to_string(),
            tp: "None".to_string(),
            status: "ready".to_string(),
            display_name: "Test Clip".to_string(),
            virtual_folder: "/".to_string(),
            mezzanine_ok: true,
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            total_frames: 250,
            gop_frames: 50,
            keyframe_safe_start_ms: 0,
            warnings: "[]".to_string(),
            keyframe_offsets_json: "[]".to_string(),
            deleted_at: None,
            original_virtual_folder: None,
        };

        let result = build_sidecar_from_db_asset(&asset).unwrap();
        let normalized = result.to_string_lossy().replace('\\', "/");
        assert!(normalized.ends_with("sidecars/test_clip.uuid.json"));
        assert!(result.exists(), "Sidecar file must exist on disk");

        let content = std::fs::read_to_string(&result).unwrap();
        let parsed: SidecarPayload = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.id, "test-uuid-1234");
        assert_eq!(parsed.duration_ms, 10000);
        assert_eq!(parsed.fps_num, 25);
        assert_eq!(parsed.fps_den, 1);
        assert!(parsed.mezzanine_ok);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
