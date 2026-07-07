use crate::bootstrap::ToolPaths;
use crate::config::AppConfig;
use crate::probe::ProbeData;
use crate::profiles::{EncodingProfile, ProfileId};
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, LazyLock};
use std::sync::{Arc, Mutex};

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
    pub current_time_ms: i64,
    pub duration_ms: i64,
}

pub struct EncodeResult {
    pub output_path: PathBuf,
    pub success: bool,
    /// One-line human-readable summary suitable for UI display.
    pub error: Option<String>,
    /// Verbose stderr tail from ffmpeg; rendered inside a collapsible UI element.
    pub stderr_tail: Vec<String>,
    #[allow(dead_code)]
    pub exit_pid: Option<u32>,
}

/// Reduce a stderr buffer to a single human-readable summary line.
/// ffmpeg's last log line is usually "Conversion failed!" preceded by the actual cause; we walk
/// backwards and pick the first non-trivial diagnostic line we can find.
fn summarize_stderr(lines: &[String]) -> Option<String> {
    const BORING: &[&str] = &[
        "Conversion failed!",
        "At least one output file must be specified",
        "frame=",
        "Press [q] to stop",
        "[libx264 @",
        "[mp4 @",
        "[aac @",
        "[libmp3lame @",
    ];
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("frame=") {
            continue;
        }
        if BORING.iter().any(|b| trimmed.starts_with(b)) || trimmed == "Conversion failed!" {
            continue;
        }
        if trimmed.len() < 6 {
            continue;
        }
        return Some(trimmed.to_string());
    }
    if let Some(last) = lines.iter().rev().find(|l| !l.is_empty()) {
        return Some(last.trim().to_string());
    }
    None
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
    active_pids: Option<Arc<Mutex<Vec<u32>>>>,
) -> EncodeResult {
    let profile = EncodingProfile::by_id(profile_id);
    let mut args = profile.build_ffmpeg_args(
        config,
        &input_path.to_string_lossy(),
        &output_path.to_string_lossy(),
        source_probe.fps_num,
        source_probe.fps_den,
    );

    let output_path_str = output_path.to_string_lossy();
    let insert_pos = args.iter().position(|a| a == output_path_str.as_ref()).unwrap_or(args.len());
    args.insert(insert_pos, "-metadata".to_string());
    args.insert(insert_pos + 1, format!("playoutvue_id={}", metadata_uuid));

    let total_frames = source_probe.frame_count;
    let duration_ms = (source_probe.duration_secs * 1000.0).round() as i64;

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
                stderr_tail: Vec::new(),
                exit_pid: None,
            };
        }
    };

    let pid = child.id();
    if let Some(ref pids) = active_pids {
        if let Ok(mut list) = pids.lock() {
            list.push(pid);
        }
    }

    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some("Failed to pipe stderr from ffmpeg process".to_string()),
                stderr_tail: Vec::new(),
                exit_pid: Some(pid),
            };
        }
    };
    let mut reader = BufReader::new(stderr);

    let time_re = &*TIME_RE;
    let frame_re = &*FRAME_RE;
    let fps_re = &*FPS_RE;
    let bitrate_re = &*BITRATE_RE;
    let speed_re = &*SPEED_RE;

    let mut last_frame = 0;
    let mut stderr_lines: Vec<String> = Vec::new();
    const STDERR_RING_SIZE: usize = 200;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Error reading ffmpeg stderr: {}", e);
                break;
            }
        }
        let line = line_buf.trim_end_matches(|c| c == '\n' || c == '\r');
        if line.is_empty() {
            continue;
        }
        stderr_lines.push(line.to_string());
        if stderr_lines.len() > STDERR_RING_SIZE {
            stderr_lines.remove(0);
        }

        if let Some(caps) = frame_re.captures(line) {
            last_frame = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(last_frame);
        }

        let time_str = time_re.captures(line).map(|caps| {
            format!(
                "{}:{}:{}.{}",
                caps.get(1).map_or("00", |m| m.as_str()),
                caps.get(2).map_or("00", |m| m.as_str()),
                caps.get(3).map_or("00", |m| m.as_str()),
                caps.get(4).map_or("00", |m| m.as_str()),
            )
        });

        let current_time_ms = time_str.as_ref().map(|ts| {
            let parts: Vec<f64> = ts.split(':').filter_map(|p| p.parse().ok()).collect();
            if parts.len() == 4 {
                (parts[0] * 3600.0 + parts[1] * 60.0 + parts[2] + parts[3] / 100.0) as i64 * 1000
            } else {
                0
            }
        }).unwrap_or(0);

        let fps_val = fps_re.captures(line)
            .and_then(|caps| caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()))
            .unwrap_or(0.0);

        let bitrate = bitrate_re.captures(line)
            .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default();

        let speed = speed_re.captures(line)
            .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default();

        let percent = if duration_ms > 0 && current_time_ms > 0 {
            ((current_time_ms as f64 / duration_ms as f64) * 100.0).min(99.0) as f32
        } else if total_frames > 0 {
            ((last_frame as f32 / total_frames as f32) * 100.0).min(99.0) as f32
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
                current_time_ms,
                duration_ms,
            });
        }
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            if let Some(ref pids) = active_pids {
                if let Ok(mut list) = pids.lock() {
                    list.retain(|&x| x != pid);
                }
            }
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some(format!("Failed to wait on ffmpeg: {}", e)),
                stderr_tail: Vec::new(),
                exit_pid: Some(pid),
            };
        }
    };

    if let Some(ref pids) = active_pids {
        if let Ok(mut list) = pids.lock() {
            list.retain(|&x| x != pid);
        }
    }

    let _ = progress_tx.send(EncodeProgress {
        frame: total_frames,
        total_frames,
        percent: 100.0,
        fps: 0.0,
        bitrate: String::new(),
        speed: String::new(),
        current_time_ms: duration_ms,
        duration_ms,
    });

    if status.success() {
        tracing::debug!("FFmpeg stderr ({} lines):\n{}", stderr_lines.len(), stderr_lines.join("\n"));
        EncodeResult {
            output_path: output_path.to_path_buf(),
            success: true,
            error: None,
            stderr_tail: Vec::new(),
            exit_pid: Some(pid),
        }
    } else {
        let tail_len = stderr_lines.len().min(50);
        let tail: Vec<String> = stderr_lines[stderr_lines.len() - tail_len..].to_vec();
        let exit_code = status.code();
        let short = summarize_stderr(&stderr_lines)
            .unwrap_or_else(|| format!("ffmpeg exited with code {:?}", exit_code));
        EncodeResult {
            output_path: output_path.to_path_buf(),
            success: false,
            error: Some(short),
            stderr_tail: tail,
            exit_pid: Some(pid),
        }
    }
}
