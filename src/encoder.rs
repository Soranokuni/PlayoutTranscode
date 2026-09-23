use crate::bootstrap::ToolPaths;
use crate::config::AppConfig;
use crate::probe::ProbeData;
use crate::profiles::{EncodingProfile, ProfileId};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;


/// One `-progress` block from FFmpeg, accumulated key by key (T2-11).
///
/// FFmpeg emits `key=value` lines and terminates each block with
/// `progress=continue` (or `progress=end` for the last one). Values are plain
/// and machine-oriented: no padding, no units to strip, no locale.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProgressBlock {
    pub frame: Option<i64>,
    pub fps: Option<f64>,
    /// Output bitrate exactly as FFmpeg spells it, e.g. `5000.0kbits/s`.
    pub bitrate: Option<String>,
    /// Encoded position in microseconds. FFmpeg's `out_time_ms` is a
    /// long-standing misnomer that also carries microseconds, so this reads
    /// `out_time_us` and falls back to `out_time_ms` only if it is absent.
    pub out_time_us: Option<i64>,
    pub speed: Option<String>,
    /// True once `progress=end` has been seen.
    pub ended: bool,
}

impl ProgressBlock {
    /// Apply one `key=value` line. Returns true when the block is complete and
    /// should be reported.
    pub fn apply(&mut self, line: &str) -> bool {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim();
        let value = value.trim();
        // FFmpeg writes `N/A` for a field it cannot compute yet.
        let usable = !value.is_empty() && value != "N/A";

        match key {
            "frame" if usable => self.frame = value.parse().ok(),
            "fps" if usable => self.fps = value.parse().ok(),
            "bitrate" if usable => self.bitrate = Some(value.to_string()),
            "speed" if usable => self.speed = Some(value.to_string()),
            "out_time_us" if usable => self.out_time_us = value.parse().ok(),
            "out_time_ms" if usable && self.out_time_us.is_none() => {
                self.out_time_us = value.parse().ok()
            }
            "progress" => {
                self.ended = value == "end";
                return true;
            }
            _ => {}
        }
        false
    }
}

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

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
    job_id: &str,
    active_pids: Option<crate::service_handle::ActivePids>,
    audio_policy: &crate::config::AudioPolicy,
    measured_loudness: Option<&crate::probe::MeasuredLoudness>,
) -> EncodeResult {
    let profile = EncodingProfile::by_id(profile_id);
    let mut args = match profile.build_ffmpeg_args_with_audio(
        config,
        &input_path.to_string_lossy(),
        &output_path.to_string_lossy(),
        source_probe,
        audio_policy,
        measured_loudness,
    ) {
        Ok(a) => a,
        Err(e) => {
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some(format!("Failed to build FFmpeg arguments: {}", e)),
                stderr_tail: Vec::new(),
                exit_pid: None,
            };
        }
    };

    let output_path_str = output_path.to_string_lossy();
    let insert_pos = args
        .iter()
        .position(|a| a == output_path_str.as_ref())
        .unwrap_or(args.len());
    args.insert(insert_pos, "-metadata".to_string());
    args.insert(insert_pos + 1, format!("playoutvue_id={}", metadata_uuid));

    let total_frames = source_probe.frame_count;
    let duration_ms = (source_probe.duration_secs * 1000.0).round() as i64;

    let mut command = Command::new(&tools.ffmpeg);
    command.args(&args);
    command.stderr(Stdio::piped());
    // Was `Stdio::null()`. `-progress pipe:1` writes here (T2-11).
    command.stdout(Stdio::piped());
    command.stdin(Stdio::null());

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

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

    // PL-01: dies with the service, however the service dies.
    crate::child::adopt(&child);

    let pid = child.id();
    if let Some(ref pids) = active_pids {
        if let Ok(mut map) = pids.lock() {
            map.insert(job_id.to_string(), pid);
        }
    }
    // Early exits below must reap the child (a bare `kill()` leaves a zombie
    // on Linux and a leaked handle on Windows) and drop its pid, or a later
    // cancel for this job would `taskkill` whatever process reused the number.
    let abandon = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        if let Some(ref pids) = active_pids {
            if let Ok(mut map) = pids.lock() {
                map.retain(|_, &mut v| v != pid);
            }
        }
    };

    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => {
            abandon(&mut child);
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some("Failed to pipe stderr from ffmpeg process".to_string()),
                stderr_tail: Vec::new(),
                exit_pid: Some(pid),
            };
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            abandon(&mut child);
            return EncodeResult {
                output_path: output_path.to_path_buf(),
                success: false,
                error: Some("Failed to pipe stdout from ffmpeg process".to_string()),
                stderr_tail: Vec::new(),
                exit_pid: Some(pid),
            };
        }
    };

    const STDERR_RING_SIZE: usize = 200;

    // stderr is now diagnostics only -- warnings and the failure reason. It has
    // to be drained on its own thread regardless: a full pipe blocks FFmpeg,
    // and a verbose encode fills 64 KiB long before it finishes.
    let stderr_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut lines: Vec<String> = Vec::new();
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Error reading ffmpeg stderr: {}", e);
                    break;
                }
            }
            let line = buf.trim_end_matches(|c| c == '\n' || c == '\r');
            if line.is_empty() {
                continue;
            }
            lines.push(line.to_string());
            if lines.len() > STDERR_RING_SIZE {
                lines.remove(0);
            }
        }
        lines
    });

    // stdout carries the `-progress` blocks: newline-terminated key=value, one
    // block every half second, ending with `progress=end`.
    let mut reader = BufReader::new(stdout);
    let mut block = ProgressBlock::default();
    let mut last_frame = 0i64;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Error reading ffmpeg progress: {}", e);
                break;
            }
        }

        if !block.apply(line_buf.trim()) {
            continue;
        }

        if let Some(f) = block.frame {
            last_frame = f;
        }
        let current_time_ms = block.out_time_us.map(|us| us / 1000).unwrap_or(0);

        let percent = if duration_ms > 0 && current_time_ms > 0 {
            ((current_time_ms as f64 / duration_ms as f64) * 100.0).min(99.0) as f32
        } else if total_frames > 0 {
            ((last_frame as f32 / total_frames as f32) * 100.0).min(99.0) as f32
        } else {
            0.0
        };

        let _ = progress_tx.send(EncodeProgress {
            frame: last_frame,
            total_frames,
            percent,
            fps: block.fps.unwrap_or(0.0),
            bitrate: block.bitrate.clone().unwrap_or_default(),
            speed: block.speed.clone().unwrap_or_default(),
            current_time_ms,
            duration_ms,
        });

        if block.ended {
            break;
        }
        // Carry `frame` forward: FFmpeg omits unchanged keys from later blocks.
        block = ProgressBlock {
            frame: Some(last_frame),
            ..ProgressBlock::default()
        };
    }

    let stderr_lines = stderr_thread.join().unwrap_or_default();

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            if let Some(ref pids) = active_pids {
                if let Ok(mut map) = pids.lock() {
                    map.retain(|_, &mut v| v != pid);
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
        if let Ok(mut map) = pids.lock() {
            map.retain(|_, &mut v| v != pid);
        }
    }

    // Only a clean exit finished the encode. A killed or failed ffmpeg used to
    // report 100% here too, so a cancelled row jumped to "Finalizing 100%"
    // just before it disappeared (UI-03).
    if status.success() {
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
    }

    if status.success() {
        tracing::debug!(
            "FFmpeg stderr ({} lines):\n{}",
            stderr_lines.len(),
            stderr_lines.join("\n")
        );
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

#[cfg(test)]
mod progress_tests {
    use super::*;

    /// A real `-progress pipe:1` block, as FFmpeg 7 emits it.
    const BLOCK: &[&str] = &[
        "bitrate=5000.0kbits/s",
        "total_size=3145728",
        "out_time_us=4920000",
        "out_time_ms=4920000",
        "out_time=00:00:04.920000",
        "dup_frames=0",
        "drop_frames=0",
        "speed=1.02x",
        "progress=continue",
    ];

    fn feed(block: &mut ProgressBlock, lines: &[&str]) -> bool {
        let mut complete = false;
        for l in lines {
            if block.apply(l) {
                complete = true;
            }
        }
        complete
    }

    #[test]
    fn a_block_is_reported_only_once_progress_arrives() {
        let mut b = ProgressBlock::default();
        // Every key except the terminator: nothing to report yet.
        assert!(!feed(&mut b, &BLOCK[..BLOCK.len() - 1]));
        assert!(b.apply("progress=continue"), "progress= terminates a block");
    }

    #[test]
    fn a_full_block_parses_every_field_this_code_uses() {
        let mut b = ProgressBlock::default();
        b.apply("frame=123");
        b.apply("fps=25.00");
        feed(&mut b, BLOCK);

        assert_eq!(b.frame, Some(123));
        assert_eq!(b.fps, Some(25.0));
        assert_eq!(b.bitrate.as_deref(), Some("5000.0kbits/s"));
        assert_eq!(b.speed.as_deref(), Some("1.02x"));
        assert_eq!(b.out_time_us, Some(4_920_000));
        assert!(!b.ended);
    }

    #[test]
    fn out_time_us_wins_over_the_misnamed_out_time_ms() {
        // FFmpeg's `out_time_ms` has carried microseconds for years. Reading it
        // as milliseconds would put progress 1000x ahead and peg the bar at
        // 99% within the first second.
        let mut b = ProgressBlock::default();
        b.apply("out_time_us=4920000");
        b.apply("out_time_ms=4920000");
        assert_eq!(b.out_time_us, Some(4_920_000));
        assert_eq!(b.out_time_us.unwrap() / 1000, 4_920, "4.92 s in ms");
    }

    #[test]
    fn out_time_ms_is_used_when_out_time_us_is_absent() {
        // Older builds emit only `out_time_ms` -- still microseconds.
        let mut b = ProgressBlock::default();
        b.apply("out_time_ms=2000000");
        assert_eq!(b.out_time_us, Some(2_000_000));
    }

    #[test]
    fn progress_end_is_recognised_as_the_last_block() {
        let mut b = ProgressBlock::default();
        assert!(b.apply("progress=end"));
        assert!(b.ended);
    }

    #[test]
    fn not_available_values_are_ignored_rather_than_parsed_as_zero() {
        // FFmpeg writes N/A before it can compute a field. Treating that as 0
        // made the UI show 0 fps and an empty speed on every early block.
        let mut b = ProgressBlock::default();
        b.apply("fps=N/A");
        b.apply("speed=N/A");
        b.apply("bitrate=N/A");
        assert_eq!(b.fps, None);
        assert_eq!(b.speed, None);
        assert_eq!(b.bitrate, None);
    }

    #[test]
    fn junk_lines_do_not_terminate_or_corrupt_a_block() {
        let mut b = ProgressBlock::default();
        assert!(!b.apply("this is not a key value line"));
        assert!(!b.apply(""));
        assert!(!b.apply("frame=notanumber"));
        assert_eq!(b.frame, None, "an unparseable value is dropped, not zeroed");
    }

    #[test]
    fn real_ffmpeg_output_with_padded_values_parses() {
        // Captured verbatim from FFmpeg 7 on 2026-09-18. Note the leading
        // spaces inside the values -- FFmpeg pads them to a fixed width, and a
        // parser that does not trim gets " 9.7x" and "   0.1kbits/s" into the
        // UI.
        let real = [
            "frame=234",
            "fps=225.30",
            "stream_0_0_q=18.0",
            "bitrate= 208.1kbits/s",
            "total_size=262192",
            "out_time_us=10077460",
            "out_time_ms=10077460",
            "out_time=00:00:10.077460",
            "dup_frames=0",
            "drop_frames=0",
            "speed= 9.7x",
            "progress=continue",
        ];

        let mut b = ProgressBlock::default();
        assert!(feed(&mut b, &real));
        assert_eq!(b.frame, Some(234));
        assert_eq!(b.fps, Some(225.30));
        assert_eq!(b.bitrate.as_deref(), Some("208.1kbits/s"));
        assert_eq!(b.speed.as_deref(), Some("9.7x"));
        assert_eq!(b.out_time_us, Some(10_077_460));
        // 10.08 s of a 120 s clip.
        assert_eq!(b.out_time_us.unwrap() / 1000, 10_077);
    }

    #[test]
    fn the_argument_builder_asks_for_machine_readable_progress() {
        // The whole fix depends on these three arguments being present.
        let config = crate::config::AppConfig::default();
        let profile = crate::profiles::EncodingProfile::by_id(crate::profiles::ProfileId::ProfileA);
        let args = profile.build_ffmpeg_args(&config, "in.mxf", "out.mp4", 25, 1);

        assert!(args.iter().any(|a| a == "-nostats"), "{:?}", args);
        let i = args
            .iter()
            .position(|a| a == "-progress")
            .expect("-progress must be passed");
        assert_eq!(args[i + 1], "pipe:1");
        assert!(
            !args.iter().any(|a| a == "-stats"),
            "-stats would put the carriage-return status line back on stderr"
        );
    }
}
