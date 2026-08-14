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
    media_path.with_extension("uuid.json")
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

    let json = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize sidecar: {}", e))?;

    let tmp_sidecar_path = sidecar_path.with_extension("tmp_json");
    if let Err(e) = fs::write(&tmp_sidecar_path, &json) {
        let _ = fs::remove_file(&tmp_sidecar_path);
        return Err(format!(
            "Failed to write temporary sidecar '{}': {}",
            tmp_sidecar_path.display(),
            e
        ));
    }

    if let Err(e) = fs::rename(&tmp_sidecar_path, &sidecar_path) {
        let _ = fs::remove_file(&tmp_sidecar_path);
        return Err(format!(
            "Failed to rename temporary sidecar '{}' -> '{}': {}",
            tmp_sidecar_path.display(),
            sidecar_path.display(),
            e
        ));
    }

    tracing::info!(
        "Written UUID sidecar atomically: {} (id={})",
        sidecar_path.display(),
        uuid
    );
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
