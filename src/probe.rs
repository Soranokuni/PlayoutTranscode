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
    pub audio_sample_rate: i64,
    pub audio_channels: i64,
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
    sample_rate: Option<FfprobeValue>,
    channels: Option<FfprobeValue>,
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

    let audio_sample_rate = astream
        .and_then(|s| s.sample_rate.as_ref())
        .and_then(|v| v.parse_i64())
        .unwrap_or(0);
    let audio_channels = astream
        .and_then(|s| s.channels.as_ref())
        .and_then(|v| v.parse_i64())
        .unwrap_or(0);

    let (fps_num_raw, fps_den_raw) = vstream
        .and_then(|s| s.r_frame_rate.as_ref())
        .map(parse_fps_value)
        .unwrap_or((25, 1));

    let (fps_num, fps_den) = snap_fps_rational(fps_num_raw, fps_den_raw);

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
    let frame_count = if fps_den > 0 {
        (duration_secs * fps_num as f64 / fps_den as f64).round() as i64
    } else {
        0
    };

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
        audio_sample_rate,
        audio_channels,
        fps_num,
        fps_den,
        field_order,
        display_aspect_ratio: dar,
        input_path: input_path.to_string_lossy().into_owned(),
    })
}

fn resolve_duration(parsed: &FfprobeOutput) -> f64 {
    let vstream = parsed.streams.iter().find(|s| s.codec_type == "video");

    if let Some(vs) = vstream {
        if let Some(d) = vs.duration.as_ref().and_then(|v| v.parse_f64()) {
            return d;
        }
        if let (Some(ts), Some(tb)) = (
            vs.duration_ts.as_ref().and_then(|v| v.parse_i64()),
            vs.time_base.as_ref().and_then(|v| v.parse_ratio()),
        ) {
            let d = ts as f64 * tb;
            if d.is_finite() && d > 0.0 {
                return d;
            }
        }
        let fps = vs.avg_frame_rate.as_ref().and_then(|v| v.parse_ratio())
            .or_else(|| vs.r_frame_rate.as_ref().and_then(|v| v.parse_ratio()));
        if let (Some(frames), Some(fps)) = (
            vs.nb_frames.as_ref().and_then(|v| v.parse_i64()),
            fps,
        ) {
            let d = frames as f64 / fps;
            if d.is_finite() && d > 0.0 {
                return d;
            }
        }
    }

    if let Some(d) = parsed.format.duration.as_ref().and_then(|v| v.parse_f64()) {
        return d;
    }

    for stream in &parsed.streams {
        if stream.codec_type == "audio" {
            if let Some(d) = stream.duration.as_ref().and_then(|v| v.parse_f64()) {
                return d;
            }
        }
    }

    0.0
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

pub fn snap_fps_rational(num: i64, den: i64) -> (i64, i64) {
    if den <= 0 {
        return (25, 1);
    }
    let fps = num as f64 / den as f64;
    let near = |a: f64, b: f64| (a - b).abs() < 0.05;
    if near(fps, 29.97) { return (30000, 1001); }
    if near(fps, 23.976) { return (24000, 1001); }
    if near(fps, 59.94) { return (60000, 1001); }
    if near(fps, 25.0) { return (25, 1); }
    if near(fps, 50.0) { return (50, 1); }
    if near(fps, 30.0) { return (30, 1); }
    if near(fps, 60.0) { return (60, 1); }
    if near(fps, 24.0) { return (24, 1); }
    (num, den)
}

pub fn get_keyframe_safe_start_ms(ffprobe_path: &Path, path: &Path) -> i64 {
    let mut command = Command::new(ffprobe_path);
    command.args([
        "-v", "error",
        "-select_streams", "v:0",
        "-skip_frame", "nokey",
        "-show_entries", "frame=pts_time",
        "-of", "csv=p=0",
    ]);
    command.arg(path);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let out = command.output();

    if let Ok(output) = out {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "N/A" {
                continue;
            }
            if let Ok(t_sec) = trimmed.parse::<f64>() {
                return (t_sec * 1000.0).round() as i64;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snap_fps_pal() {
        assert_eq!(snap_fps_rational(25, 1), (25, 1));
    }

    #[test]
    fn test_snap_fps_ntsc() {
        assert_eq!(snap_fps_rational(30000, 1001), (30000, 1001));
        assert_eq!(snap_fps_rational(29970, 1000), (30000, 1001));
    }

    #[test]
    fn test_snap_fps_film() {
        assert_eq!(snap_fps_rational(24000, 1001), (24000, 1001));
        assert_eq!(snap_fps_rational(23976, 1000), (24000, 1001));
    }

    #[test]
    fn test_snap_fps_unknown_preserved() {
        assert_eq!(snap_fps_rational(48, 1), (48, 1));
    }

    #[test]
    fn test_snap_fps_zero_den() {
        assert_eq!(snap_fps_rational(25, 0), (25, 1));
    }
}
