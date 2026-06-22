use crate::bootstrap::ToolPaths;
use crate::profiles::ProfileId;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Serialize)]
pub struct ProbeData {
    pub duration_secs: f64,
    pub frame_count: i64,
    pub width: i64,
    pub height: i64,
    pub video_codec: String,
    pub audio_codec: String,
    pub fps_num: i64,
    pub fps_den: i64,
    pub field_order: String,
    pub display_aspect_ratio: String,
    pub input_path: String,
}

impl ProbeData {
    pub fn fps(&self) -> f64 {
        if self.fps_den > 0 {
            self.fps_num as f64 / self.fps_den as f64
        } else {
            25.0
        }
    }

    pub fn profile_id(&self) -> ProfileId {
        if self.height > 900 {
            match self.field_order.as_str() {
                "tt" | "tb" | "tff" | "bff" | "bb" | "bt" => ProfileId::ProfileB,
                _ => ProfileId::ProfileA,
            }
        } else {
            ProfileId::ProfileC
        }
    }
}

#[derive(Deserialize, Debug)]
struct FfprobeOutput {
    streams: Vec<StreamInfo>,
    format: FormatInfo,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum FfprobeValue {
    Str(String),
    Num(f64),
}

impl FfprobeValue {
    fn as_opt_string(&self) -> Option<String> {
        match self {
            Self::Str(s) if !s.is_empty() => Some(s.clone()),
            Self::Num(n) => Some(n.to_string()),
            _ => None,
        }
    }
    fn parse_f64(&self) -> Option<f64> {
        match self {
            Self::Str(s) => s.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0),
            Self::Num(n) if n.is_finite() && *n > 0.0 => Some(*n),
            _ => None,
        }
    }
    fn parse_i64(&self) -> Option<i64> {
        match self {
            Self::Str(s) => s.trim().parse::<i64>().ok().filter(|v| *v > 0),
            Self::Num(n) if n.is_finite() && *n > 0.0 => Some(*n as i64),
            _ => None,
        }
    }
    fn parse_ratio(&self) -> Option<f64> {
        match self {
            Self::Str(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() || trimmed == "0/0" { return None; }
                if let Some((n, d)) = trimmed.split_once('/') {
                    let n = n.trim().parse::<f64>().ok()?;
                    let d = d.trim().parse::<f64>().ok()?;
                    if d.abs() < f64::EPSILON { return None; }
                    let r = n / d;
                    if r.is_finite() && r > 0.0 { Some(r) } else { None }
                } else {
                    Self::parse_f64(&Self::Str(trimmed.to_string()))
                }
            }
            Self::Num(n) if n.is_finite() && *n > 0.0 => Some(*n),
            _ => None,
        }
    }
}

#[derive(Deserialize, Debug)]
struct StreamInfo {
    codec_type: String,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<FfprobeValue>,
    avg_frame_rate: Option<FfprobeValue>,
    duration: Option<FfprobeValue>,
    duration_ts: Option<FfprobeValue>,
    time_base: Option<FfprobeValue>,
    nb_frames: Option<FfprobeValue>,
    display_aspect_ratio: Option<FfprobeValue>,
    field_order: Option<FfprobeValue>,
}

#[derive(Deserialize, Debug)]
struct FormatInfo {
    duration: Option<FfprobeValue>,
}

pub fn probe_media(tools: &ToolPaths, input_path: &Path) -> Result<ProbeData, String> {
    let mut command = Command::new(&tools.ffprobe);
    command.args([
        "-v", "quiet",
        "-print_format", "json",
        "-show_streams",
        "-show_format",
    ]);
    command.arg(input_path);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command
        .output()
        .map_err(|e| format!("ffprobe exec failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed: {}", stderr));
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("ffprobe JSON parse failed: {}", e))?;

    let vstream = parsed.streams.iter().find(|s| s.codec_type == "video");
    let astream = parsed.streams.iter().find(|s| s.codec_type == "audio");

    let video_codec = vstream
        .and_then(|s| s.codec_name.clone())
        .unwrap_or_else(|| "unknown".into());
    let audio_codec = astream
        .and_then(|s| s.codec_name.clone())
        .unwrap_or_else(|| "none".into());

    let width = vstream.and_then(|s| s.width).unwrap_or(0) as i64;
    let height = vstream.and_then(|s| s.height).unwrap_or(0) as i64;

    let (fps_num, fps_den) = vstream
        .and_then(|s| s.r_frame_rate.as_ref())
        .map(parse_fps_value)
        .unwrap_or((25, 1));

    let field_order = vstream
        .and_then(|s| s.field_order.as_ref())
        .and_then(|v| v.as_opt_string())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let dar = vstream
        .and_then(|s| s.display_aspect_ratio.as_ref())
        .and_then(|v| v.as_opt_string())
        .unwrap_or_default();

    let duration_secs = resolve_duration(&parsed);
    let frame_count = (duration_secs * fps_num as f64 / fps_den as f64).round() as i64;

    if width == 0 || height == 0 {
        return Err("No video stream found".into());
    }

    Ok(ProbeData {
        duration_secs,
        frame_count,
        width,
        height,
        video_codec,
        audio_codec,
        fps_num,
        fps_den,
        field_order,
        display_aspect_ratio: dar,
        input_path: input_path.to_string_lossy().into_owned(),
    })
}

fn resolve_duration(parsed: &FfprobeOutput) -> f64 {
    let mut candidates: Vec<f64> = Vec::new();

    if let Some(d) = parsed.format.duration.as_ref().and_then(|v| v.parse_f64()) {
        candidates.push(d);
    }

    for stream in &parsed.streams {
        if let Some(d) = stream.duration.as_ref().and_then(|v| v.parse_f64()) {
            candidates.push(d);
        }
        if let (Some(ts), Some(tb)) = (
            stream.duration_ts.as_ref().and_then(|v| v.parse_i64()),
            stream.time_base.as_ref().and_then(|v| v.parse_ratio()),
        ) {
            candidates.push(ts as f64 * tb);
        }
        let fps_from_rf = stream.r_frame_rate.as_ref().and_then(|v| v.parse_ratio());
        let fps_from_avg = stream.avg_frame_rate.as_ref().and_then(|v| v.parse_ratio());
        let fps = fps_from_avg.or(fps_from_rf);
        if let (Some(frames), Some(fps)) = (
            stream.nb_frames.as_ref().and_then(|v| v.parse_i64()),
            fps,
        ) {
            candidates.push(frames as f64 / fps);
        }
    }

    candidates
        .into_iter()
        .filter(|v| v.is_finite() && *v > 0.0)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0)
}

fn parse_fps_value(value: &FfprobeValue) -> (i64, i64) {
    match value {
        FfprobeValue::Str(raw) => {
            let parts: Vec<&str> = raw.split('/').collect();
            if parts.len() == 2 {
                let n = parts[0].parse::<i64>().unwrap_or(25);
                let d = parts[1].parse::<i64>().unwrap_or(1);
                (n, d)
            } else {
                let n = raw.parse::<f64>().unwrap_or(25.0);
                ((n * 1000.0).round() as i64, 1000)
            }
        }
        FfprobeValue::Num(n) => {
            ((n * 1000.0).round() as i64, 1000)
        }
    }
}
