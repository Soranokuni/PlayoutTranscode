use crate::bootstrap::ToolPaths;
use crate::config::AppConfig;
use crate::probe::ProbeData;
use crate::profiles::{EncodingProfile, ProfileId};
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, LazyLock};

static TIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"time=(\d+):(\d+):(\d+)\.(\d+)").unwrap());
static FRAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"frame=\s*(\d+)").unwrap());
static FPS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"fps=\s*([\d.]+)").unwrap());
static BITRATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"bitrate=\s*([\d.]+kbits/s)").unwrap());
static SPEED_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"speed=\s*([\d.]+x)").unwrap());

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone)]
pub struct EncodeProgress {
    pub frame: i64,
    pub total_frames: i64,
    pub percent: f32,
    pub fps: f64,
    pub bitrate: String,
    pub speed: String,
}

pub struct EncodeResult {
    pub output_path: PathBuf,
    pub success: bool,
    pub error: Option<String>,
}

pub fn transcode_file(
    tools: &ToolPaths,
    config: &AppConfig,
    input_path: &Path,
    source_probe: &ProbeData,
    profile_id: ProfileId,
    output_path: &Path,
    metadata_uuid: &str,
    progress_tx: mpsc::Sender<EncodeProgress>,
) -> EncodeResult {
    let profile = EncodingProfile::by_id(profile_id);
    let mut args = profile.build_ffmpeg_args(
        config,
        &input_path.to_string_lossy(),
        &output_path.to_string_lossy(),
    );

    let metadata_arg = format!("playoutvue_id:{}", metadata_uuid);
    let output_path_str = output_path.to_string_lossy();
    let insert_pos = args.iter().position(|a| a == output_path_str.as_ref()).unwrap_or(args.len());
    args.insert(insert_pos, "-metadata".to_string());
    args.insert(insert_pos + 1, format!("comment={}", metadata_arg));
    args.insert(insert_pos + 2, "-metadata".to_string());
    args.insert(insert_pos + 3, format!("playoutvue_id={}", metadata_uuid));

    let total_frames = source_probe.frame_count;

    let mut command = Command::new(&tools.ffmpeg);
    command.args(&args);
    command.stderr(Stdio::piped());
    command.stdout(Stdio::null());
    command.stdin(Stdio::null());

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some(format!("Failed to spawn ffmpeg: {}", e)),
            };
        }
    };

    let stderr = child.stderr.take().expect("stderr should be piped");
    let reader = BufReader::new(stderr);

    let time_re = &*TIME_RE;
    let frame_re = &*FRAME_RE;
    let fps_re = &*FPS_RE;
    let bitrate_re = &*BITRATE_RE;
    let speed_re = &*SPEED_RE;

    let mut last_frame = 0;

    for line in reader.lines().flatten() {
        if let Some(caps) = frame_re.captures(&line) {
            last_frame = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(last_frame);
        }

        let time_str = time_re.captures(&line).map(|caps| {
            format!(
                "{}:{}:{}.{}",
                caps.get(1).map_or("00", |m| m.as_str()),
                caps.get(2).map_or("00", |m| m.as_str()),
                caps.get(3).map_or("00", |m| m.as_str()),
                caps.get(4).map_or("00", |m| m.as_str()),
            )
        });

        let fps_val = fps_re.captures(&line)
            .and_then(|caps| caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()))
            .unwrap_or(0.0);

        let bitrate = bitrate_re.captures(&line)
            .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default();

        let speed = speed_re.captures(&line)
            .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default();

        let percent = if total_frames > 0 {
            ((last_frame as f32 / total_frames as f32) * 100.0).min(99.0)
        } else if let Some(ref ts) = time_str {
            let parts: Vec<f64> = ts.split(':').filter_map(|p| p.parse().ok()).collect();
            let secs = if parts.len() == 4 {
                parts[0] * 3600.0 + parts[1] * 60.0 + parts[2] + parts[3] / 100.0
            } else {
                0.0
            };
            if source_probe.duration_secs > 0.0 {
                ((secs / source_probe.duration_secs) * 100.0).min(99.0) as f32
            } else {
                0.0
            }
        } else {
            0.0
        };

        if time_str.is_some() {
            let _ = progress_tx.send(EncodeProgress {
                frame: last_frame,
                total_frames,
                percent,
                fps: fps_val,
                bitrate,
                speed,
            });
        }
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some(format!("Failed to wait on ffmpeg: {}", e)),
            };
        }
    };

    let _ = progress_tx.send(EncodeProgress {
        frame: total_frames,
        total_frames,
        percent: 100.0,
        fps: 0.0,
        bitrate: String::new(),
        speed: String::new(),
    });

    if status.success() {
        EncodeResult {
            output_path: output_path.to_path_buf(),
            success: true,
            error: None,
        }
    } else {
        EncodeResult {
            output_path: output_path.to_path_buf(),
            success: false,
            error: Some(format!("ffmpeg exited with code {:?}", status.code())),
        }
    }
}
