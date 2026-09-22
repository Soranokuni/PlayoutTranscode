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

/// Where the sidecar for a mezzanine lives.
///
/// **Deterministic** since T3-5: the answer depends only on `media_path`, never
/// on what happens to exist on disk. It used to probe the filesystem and return
/// whichever of three candidate locations it found first, which meant the same
/// asset resolved to different paths depending on call order and on whether a
/// previous write had landed — a write could create the file the *next* read
/// then found somewhere else, and a delete could silently retarget the next
/// write to a legacy location (F-28).
///
/// The rule is now flat:
///
/// * `<root>/videos/clip.mp4` -> `<root>/sidecars/clip.uuid.json`
/// * anything else            -> `<dir>/sidecars/clip.uuid.json`
///
/// Legacy sidecars written adjacent to the media (`clip.uuid.json` next to
/// `clip.mp4`) are moved into place once, at startup, by
/// [`migrate_legacy_sidecars`].
pub fn sidecar_path_for(media_path: &Path) -> PathBuf {
    let sidecar_filename = match media_path.file_stem() {
        Some(stem) => format!("{}.uuid.json", stem.to_string_lossy()),
        None => "metadata.uuid.json".to_string(),
    };

    let Some(parent) = media_path.parent() else {
        return media_path.with_extension("uuid.json");
    };

    // `<root>/videos/clip.mp4` -> `<root>/sidecars/clip.uuid.json`
    if parent.file_name().map(|n| n == "videos").unwrap_or(false) {
        if let Some(root) = parent.parent() {
            return root.join("sidecars").join(&sidecar_filename);
        }
    }

    parent.join("sidecars").join(&sidecar_filename)
}

/// The pre-T3-5 location: `clip.uuid.json` beside `clip.mp4`.
pub fn legacy_sidecar_path_for(media_path: &Path) -> PathBuf {
    media_path.with_extension("uuid.json")
}

/// What a sidecar migration sweep did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SidecarMigration {
    pub moved: usize,
    pub already_current: usize,
    pub failed: usize,
}

/// Move legacy adjacent sidecars to the canonical `sidecars/` directory.
///
/// Run once at startup over `<target>/videos`. Idempotent: a second run finds
/// nothing to do. A legacy file whose canonical counterpart already exists is
/// **left alone**, not overwritten — the canonical one is the newer of the two
/// by construction, and destroying it to honour a stale file would lose data.
pub fn migrate_legacy_sidecars(videos_dir: &Path) -> SidecarMigration {
    let mut out = SidecarMigration::default();

    let Ok(entries) = fs::read_dir(videos_dir) else {
        return out;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Only media files; the legacy sidecars themselves end `.uuid.json`.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.ends_with(".uuid.json") {
            continue;
        }

        let legacy = legacy_sidecar_path_for(&path);
        if !legacy.exists() {
            continue;
        }

        let canonical = sidecar_path_for(&path);
        if canonical.exists() {
            out.already_current += 1;
            continue;
        }

        if let Some(parent) = canonical.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                tracing::warn!("Sidecar migration: cannot create {}: {}", parent.display(), e);
                out.failed += 1;
                continue;
            }
        }

        match fs::rename(&legacy, &canonical) {
            Ok(()) => {
                tracing::info!(
                    "Migrated sidecar {} -> {}",
                    legacy.display(),
                    canonical.display()
                );
                out.moved += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "Sidecar migration: could not move {}: {}",
                    legacy.display(),
                    e
                );
                out.failed += 1;
            }
        }
    }

    out
}

/// Rewrite just `keyframe_safe_start_ms` in an existing sidecar, in place.
///
/// The T-1 backfill needs to correct one number on assets whose sidecars carry
/// a loudness measurement, a QC report and a validation report that cannot be
/// reconstructed from the registry row. Regenerating the payload from the row
/// would silently drop all of it, so this patches the parsed JSON and leaves
/// every other key exactly as it was.
///
/// Returns `Ok(false)` when the sidecar is absent or already correct -- neither
/// is an error, and an asset ingested before sidecars were written has nothing
/// to patch.
pub fn patch_sidecar_keyframe_safe_start(
    media_path: &Path,
    keyframe_safe_start_ms: i64,
) -> Result<bool, String> {
    let sidecar_path = sidecar_path_for(media_path);
    if !sidecar_path.exists() {
        return Ok(false);
    }

    let raw = fs::read_to_string(&sidecar_path)
        .map_err(|e| format!("cannot read sidecar '{}': {}", sidecar_path.display(), e))?;
    let mut doc: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("sidecar '{}' is not valid JSON: {}", sidecar_path.display(), e))?;

    let Some(obj) = doc.as_object_mut() else {
        return Err(format!(
            "sidecar '{}' is not a JSON object",
            sidecar_path.display()
        ));
    };

    if obj.get("keyframe_safe_start_ms").and_then(|v| v.as_i64()) == Some(keyframe_safe_start_ms) {
        return Ok(false);
    }
    obj.insert(
        "keyframe_safe_start_ms".to_string(),
        serde_json::json!(keyframe_safe_start_ms),
    );

    let json = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("cannot serialize the patched sidecar: {}", e))?;

    // Same staging-then-rename as `write_sidecar_payload`: a half-written
    // sidecar is what PlayOut hydrates from.
    let tmp = sidecar_path.with_extension("tmp_json");
    fs::write(&tmp, &json).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write '{}': {}", tmp.display(), e)
    })?;
    fs::rename(&tmp, &sidecar_path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot publish '{}': {}", sidecar_path.display(), e)
    })?;

    Ok(true)
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

/// Why a sidecar could not be rebuilt.
#[derive(Debug)]
pub enum SidecarRebuildError {
    /// The mezzanine is not on disk.
    MezzanineMissing,
    /// ffprobe is unavailable or could not read the file.
    ///
    /// The caller must surface this rather than write a sidecar, because the
    /// only alternative is fabricating the values — which is what this used to
    /// do (T3-5, F-27).
    ProbeUnavailable(String),
    /// The sidecar itself could not be written.
    WriteFailed(String),
}

/// Rebuild a sidecar for an asset whose sidecar was lost.
///
/// **Re-probes the mezzanine.** The previous implementation filled the payload
/// from the registry row and hard-coded everything the row does not carry:
/// `width: 1920`, `height: 1080`, `codec: "h264"`, `audio_codec: "aac"`,
/// `audio_sample_rate: 48000`, `field_order: "progressive"`. For a 1080p25
/// H.264 mezzanine those happen to be right, which is why it went unnoticed;
/// for anything else — a legacy SD asset, a 720p promo, a 5.1 feature — it
/// wrote confident, wrong metadata into the file PlayOut hydrates from, and
/// nothing downstream could tell it apart from a real sidecar (F-27).
///
/// Without a usable ffprobe there is no honest output, so this returns
/// `ProbeUnavailable` and the caller answers 503. A missing sidecar is a
/// recoverable annoyance; a plausible wrong one is not.
pub fn rebuild_sidecar_from_media(
    asset: &crate::db::MediaAsset,
    tools: &crate::bootstrap::ToolPaths,
) -> Result<PathBuf, SidecarRebuildError> {
    if asset.current_path.is_empty() {
        return Err(SidecarRebuildError::MezzanineMissing);
    }
    let media_path = Path::new(&asset.current_path);
    if !media_path.exists() {
        return Err(SidecarRebuildError::MezzanineMissing);
    }

    let probed = crate::probe::probe_media(tools, media_path)
        .map_err(SidecarRebuildError::ProbeUnavailable)?;

    build_sidecar_from_db_asset_with_probe(asset, Some(&probed))
        .map_err(SidecarRebuildError::WriteFailed)
}

/// The payload builder. `probe` supplies the real stream properties; `None`
/// falls back to the registry row and to conservative defaults, and is only
/// used by tests — every production path goes through
/// [`rebuild_sidecar_from_media`].
pub fn build_sidecar_from_db_asset_with_probe(
    asset: &crate::db::MediaAsset,
    probe: Option<&ProbeData>,
) -> Result<PathBuf, String> {
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
        // Every field below comes from the probe when there is one. The
        // rebuilt sidecar describes the mezzanine as it actually is; the only
        // thing it cannot recover is the *original source*, which is long gone
        // by the time anyone needs to rebuild, so `original_source` is filled
        // from the mezzanine and `profile_used` says `rebuilt_from_db` to make
        // that explicit.
        original_source: SourceInfo {
            path: asset.current_path.clone(),
            codec: probe
                .map(|p| p.video_codec.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            duration_secs,
            frame_count: asset.total_frames,
            width: probe.map(|p| p.width).unwrap_or(0),
            height: probe.map(|p| p.height).unwrap_or(0),
            fps: asset.fps,
            fps_num: asset.fps_num,
            fps_den: asset.fps_den,
            field_order: probe
                .map(|p| p.field_order.clone())
                .unwrap_or_else(|| "unknown".to_string()),
        },
        output_media: OutputInfo {
            duration_secs,
            frame_count: asset.total_frames,
            width: probe.map(|p| p.width).unwrap_or(0),
            height: probe.map(|p| p.height).unwrap_or(0),
            codec: probe
                .map(|p| p.video_codec.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            audio_codec: probe
                .map(|p| p.audio_codec.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            audio_sample_rate: probe.map(|p| p.audio_sample_rate).unwrap_or(0),
            audio_channels: probe.map(|p| p.audio_channels).unwrap_or(0),
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

pub fn transliterate_greek(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(chars.len() * 2);
    let mut i = 0;

    fn is_voiceless(c: char) -> bool {
        matches!(
            c,
            'θ' | 'Θ' | 'κ' | 'Κ' | 'ξ' | 'Ξ' | 'π' | 'Π' | 'σ' | 'Σ' | 'ς' | 'τ' | 'Τ' | 'φ'
                | 'Φ' | 'χ' | 'Χ' | 'ψ' | 'Ψ'
        )
    }

    while i < chars.len() {
        let c = chars[i];
        let next = if i + 1 < chars.len() {
            Some(chars[i + 1])
        } else {
            None
        };

        if let Some(n) = next {
            let pair = (c, n);
            match pair {
                ('α' | 'Α' | 'ά' | 'Ά', 'υ' | 'Υ' | 'ύ' | 'Ύ') => {
                    let next_next = if i + 2 < chars.len() {
                        Some(chars[i + 2])
                    } else {
                        None
                    };
                    let v = if next_next.map_or(false, is_voiceless) {
                        "af"
                    } else {
                        "av"
                    };
                    out.push_str(v);
                    i += 2;
                    continue;
                }
                ('ε' | 'Ε' | 'έ' | 'Έ', 'υ' | 'Υ' | 'ύ' | 'Ύ') => {
                    let next_next = if i + 2 < chars.len() {
                        Some(chars[i + 2])
                    } else {
                        None
                    };
                    let v = if next_next.map_or(false, is_voiceless) {
                        "ef"
                    } else {
                        "ev"
                    };
                    out.push_str(v);
                    i += 2;
                    continue;
                }
                ('η' | 'Η' | 'ή' | 'Ή', 'υ' | 'Υ' | 'ύ' | 'Ύ') => {
                    let next_next = if i + 2 < chars.len() {
                        Some(chars[i + 2])
                    } else {
                        None
                    };
                    let v = if next_next.map_or(false, is_voiceless) {
                        "if"
                    } else {
                        "iv"
                    };
                    out.push_str(v);
                    i += 2;
                    continue;
                }
                ('ο' | 'Ο' | 'ό' | 'Ό', 'υ' | 'Υ' | 'ύ' | 'Ύ') => {
                    out.push_str("ou");
                    i += 2;
                    continue;
                }
                ('γ' | 'Γ', 'γ' | 'Γ') => {
                    out.push_str("ng");
                    i += 2;
                    continue;
                }
                ('γ' | 'Γ', 'κ' | 'Κ') => {
                    out.push_str("gk");
                    i += 2;
                    continue;
                }
                ('γ' | 'Γ', 'ξ' | 'Ξ') => {
                    out.push_str("nx");
                    i += 2;
                    continue;
                }
                ('γ' | 'Γ', 'χ' | 'Χ') => {
                    out.push_str("nch");
                    i += 2;
                    continue;
                }
                ('μ' | 'Μ', 'π' | 'Π') => {
                    let prev = if i > 0 { Some(chars[i - 1]) } else { None };
                    let is_word_start = prev.map_or(true, |p| !p.is_alphabetic());
                    if is_word_start {
                        out.push_str("b");
                    } else {
                        out.push_str("mp");
                    }
                    i += 2;
                    continue;
                }
                ('ν' | 'Ν', 'τ' | 'Τ') => {
                    out.push_str("nt");
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        match c {
            'α' | 'Α' | 'ά' | 'Ά' => out.push('a'),
            'β' | 'Β' => out.push('v'),
            'γ' | 'Γ' => out.push('g'),
            'δ' | 'Δ' => out.push('d'),
            'ε' | 'Ε' | 'έ' | 'Έ' => out.push('e'),
            'ζ' | 'Ζ' => out.push('z'),
            'η' | 'Η' | 'ή' | 'Ή' => out.push('i'),
            'θ' | 'Θ' => out.push_str("th"),
            'ι' | 'Ι' | 'ί' | 'Ί' | 'ϊ' | 'Ϊ' | 'ΐ' => out.push('i'),
            'κ' | 'Κ' => out.push('k'),
            'λ' | 'Λ' => out.push('l'),
            'μ' | 'Μ' => out.push('m'),
            'ν' | 'Ν' => out.push('n'),
            'ξ' | 'Ξ' => out.push('x'),
            'ο' | 'Ο' | 'ό' | 'Ό' => out.push('o'),
            'π' | 'Π' => out.push('p'),
            'ρ' | 'Ρ' => out.push('r'),
            'σ' | 'Σ' | 'ς' => out.push('s'),
            'τ' | 'Τ' => out.push('t'),
            'υ' | 'Υ' | 'ύ' | 'Ύ' | 'ϋ' | 'Ϋ' | 'ΰ' => out.push('y'),
            'φ' | 'Φ' => out.push('f'),
            'χ' | 'Χ' => out.push_str("ch"),
            'ψ' | 'Ψ' => out.push_str("ps"),
            'ω' | 'Ω' | 'ώ' | 'Ώ' => out.push('o'),
            other => out.push(other),
        }
        i += 1;
    }

    out
}

pub fn sanitize_filename(raw: &str) -> String {
    let transliterated = transliterate_greek(raw);
    transliterated
        .chars()
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
    fn a_generic_folder_also_resolves_into_a_sidecars_subdirectory() {
        // Was `C:/media/custom_folder/clip_abc.uuid.json`. T3-5 made the rule
        // flat: every mezzanine's sidecar lives in a `sidecars/` directory, so
        // the answer no longer depends on which branch of the old lookup won.
        let media = Path::new("C:/media/custom_folder/clip_abc.mp4");
        let sidecar = sidecar_path_for(media);
        let normalized = sidecar.to_string_lossy().replace('\\', "/");
        assert_eq!(normalized, "C:/media/custom_folder/sidecars/clip_abc.uuid.json");
    }

    #[test]
    fn the_resolver_does_not_depend_on_what_is_on_disk() {
        // The heart of F-28. The old resolver probed the filesystem and
        // returned whichever candidate it found first, so a write could create
        // the file that the next read then located somewhere else, and a delete
        // could silently retarget the next write to a legacy location.
        let dir = std::env::temp_dir().join(format!(
            "pt-sidecar-det-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let videos = dir.join("videos");
        std::fs::create_dir_all(&videos).unwrap();
        let media = videos.join("clip.mp4");
        std::fs::write(&media, b"x").unwrap();

        let before = sidecar_path_for(&media);

        // Create a legacy adjacent sidecar -- the old code would now return it.
        std::fs::write(legacy_sidecar_path_for(&media), b"{}").unwrap();
        assert_eq!(sidecar_path_for(&media), before, "a legacy file must not retarget the resolver");

        // And create the canonical one -- still the same answer.
        std::fs::create_dir_all(before.parent().unwrap()).unwrap();
        std::fs::write(&before, b"{}").unwrap();
        assert_eq!(sidecar_path_for(&media), before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn migration_fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pt-sidecar-mig-{}-{}-{}",
            name,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("videos")).unwrap();
        dir
    }

    #[test]
    fn a_legacy_adjacent_sidecar_is_moved_into_place() {
        let root = migration_fixture("move");
        let videos = root.join("videos");
        let media = videos.join("programme.mp4");
        std::fs::write(&media, b"video").unwrap();
        std::fs::write(legacy_sidecar_path_for(&media), br#"{"id":"legacy"}"#).unwrap();

        let report = migrate_legacy_sidecars(&videos);
        assert_eq!(report.moved, 1);
        assert_eq!(report.failed, 0);

        let canonical = sidecar_path_for(&media);
        assert!(canonical.exists(), "moved to {}", canonical.display());
        assert!(!legacy_sidecar_path_for(&media).exists(), "and removed from beside the media");
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            r#"{"id":"legacy"}"#,
            "contents must survive the move"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_is_idempotent() {
        let root = migration_fixture("idempotent");
        let videos = root.join("videos");
        let media = videos.join("clip.mp4");
        std::fs::write(&media, b"video").unwrap();
        std::fs::write(legacy_sidecar_path_for(&media), b"{}").unwrap();

        assert_eq!(migrate_legacy_sidecars(&videos).moved, 1);
        // It runs at every startup, so a second pass must be a no-op.
        assert_eq!(migrate_legacy_sidecars(&videos), SidecarMigration::default());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_legacy_file_never_overwrites_a_current_one() {
        // The canonical sidecar is the newer of the two by construction.
        // Honouring a stale legacy file would lose the real metadata.
        let root = migration_fixture("no-clobber");
        let videos = root.join("videos");
        let media = videos.join("clip.mp4");
        std::fs::write(&media, b"video").unwrap();
        std::fs::write(legacy_sidecar_path_for(&media), br#"{"which":"stale"}"#).unwrap();

        let canonical = sidecar_path_for(&media);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        std::fs::write(&canonical, br#"{"which":"current"}"#).unwrap();

        let report = migrate_legacy_sidecars(&videos);
        assert_eq!(report.already_current, 1);
        assert_eq!(report.moved, 0);
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            r#"{"which":"current"}"#
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrating_a_directory_that_does_not_exist_is_not_an_error() {
        let missing = std::env::temp_dir().join("pt-sidecar-mig-absent");
        assert_eq!(migrate_legacy_sidecars(&missing), SidecarMigration::default());
    }

    #[test]
    fn test_build_sidecar_from_db_asset_creates_file() {
        let temp_dir = std::env::temp_dir().join(format!("pt_sidecar_test_{}", uuid::Uuid::new_v4()));
        let videos_dir = temp_dir.join("videos");
        std::fs::create_dir_all(&videos_dir).unwrap();
        let media_file = videos_dir.join("test_clip.mp4");
        std::fs::write(&media_file, b"fake video content").unwrap();

        let asset = crate::db::MediaAsset {
            source_sha256: None,
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
            parent_uuid: None,
            qc_verdict_key: None,
        };

        // A 720p50 MPEG-2 asset with 5.1 audio: nothing like the 1080p25 H.264
        // stereo the old code hard-coded.
        let probe = ProbeData {
            duration_secs: 10.0,
            frame_count: 500,
            width: 1280,
            height: 720,
            video_codec: "mpeg2video".into(),
            audio_codec: "pcm_s24le".into(),
            audio_sample_rate: 44100,
            audio_channels: 6,
            fps_num: 50,
            fps_den: 1,
            field_order: "tt".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: asset.current_path.clone(),
        };

        let result = build_sidecar_from_db_asset_with_probe(&asset, Some(&probe)).unwrap();
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

        // The point of T3-5: these come from the probe, not from constants.
        // The old code wrote 1920x1080 / h264 / aac / 48000 / 2 / progressive
        // for every asset it rebuilt, and nothing downstream could tell that
        // apart from a real sidecar (F-27).
        assert_eq!(parsed.output_media.width, 1280);
        assert_eq!(parsed.output_media.height, 720);
        assert_eq!(parsed.output_media.codec, "mpeg2video");
        assert_eq!(parsed.output_media.audio_codec, "pcm_s24le");
        assert_eq!(parsed.output_media.audio_sample_rate, 44100);
        assert_eq!(parsed.output_media.audio_channels, 6);
        assert_eq!(parsed.original_source.field_order, "tt");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn a_rebuild_without_a_probe_says_unknown_rather_than_guessing() {
        // The `None` path exists only for tests, but if it is ever reached the
        // output must be obviously incomplete rather than plausibly wrong.
        let temp_dir = std::env::temp_dir().join(format!(
            "pt-sidecar-noprobe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("videos")).unwrap();
        let media = temp_dir.join("videos").join("clip.mp4");
        std::fs::write(&media, b"x").unwrap();

        let asset = crate::db::MediaAsset {
            uuid: "u".into(),
            fingerprint: 1,
            source_sha256: None,
            current_path: media.to_string_lossy().into_owned(),
            duration_ms: 1000,
            trim_in_ms: 0,
            trim_out_ms: 1000,
            rating: "K".into(),
            tp: "None".into(),
            status: "ready".into(),
            display_name: "clip".into(),
            virtual_folder: "/".into(),
            mezzanine_ok: true,
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            total_frames: 25,
            gop_frames: 50,
            keyframe_safe_start_ms: 0,
            warnings: "[]".into(),
            keyframe_offsets_json: "[]".into(),
            deleted_at: None,
            original_virtual_folder: None,
            parent_uuid: None,
            qc_verdict_key: None,
        };

        let path = build_sidecar_from_db_asset_with_probe(&asset, None).unwrap();
        let parsed: SidecarPayload =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(parsed.output_media.width, 0, "0, not a fabricated 1920");
        assert_eq!(parsed.output_media.height, 0);
        assert_eq!(parsed.output_media.codec, "unknown");
        assert_eq!(parsed.output_media.audio_codec, "unknown");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn a_rebuild_of_a_missing_mezzanine_never_reaches_the_probe() {
        let asset = crate::db::MediaAsset {
            uuid: "gone".into(),
            fingerprint: 1,
            source_sha256: None,
            current_path: "D:/definitely/not/here.mp4".into(),
            duration_ms: 1000,
            trim_in_ms: 0,
            trim_out_ms: 1000,
            rating: "K".into(),
            tp: "None".into(),
            status: "ready".into(),
            display_name: "gone".into(),
            virtual_folder: "/".into(),
            mezzanine_ok: true,
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            total_frames: 25,
            gop_frames: 50,
            keyframe_safe_start_ms: 0,
            warnings: "[]".into(),
            keyframe_offsets_json: "[]".into(),
            deleted_at: None,
            original_virtual_folder: None,
            parent_uuid: None,
            qc_verdict_key: None,
        };

        let tools = crate::bootstrap::ToolPaths {
            ffmpeg: std::path::PathBuf::new(),
            ffprobe: std::path::PathBuf::new(),
        };
        match rebuild_sidecar_from_media(&asset, &tools) {
            Err(SidecarRebuildError::MezzanineMissing) => {}
            other => panic!("expected MezzanineMissing, got {:?}", other),
        }
    }

    #[test]
    fn a_rebuild_without_ffprobe_reports_probe_unavailable_and_writes_nothing() {
        let temp_dir = std::env::temp_dir().join(format!(
            "pt-sidecar-noffprobe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("videos")).unwrap();
        let media = temp_dir.join("videos").join("clip.mp4");
        std::fs::write(&media, b"not really a video").unwrap();

        let asset = crate::db::MediaAsset {
            uuid: "u".into(),
            fingerprint: 1,
            source_sha256: None,
            current_path: media.to_string_lossy().into_owned(),
            duration_ms: 1000,
            trim_in_ms: 0,
            trim_out_ms: 1000,
            rating: "K".into(),
            tp: "None".into(),
            status: "ready".into(),
            display_name: "clip".into(),
            virtual_folder: "/".into(),
            mezzanine_ok: true,
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            total_frames: 25,
            gop_frames: 50,
            keyframe_safe_start_ms: 0,
            warnings: "[]".into(),
            keyframe_offsets_json: "[]".into(),
            deleted_at: None,
            original_virtual_folder: None,
            parent_uuid: None,
            qc_verdict_key: None,
        };

        let tools = crate::bootstrap::ToolPaths {
            ffmpeg: std::path::PathBuf::from("no-such-ffmpeg"),
            ffprobe: std::path::PathBuf::from("no-such-ffprobe"),
        };
        match rebuild_sidecar_from_media(&asset, &tools) {
            Err(SidecarRebuildError::ProbeUnavailable(_)) => {}
            other => panic!("expected ProbeUnavailable, got {:?}", other),
        }

        // And crucially, no sidecar was left behind. A plausible wrong sidecar
        // is worse than a missing one, because nothing downstream can tell.
        assert!(
            !sidecar_path_for(&media).exists(),
            "a failed rebuild must not write a partial or fabricated sidecar"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sanitize_filename_greek_elot743() {
        assert_eq!(sanitize_filename("ΕΙΔΗΣΕΙΣ_2026"), "eidiseis_2026");
        assert_eq!(sanitize_filename("Δελτίο Ειδήσεων"), "deltio_eidiseon");
        assert_eq!(sanitize_filename("ΧΑΡΑΥΓΗ"), "charavgi");
        assert_eq!(sanitize_filename("αυτοκίνητο"), "aftokinito");
        assert_eq!(sanitize_filename("ΨΥΧΗ"), "psychi");
        assert_eq!(sanitize_filename("ΘΕΑΤΡΟ"), "theatro");
        assert_eq!(sanitize_filename("   ___ΕΛΛΑΔΑ___  "), "ellada");
    }
}
