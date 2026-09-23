use crate::bootstrap::ToolPaths;
use crate::profiles::ProfileId;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

/// Upper bound on one ffprobe of a source (PL-01). A healthy probe of even a
/// multi-hour MXF on a share finishes in seconds.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// One audio stream as ffprobe reports it.
///
/// Only the first stream used to be probed, so an MXF with eight mono PCM
/// tracks (the XDCAM HD422 layout this station ingests) was encoded from
/// track 1 alone: a mono programme duplicated to both channels, the right-hand
/// channel of the mix lost (slice 5 #2).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct AudioStreamInfo {
    /// Audio-relative index: the `N` in `0:a:N`.
    pub index: usize,
    pub codec: String,
    pub channels: i64,
    /// ffprobe's `channel_layout`; empty when it reports `unknown` or nothing.
    pub channel_layout: String,
    pub sample_rate: i64,
}

/// How the pictures are scanned, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ScanType {
    Progressive,
    Tff,
    Bff,
}

/// Multi-frame counts from an `idet` pass over real frames (slice 5 #6).
///
/// The container flag is wrong often enough to matter: PsF material is
/// routinely flagged `tt`, and some 576i captures carry no flag at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct IdetCounts {
    pub tff: i64,
    pub bff: i64,
    pub progressive: i64,
    pub undetermined: i64,
}

/// Fewer determined frames than this and the pass says nothing useful: a
/// static slate or colour bars is "undetermined" whatever its scan.
const IDET_MIN_DETERMINED: i64 = 25;
/// Frames the idet pass decodes: 24 s of 25 fps, ~1-3 s of wall time for
/// 1080-line MPEG-2 or H.264 on this station's hosts.
pub const IDET_FRAMES: u32 = 600;

impl IdetCounts {
    /// The scan the frames show, falling back to the container flag when the
    /// counts are too thin or too mixed to overrule it.
    pub fn decide(&self, container: ScanType) -> ScanType {
        let interlaced = self.tff + self.bff;
        let determined = interlaced + self.progressive;
        if determined < IDET_MIN_DETERMINED {
            return container;
        }
        let ratio = interlaced as f64 / determined as f64;
        let dominant = self.tff.max(self.bff);
        let minority = self.tff.min(self.bff);
        // Real interlace has one field order: 1080i50 XDCAM measured TFF
        // 312 / BFF 0, a 576i capture BFF 294 / TFF 0. Sharp synthetic
        // motion in *progressive* frames reads as both (testsrc2 25p: TFF 111
        // / BFF 78 / progressive 61), so a split verdict overrules nothing.
        let consistent = minority * 4 <= dominant;
        if dominant as f64 / determined as f64 >= 0.35 && consistent {
            if self.tff >= self.bff {
                ScanType::Tff
            } else {
                ScanType::Bff
            }
        } else if ratio <= 0.05 {
            ScanType::Progressive
        } else {
            container
        }
    }
}

/// Integrated loudness and true peak re-measured on an encoded mezzanine,
/// for QC (slice 5 #12). Only ever set on an output probe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OutputLoudness {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
}

/// What the resolved ffmpeg can do, checked once per binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct FfmpegCaps {
    /// `zscale` and `tonemap` are both present (HDR -> SDR tone mapping).
    pub zscale_tonemap: bool,
    /// Built with libsoxr. The gyan.dev *essentials* build is not.
    pub soxr: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
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
    /// `num:den`, empty when ffprobe reports none (`N/A`, `0:1`).
    pub sample_aspect_ratio: String,
    pub pix_fmt: String,
    pub color_space: String,
    pub color_transfer: String,
    pub color_primaries: String,
    pub color_range: String,
    /// `avg_frame_rate`, 0/0 when absent. H.264 1080i reports
    /// `r_frame_rate=50/1` (field rate) with `avg_frame_rate=25/1`.
    pub avg_fps_num: i64,
    pub avg_fps_den: i64,
    pub audio_channel_layout: String,
    /// Every audio stream, in order. Empty for probes built by hand in tests;
    /// see [`ProbeData::audio_stream_list`].
    pub audio_streams: Vec<AudioStreamInfo>,
    /// Set by [`probe_source`] only; output probes trust the container flag.
    pub idet: Option<IdetCounts>,
    /// Set by [`probe_source`] only.
    pub ffmpeg_caps: Option<FfmpegCaps>,
    /// Set on output probes by the QC re-measure only.
    pub output_loudness: Option<OutputLoudness>,
}

impl ProbeData {
    pub fn fps(&self) -> f64 {
        if self.fps_den > 0 {
            self.fps_num as f64 / self.fps_den as f64
        } else {
            25.0
        }
    }

    /// Stored frames per second: `avg_frame_rate` when it is sane, else the
    /// snapped `r_frame_rate`. A VFR screen capture reported `r_frame_rate`
    /// 48/1 against an average of 24.4, and must not be taken for 48p.
    pub fn frame_rate(&self) -> f64 {
        if self.avg_fps_num > 0 && self.avg_fps_den > 0 {
            let avg = self.avg_fps_num as f64 / self.avg_fps_den as f64;
            if avg.is_finite() && (1.0..=300.0).contains(&avg) {
                return avg;
            }
        }
        self.fps()
    }

    /// The scan the container claims.
    pub fn container_scan(&self) -> ScanType {
        // `tb` = top coded first, bottom displayed first; display order is
        // what matters for field order.
        match self.field_order.as_str() {
            "tt" | "tff" | "bt" => ScanType::Tff,
            "bb" | "bff" | "tb" => ScanType::Bff,
            _ => ScanType::Progressive,
        }
    }

    /// The scan the pictures actually have: the idet verdict when a source
    /// pass ran, else the container flag.
    pub fn scan(&self) -> ScanType {
        match &self.idet {
            Some(counts) => counts.decide(self.container_scan()),
            None => self.container_scan(),
        }
    }

    pub fn is_interlaced(&self) -> bool {
        self.scan() != ScanType::Progressive
    }

    /// Distinct moments in time per second: the field rate of interlaced
    /// material, the frame rate of progressive.
    pub fn motion_rate(&self) -> f64 {
        let f = self.frame_rate();
        if self.is_interlaced() {
            f * 2.0
        } else {
            f
        }
    }

    /// The CasparCG channel is 1080i50. Anything with ~50 or more motion
    /// samples a second (50i, 50p, 59.94i/p) keeps its motion as 50 fields a
    /// second rather than being decimated to 25p (slice 6c).
    pub fn wants_interlaced_output(&self) -> bool {
        self.motion_rate() >= 40.0
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self.color_transfer.as_str(), "smpte2084" | "arib-std-b67")
    }

    /// Standard definition by raster, for the untagged-colour default.
    pub fn is_sd_raster(&self) -> bool {
        self.height < 700 && self.width < 1280
    }

    pub fn has_valid_audio(&self) -> bool {
        self.audio_sample_rate > 0 && self.audio_channels > 0
    }

    /// The audio streams, or a single one synthesised from the first-stream
    /// fields when the list was not populated.
    pub fn audio_stream_list(&self) -> Vec<AudioStreamInfo> {
        if !self.audio_streams.is_empty() {
            return self.audio_streams.clone();
        }
        if self.has_valid_audio() {
            vec![AudioStreamInfo {
                index: 0,
                codec: self.audio_codec.clone(),
                channels: self.audio_channels,
                channel_layout: self.audio_channel_layout.clone(),
                sample_rate: self.audio_sample_rate,
            }]
        } else {
            Vec::new()
        }
    }

    /// Profile routing (slice 6c).
    ///
    /// - B: anything that should reach air as true 1080i50 -- 1080i50 passed
    ///   through field for field, and 50p / 576i / 59.94 converted to 50
    ///   fields a second.
    /// - A: progressive HD at <= 30 fps, normalised to 25p.
    /// - C: progressive SD at <= 30 fps, normalised to 25p.
    ///
    /// All three report 25/1 frames a second, as B always has.
    pub fn profile_id(&self) -> ProfileId {
        if self.wants_interlaced_output() {
            ProfileId::ProfileB
        } else if self.height >= 700 {
            ProfileId::ProfileA
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
            Self::Str(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0),
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
                if trimmed.is_empty() || trimmed == "0/0" {
                    return None;
                }
                if let Some((n, d)) = trimmed.split_once('/') {
                    let n = n.trim().parse::<f64>().ok()?;
                    let d = d.trim().parse::<f64>().ok()?;
                    if d.abs() < f64::EPSILON {
                        return None;
                    }
                    let r = n / d;
                    if r.is_finite() && r > 0.0 {
                        Some(r)
                    } else {
                        None
                    }
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
    sample_aspect_ratio: Option<FfprobeValue>,
    pix_fmt: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_range: Option<String>,
    channel_layout: Option<String>,
}

/// ffprobe writes `unknown` for an unset property; treat it as unset.
fn known_tag(v: Option<&String>) -> String {
    match v.map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if !s.is_empty() && s != "unknown" && s != "unspecified" && s != "n/a" => s,
        _ => String::new(),
    }
}

/// `num:den` with both parts positive, else empty.
fn valid_ratio_string(v: Option<&FfprobeValue>) -> String {
    let Some(s) = v.and_then(|v| v.as_opt_string()) else {
        return String::new();
    };
    match crate::profiles::parse_ratio(&s) {
        Some((n, d)) => format!("{}:{}", n, d),
        None => String::new(),
    }
}

fn audio_streams_from(streams: &[StreamInfo]) -> Vec<AudioStreamInfo> {
    streams
        .iter()
        .filter(|s| s.codec_type == "audio")
        .enumerate()
        .map(|(index, s)| AudioStreamInfo {
            index,
            codec: s.codec_name.clone().unwrap_or_else(|| "none".into()),
            channels: s.channels.as_ref().and_then(|v| v.parse_i64()).unwrap_or(0),
            channel_layout: known_tag(s.channel_layout.as_ref()),
            sample_rate: s.sample_rate.as_ref().and_then(|v| v.parse_i64()).unwrap_or(0),
        })
        .collect()
}

#[derive(Deserialize, Debug)]
struct FormatInfo {
    duration: Option<FfprobeValue>,
}

pub fn probe_media(tools: &ToolPaths, input_path: &Path) -> Result<ProbeData, String> {
    let mut command = Command::new(&tools.ffprobe);
    command.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_streams",
        "-show_format",
    ]);
    command.arg(input_path);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    // Bounded (PL-01): a source on a share that stops answering used to hold
    // this call, and the job's concurrency slot, forever.
    let output = crate::child::output_with_timeout(&mut command, PROBE_TIMEOUT)
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
    let width = vstream.and_then(|s| s.width).unwrap_or(0) as i64;
    let height = vstream.and_then(|s| s.height).unwrap_or(0) as i64;
    let raw_audio_sample_rate = astream
        .and_then(|s| s.sample_rate.as_ref())
        .and_then(|v| v.parse_i64())
        .unwrap_or(0);
    let raw_audio_channels = astream
        .and_then(|s| s.channels.as_ref())
        .and_then(|v| v.parse_i64())
        .unwrap_or(0);

    let (audio_sample_rate, audio_channels, audio_codec) =
        if raw_audio_sample_rate == 0 || raw_audio_channels == 0 {
            (0, 0, "none".to_string())
        } else {
            let codec = astream
                .and_then(|s| s.codec_name.clone())
                .unwrap_or_else(|| "none".into());
            (raw_audio_sample_rate, raw_audio_channels, codec)
        };

    let (fps_num_raw, fps_den_raw) = vstream
        .and_then(|s| s.r_frame_rate.as_ref())
        .map(parse_fps_value)
        .unwrap_or((25, 1));

    let (avg_fps_num, avg_fps_den) = vstream
        .and_then(|s| s.avg_frame_rate.as_ref())
        .filter(|v| v.parse_ratio().is_some())
        .map(parse_fps_value)
        .unwrap_or((0, 0));

    let field_order = vstream
        .and_then(|s| s.field_order.as_ref())
        .and_then(|v| v.as_opt_string())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // H.264 1080i from AVCHD / broadcast encoders reports the field rate as
    // `r_frame_rate` (50/1) and the frame rate as `avg_frame_rate` (25/1).
    // The frame rate is the one that means anything downstream.
    let (fps_num, fps_den) = {
        let snapped = snap_fps_rational(fps_num_raw, fps_den_raw);
        let interlaced_flag = matches!(field_order.as_str(), "tt" | "bb" | "tb" | "bt");
        if interlaced_flag && avg_fps_num > 0 && avg_fps_den > 0 {
            let r = snapped.0 as f64 / snapped.1 as f64;
            let avg = avg_fps_num as f64 / avg_fps_den as f64;
            if (r / avg - 2.0).abs() < 0.02 {
                snap_fps_rational(avg_fps_num, avg_fps_den)
            } else {
                snapped
            }
        } else {
            snapped
        }
    };

    let audio_streams = audio_streams_from(&parsed.streams);

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
        sample_aspect_ratio: valid_ratio_string(vstream.and_then(|s| s.sample_aspect_ratio.as_ref())),
        pix_fmt: known_tag(vstream.and_then(|s| s.pix_fmt.as_ref())),
        color_space: known_tag(vstream.and_then(|s| s.color_space.as_ref())),
        color_transfer: known_tag(vstream.and_then(|s| s.color_transfer.as_ref())),
        color_primaries: known_tag(vstream.and_then(|s| s.color_primaries.as_ref())),
        color_range: known_tag(vstream.and_then(|s| s.color_range.as_ref())),
        avg_fps_num,
        avg_fps_den,
        audio_channel_layout: known_tag(astream.and_then(|s| s.channel_layout.as_ref())),
        audio_streams,
        idet: None,
        ffmpeg_caps: None,
        output_loudness: None,
    })
}

/// [`probe_media`] plus the analysis only a *source* needs: an idet pass over
/// real frames, and the ffmpeg capability check that decides whether an HDR
/// source can be tone-mapped. Output probes skip both.
pub fn probe_source(tools: &ToolPaths, input_path: &Path) -> Result<ProbeData, String> {
    let mut probe = probe_media(tools, input_path)?;
    match detect_interlace(tools, input_path, probe.duration_secs) {
        Ok(counts) => probe.idet = counts,
        // Not fatal: the container flag is what V1 always used.
        Err(e) => tracing::warn!(
            "idet pass failed for {}: {} -- trusting the container field order",
            input_path.display(),
            e
        ),
    }
    probe.ffmpeg_caps = Some(ffmpeg_caps(tools));
    Ok(probe)
}

/// Where the idet pass starts: past the head of a long file, where slates,
/// black and bars say nothing about the scan.
pub fn idet_start_secs(duration_secs: f64) -> f64 {
    if duration_secs > 60.0 {
        (duration_secs * 0.2).min(300.0)
    } else if duration_secs > 30.0 {
        5.0
    } else {
        0.0
    }
}

/// Counts from the last `Multi frame detection` line. ffmpeg can print the
/// idet summary twice (a first, empty filter graph is torn down when the
/// stream parameters settle), so the last line is the real one.
pub fn parse_idet_multi(stderr: &str) -> Option<IdetCounts> {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.contains("Multi frame detection:"))?;
    let tail = &line[line.find("Multi frame detection:")? + "Multi frame detection:".len()..];
    let value_after = |key: &str| -> Option<i64> {
        let at = tail.find(key)? + key.len();
        tail[at..].split_whitespace().next()?.parse().ok()
    };
    Some(IdetCounts {
        tff: value_after("TFF:")?,
        bff: value_after("BFF:")?,
        progressive: value_after("Progressive:")?,
        undetermined: value_after("Undetermined:").unwrap_or(0),
    })
}

/// Bounded idet pass: [`IDET_FRAMES`] decoded frames, video only.
fn detect_interlace(
    tools: &ToolPaths,
    input_path: &Path,
    duration_secs: f64,
) -> Result<Option<IdetCounts>, String> {
    let start = idet_start_secs(duration_secs);
    let mut command = Command::new(&tools.ffmpeg);
    command.args(["-hide_banner", "-nostats", "-nostdin"]);
    if start > 0.0 {
        command.args(["-ss", &format!("{:.3}", start)]);
    }
    command.arg("-i");
    command.arg(input_path);
    command.args([
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-vf",
        "idet",
        "-frames:v",
        &IDET_FRAMES.to_string(),
        "-f",
        "null",
        "-",
    ]);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    // Bounded and interruptible like every probe helper (PL-01).
    let output = crate::child::output_with_timeout(&mut command, PROBE_TIMEOUT)
        .map_err(|e| format!("idet exec failed: {}", e))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!("idet exited {:?}", output.status.code()));
    }
    Ok(parse_idet_multi(&stderr))
}

static CAPS_CACHE: std::sync::Mutex<Option<(std::path::PathBuf, FfmpegCaps)>> =
    std::sync::Mutex::new(None);

/// Parse `ffmpeg -filters` and `-buildconf` output into capabilities.
pub fn parse_ffmpeg_caps(filters: &str, buildconf: &str) -> FfmpegCaps {
    let has = |name: &str| {
        filters
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name))
    };
    FfmpegCaps {
        zscale_tonemap: has("zscale") && has("tonemap"),
        soxr: buildconf.contains("--enable-libsoxr"),
    }
}

/// Capabilities of the resolved ffmpeg, cached per binary path.
pub fn ffmpeg_caps(tools: &ToolPaths) -> FfmpegCaps {
    if let Ok(guard) = CAPS_CACHE.lock() {
        if let Some((path, caps)) = guard.as_ref() {
            if *path == tools.ffmpeg {
                return *caps;
            }
        }
    }
    let run = |arg: &str| -> String {
        let mut command = Command::new(&tools.ffmpeg);
        command.args(["-hide_banner", arg]);
        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        crate::child::output_with_timeout(&mut command, std::time::Duration::from_secs(30))
            .map(|o| {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                s
            })
            .unwrap_or_default()
    };
    let caps = parse_ffmpeg_caps(&run("-filters"), &run("-buildconf"));
    if let Ok(mut guard) = CAPS_CACHE.lock() {
        *guard = Some((tools.ffmpeg.clone(), caps));
    }
    caps
}

/// Every audio stream of `input_path`, for the loudness pass (which is handed
/// only a path and a channel count).
pub fn probe_audio_streams(
    tools: &ToolPaths,
    input_path: &Path,
) -> Result<Vec<AudioStreamInfo>, String> {
    #[derive(Deserialize)]
    struct StreamsOnly {
        streams: Vec<StreamInfo>,
    }
    let mut command = Command::new(&tools.ffprobe);
    command.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_streams",
        "-select_streams",
        "a",
    ]);
    command.arg(input_path);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    let output = crate::child::output_with_timeout(&mut command, PROBE_TIMEOUT)
        .map_err(|e| format!("ffprobe exec failed: {}", e))?;
    if !output.status.success() {
        return Err("ffprobe failed on audio streams".into());
    }
    let parsed: StreamsOnly = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("ffprobe JSON parse failed: {}", e))?;
    Ok(audio_streams_from(&parsed.streams))
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
        let fps = vs
            .avg_frame_rate
            .as_ref()
            .and_then(|v| v.parse_ratio())
            .or_else(|| vs.r_frame_rate.as_ref().and_then(|v| v.parse_ratio()));
        if let (Some(frames), Some(fps)) = (vs.nb_frames.as_ref().and_then(|v| v.parse_i64()), fps)
        {
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
        FfprobeValue::Num(n) => ((n * 1000.0).round() as i64, 1000),
    }
}

pub fn snap_fps_rational(num: i64, den: i64) -> (i64, i64) {
    if den <= 0 {
        return (25, 1);
    }
    let fps = num as f64 / den as f64;
    let near = |a: f64, b: f64| (a - b).abs() < 0.05;
    if near(fps, 29.97) {
        return (30000, 1001);
    }
    if near(fps, 23.976) {
        return (24000, 1001);
    }
    if near(fps, 59.94) {
        return (60000, 1001);
    }
    if near(fps, 25.0) {
        return (25, 1);
    }
    if near(fps, 50.0) {
        return (50, 1);
    }
    if near(fps, 30.0) {
        return (30, 1);
    }
    if near(fps, 60.0) {
        return (60, 1);
    }
    if near(fps, 24.0) {
        return (24, 1);
    }
    (num, den)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeasuredLoudness {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
    pub is_linear: bool,
    pub target_i: f64,
    pub target_tp: f64,
    pub target_lra: f64,
    pub is_silent: bool,
    pub is_short: bool,
}

#[derive(Deserialize, Debug)]
struct RawLoudnormJson {
    input_i: String,
    input_tp: String,
    input_lra: String,
    input_thresh: String,
    target_offset: Option<String>,
}

pub fn parse_loudnorm_json(
    stderr: &str,
    target_i: f64,
    target_tp: f64,
    target_lra: f64,
    duration_secs: f64,
) -> Result<MeasuredLoudness, String> {
    // Slice from the loudnorm summary itself (slice 5 #7). A brace anywhere
    // earlier in stderr -- a `{GUID}` in a Windows volume path, a codec
    // private-data dump -- used to become the start of "the JSON". The object
    // is flat, so it ends at the first closing brace after it opens.
    let tail = match stderr.rfind("[Parsed_loudnorm") {
        Some(i) => &stderr[i..],
        None => stderr,
    };
    let start = tail
        .find('{')
        .ok_or_else(|| "No JSON found in loudnorm output".to_string())?;
    let end = tail[start..]
        .find('}')
        .map(|e| start + e)
        .ok_or_else(|| "Malformed JSON in loudnorm output".to_string())?;
    let json_str = &tail[start..=end];
    let raw: RawLoudnormJson = serde_json::from_str(json_str)
        .map_err(|e| format!("Failed to parse loudnorm JSON: {}", e))?;

    let parse_num = |s: &str, field: &str| -> Result<f64, String> {
        let trimmed = s.trim();
        if trimmed == "-inf" || trimmed == "-Infinity" {
            return Ok(f64::NEG_INFINITY);
        }
        if trimmed == "inf" || trimmed == "Infinity" {
            return Ok(f64::INFINITY);
        }
        let val: f64 = trimmed
            .parse()
            .map_err(|e| format!("Invalid numeric value for {}: {}", field, e))?;
        if val.is_nan() {
            return Err(format!("NaN value for {}", field));
        }
        Ok(val)
    };

    let input_i = parse_num(&raw.input_i, "input_i")?;
    let input_tp = parse_num(&raw.input_tp, "input_tp")?;
    let input_lra = parse_num(&raw.input_lra, "input_lra")?;
    let input_thresh = parse_num(&raw.input_thresh, "input_thresh")?;

    let target_offset = if let Some(ref to) = raw.target_offset {
        parse_num(to, "target_offset")?
    } else if input_i.is_finite() {
        target_i - input_i
    } else {
        0.0
    };

    let is_silent = input_i <= -70.0 || input_i.is_infinite();
    let is_short = duration_secs > 0.0 && duration_secs < 3.0;

    // Linear (a single static gain) is requested whenever that gain cannot
    // push the true peak over target. LRA is deliberately not a gate any more
    // (slice 5 #8): refusing linear above 1.5x the LRA target forced dynamic
    // mode -- audible pumping -- onto exactly the wide-range programmes
    // (drama, music) that a static gain suits best. loudnorm itself falls
    // back to dynamic when it cannot honour linear, so asking is safe.
    let is_linear = if is_silent || is_short {
        false
    } else {
        let projected_tp = input_tp + (target_i - input_i);
        projected_tp <= target_tp + 0.001
    };

    Ok(MeasuredLoudness {
        input_i,
        input_tp,
        input_lra,
        input_thresh,
        target_offset,
        is_linear,
        target_i,
        target_tp,
        target_lra,
        is_silent,
        is_short,
    })
}

pub trait LoudnessMeasurer: Send + Sync {
    fn measure_loudness(
        &self,
        tools: &ToolPaths,
        input_path: &Path,
        channels: i64,
        duration_secs: f64,
        policy: &crate::config::AudioPolicy,
    ) -> Result<Option<MeasuredLoudness>, String>;
}

#[derive(Default, Clone)]
pub struct RealLoudnessMeasurer;

impl LoudnessMeasurer for RealLoudnessMeasurer {
    fn measure_loudness(
        &self,
        tools: &ToolPaths,
        input_path: &Path,
        channels: i64,
        duration_secs: f64,
        policy: &crate::config::AudioPolicy,
    ) -> Result<Option<MeasuredLoudness>, String> {
        if !policy.mode.measures() || channels <= 0 {
            return Ok(None);
        }

        let (target_i, target_tp, target_lra) = crate::profiles::resolve_loudness_targets(policy);

        // The pass must hear exactly what the encode will: the same stream
        // selection (two mono MXF tracks joined into L/R) and the same
        // downmix. The trait hands over only a path and a channel count, so
        // the stream list is re-read here; it costs one ffprobe.
        let streams = probe_audio_streams(tools, input_path).unwrap_or_default();
        let streams = if streams.is_empty() {
            vec![AudioStreamInfo {
                index: 0,
                codec: String::new(),
                channels,
                channel_layout: String::new(),
                sample_rate: 48000,
            }]
        } else {
            streams
        };
        let source = crate::profiles::select_audio_source(&streams, policy);
        let loudnorm = format!(
            "loudnorm=I={:.2}:TP={:.2}:LRA={:.2}:print_format=json",
            target_i, target_tp, target_lra
        );
        let graph = crate::profiles::audio_analysis_graph(&source, policy, &loudnorm);
        let Some(graph) = graph else {
            return Ok(None);
        };

        let mut command = Command::new(&tools.ffmpeg);
        command.args([
            "-hide_banner",
            "-nostats",
            "-nostdin",
            "-analyzeduration",
            "500M",
            "-probesize",
            "500M",
            "-i",
        ]);
        command.arg(input_path);
        command.args([
            "-filter_complex",
            &graph,
            "-map",
            "[aout]",
            "-vn",
            "-sn",
            "-dn",
            "-f",
            "null",
            "-",
        ]);

        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

        // Bounded and interruptible (PL-01). This pass decodes the whole
        // programme's audio, so a cancel or a service stop used to wait for all
        // of it. The timeout scales with the programme: roughly 100x realtime
        // is typical, so allowing 1x realtime is generous without being never.
        let timeout = std::time::Duration::from_secs_f64(duration_secs.max(0.0) + 600.0);
        let output = crate::child::output_with_timeout(&mut command, timeout)
            .map_err(|e| format!("Failed to execute FFmpeg loudness measurement: {}", e))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!(
                "FFmpeg loudness measurement failed (exit code {:?}): {}",
                output.status.code(),
                stderr
            ));
        }

        let measured =
            parse_loudnorm_json(&stderr, target_i, target_tp, target_lra, duration_secs)?;
        Ok(Some(measured))
    }
}

/// Integrated loudness and true peak of an encoded mezzanine's first audio
/// stream, for QC (slice 5 #12). Same loudnorm analysis as the source pass,
/// so the two numbers are comparable.
pub fn measure_output_loudness(tools: &ToolPaths, path: &Path) -> Result<OutputLoudness, String> {
    let mut command = Command::new(&tools.ffmpeg);
    command.args(["-hide_banner", "-nostats", "-nostdin", "-i"]);
    command.arg(path);
    command.args([
        "-map",
        "0:a:0",
        "-vn",
        "-sn",
        "-dn",
        "-af",
        "loudnorm=print_format=json",
        "-f",
        "null",
        "-",
    ]);

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    // A decode of the mezzanine's audio only, typically ~100x realtime; half
    // an hour trips on a stalled volume, not on a long programme (PL-01).
    let output =
        crate::child::output_with_timeout(&mut command, std::time::Duration::from_secs(1800))
            .map_err(|e| format!("Failed to execute output loudness measurement: {}", e))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "output loudness measurement failed (exit code {:?})",
            output.status.code()
        ));
    }
    let m = parse_loudnorm_json(&stderr, -23.0, -1.0, 7.0, 60.0)?;
    Ok(OutputLoudness {
        integrated_lufs: m.input_i,
        true_peak_dbtp: m.input_tp,
    })
}

/// A probe of a conforming mezzanine, for tests that vary one property.
#[cfg(test)]
impl ProbeData {
    pub fn conforming_mezzanine_for_tests() -> Self {
        ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "out.mp4".into(),
            sample_aspect_ratio: "1:1".into(),
            pix_fmt: "yuv420p".into(),
            color_space: "bt709".into(),
            color_transfer: "bt709".into(),
            color_primaries: "bt709".into(),
            color_range: "tv".into(),
            ..Default::default()
        }
    }
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

    #[test]
    fn test_parse_loudnorm_json_valid_ebu() {
        let stderr = r#"
[Parsed_loudnorm_0 @ 000001] 
{
	"input_i" : "-24.50",
	"input_tp" : "-3.00",
	"input_lra" : "6.50",
	"input_thresh" : "-35.00",
	"output_i" : "-23.00",
	"output_tp" : "-1.50",
	"output_lra" : "6.50",
	"output_thresh" : "-33.50",
	"normalization_type" : "dynamic",
	"target_offset" : "1.50"
}
"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).unwrap();
        assert_eq!(res.input_i, -24.5);
        assert_eq!(res.input_tp, -3.0);
        assert_eq!(res.input_lra, 6.5);
        assert_eq!(res.input_thresh, -35.0);
        assert_eq!(res.target_offset, 1.5);
        assert!(res.is_linear);
        assert!(!res.is_silent);
        assert!(!res.is_short);
    }

    #[test]
    fn test_parse_loudnorm_json_valid_atsc() {
        let stderr = r#"{
	"input_i" : "-27.00",
	"input_tp" : "-5.00",
	"input_lra" : "10.00",
	"input_thresh" : "-38.00",
	"target_offset" : "3.00"
}"#;
        let res = parse_loudnorm_json(stderr, -24.0, -2.0, 11.0, 30.0).unwrap();
        assert_eq!(res.input_i, -27.0);
        assert_eq!(res.input_tp, -5.0);
        assert_eq!(res.target_i, -24.0);
        assert!(res.is_linear);
    }

    #[test]
    fn test_parse_loudnorm_json_malformed() {
        let stderr = "some ffmpeg error occurred without json";
        assert!(parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).is_err());
    }

    #[test]
    fn test_parse_loudnorm_json_missing_fields() {
        let stderr = r#"{"input_i": "-24.0"}"#;
        assert!(parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).is_err());
    }

    #[test]
    fn test_parse_loudnorm_json_non_finite() {
        let stderr = r#"{"input_i": "NaN", "input_tp": "-1.0", "input_lra": "5.0", "input_thresh": "-30.0"}"#;
        assert!(parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).is_err());
    }

    #[test]
    fn test_parse_loudnorm_json_silent() {
        let stderr = r#"{"input_i": "-inf", "input_tp": "-inf", "input_lra": "0.0", "input_thresh": "-70.0"}"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).unwrap();
        assert!(res.is_silent);
        assert!(!res.is_linear);
    }

    #[test]
    fn test_parse_loudnorm_json_short_clip() {
        let stderr = r#"{"input_i": "-25.0", "input_tp": "-2.0", "input_lra": "5.0", "input_thresh": "-35.0"}"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 2.5).unwrap();
        assert!(res.is_short);
        assert!(!res.is_linear);
    }

    #[test]
    fn test_linear_eligibility_false_due_to_projected_peak() {
        // input_i = -30, input_tp = -0.5, target_i = -23 -> gain = +7dB -> projected_tp = +6.5dB > -1.0dB target_tp
        let stderr = r#"{"input_i": "-30.0", "input_tp": "-0.5", "input_lra": "5.0", "input_thresh": "-40.0"}"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).unwrap();
        assert!(
            !res.is_linear,
            "Projected true peak overshoots target -> linear mode must be false"
        );
    }

    #[test]
    fn a_wide_loudness_range_no_longer_refuses_linear_gain() {
        // Slice 5 #8: input_lra 15 > 1.5 x 7 used to force dynamic mode even
        // though +1 dB of static gain keeps the peak at -4 dBTP.
        let stderr = r#"{"input_i": "-24.0", "input_tp": "-5.0", "input_lra": "15.0", "input_thresh": "-34.0"}"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).unwrap();
        assert!(res.is_linear, "only the projected true peak gates linear");
    }

    #[test]
    fn loudnorm_json_is_found_after_a_guid_in_stderr() {
        // Slice 5 #7: the first `{` in stderr used to be taken as the JSON.
        let stderr = r#"Input #0, mxf, from '\\?\Volume{6f1c2a3b-0000-0000-0000-100000000000}\ingest\clip.mxf':
  Metadata:
    uid             : {a1b2c3d4-e5f6-7890-abcd-ef0123456789}
[Parsed_loudnorm_1 @ 000001]
{
	"input_i" : "-19.80",
	"input_tp" : "-0.40",
	"input_lra" : "9.10",
	"input_thresh" : "-30.10",
	"output_i" : "-23.00",
	"output_tp" : "-1.00",
	"output_lra" : "7.00",
	"output_thresh" : "-33.20",
	"normalization_type" : "dynamic",
	"target_offset" : "0.10"
}
[out#0/null @ 000002] video:0KiB audio:1KiB {trailing brace}
"#;
        let res = parse_loudnorm_json(stderr, -23.0, -1.0, 7.0, 60.0).unwrap();
        assert_eq!(res.input_i, -19.8);
        assert_eq!(res.input_tp, -0.4);
        assert_eq!(res.target_offset, 0.1);
    }

    fn make_probe(width: i64, height: i64, fps: (i64, i64), field_order: &str) -> ProbeData {
        ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width,
            height,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: fps.0,
            fps_den: fps.1,
            field_order: field_order.into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "test.mp4".into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_profile_id_routing_progressive_height_700() {
        // 720p25 progressive -> Profile A (1080p25 HD)
        assert_eq!(make_probe(1280, 720, (25, 1), "progressive").profile_id(), ProfileId::ProfileA);
        // 1080p25 progressive -> Profile A
        assert_eq!(make_probe(1920, 1080, (25, 1), "progressive").profile_id(), ProfileId::ProfileA);
        // 1080i50 -> Profile B
        assert_eq!(make_probe(1920, 1080, (25, 1), "tt").profile_id(), ProfileId::ProfileB);
        // 576p25 SD -> Profile C (height < 700)
        assert_eq!(make_probe(720, 576, (25, 1), "progressive").profile_id(), ProfileId::ProfileC);
    }

    #[test]
    fn fifty_field_sources_route_to_true_interlaced_output() {
        // Slice 6c: the channel is 1080i50, so 50 motion samples a second
        // reach air as 50 fields, not decimated to 25p.
        let p1080p50 = make_probe(1920, 1080, (50, 1), "progressive");
        assert_eq!(p1080p50.profile_id(), ProfileId::ProfileB);
        let p720p50 = make_probe(1280, 720, (50, 1), "progressive");
        assert_eq!(p720p50.profile_id(), ProfileId::ProfileB);
        let p576i = make_probe(720, 576, (25, 1), "bb");
        assert_eq!(p576i.profile_id(), ProfileId::ProfileB);
        let p5994 = make_probe(1920, 1080, (60000, 1001), "progressive");
        assert_eq!(p5994.profile_id(), ProfileId::ProfileB);
        let p480i = make_probe(720, 480, (30000, 1001), "tt");
        assert_eq!(p480i.profile_id(), ProfileId::ProfileB);

        // <= 30 fps progressive stays progressive 25p.
        for fps in [(24000, 1001), (24, 1), (25, 1), (30000, 1001), (30, 1)] {
            assert_eq!(make_probe(1920, 1080, fps, "progressive").profile_id(), ProfileId::ProfileA);
            assert_eq!(make_probe(720, 576, fps, "progressive").profile_id(), ProfileId::ProfileC);
        }
    }

    #[test]
    fn idet_overrules_a_wrong_container_flag() {
        // PsF flagged `tt`: idet sees progressive frames -> progressive, A.
        let mut psf = make_probe(1920, 1080, (25, 1), "tt");
        psf.idet = Some(IdetCounts { tff: 3, bff: 0, progressive: 580, undetermined: 17 });
        assert_eq!(psf.scan(), ScanType::Progressive);
        assert_eq!(psf.profile_id(), ProfileId::ProfileA);

        // Unflagged interlaced SD: idet finds BFF -> interlaced, B.
        let mut unflagged = make_probe(720, 576, (25, 1), "progressive");
        unflagged.idet = Some(IdetCounts { tff: 1, bff: 420, progressive: 100, undetermined: 79 });
        assert_eq!(unflagged.scan(), ScanType::Bff);
        assert_eq!(unflagged.profile_id(), ProfileId::ProfileB);

        // Both orders at once is sharp progressive motion, not interlace.
        let mut synthetic = make_probe(720, 576, (25, 1), "progressive");
        synthetic.idet = Some(IdetCounts { tff: 111, bff: 78, progressive: 61, undetermined: 0 });
        assert_eq!(synthetic.scan(), ScanType::Progressive);
        assert_eq!(synthetic.profile_id(), ProfileId::ProfileC);

        // A static slate decides nothing: the flag stands.
        let mut slate = make_probe(1920, 1080, (25, 1), "tt");
        slate.idet = Some(IdetCounts { tff: 4, bff: 0, progressive: 5, undetermined: 591 });
        assert_eq!(slate.scan(), ScanType::Tff);
    }

    #[test]
    fn the_last_idet_summary_is_the_real_one() {
        let stderr = "\
[Parsed_idet_0 @ 000002723914dde0] Multi frame detection: TFF:     0 BFF:     0 Progressive:     0 Undetermined:     0
[Parsed_idet_0 @ 000002723914d2a0] Single frame detection: TFF:   292 BFF:     0 Progressive:     9 Undetermined:     0
[Parsed_idet_0 @ 000002723914d2a0] Multi frame detection: TFF:   301 BFF:     0 Progressive:     0 Undetermined:     0
";
        assert_eq!(
            parse_idet_multi(stderr),
            Some(IdetCounts { tff: 301, bff: 0, progressive: 0, undetermined: 0 })
        );
        assert_eq!(parse_idet_multi("no idet here"), None);
        assert_eq!(idet_start_secs(12.5), 0.0);
        assert_eq!(idet_start_secs(45.0), 5.0);
        assert_eq!(idet_start_secs(154.0), 154.0 * 0.2);
        assert_eq!(idet_start_secs(7200.0), 300.0);
    }

    #[test]
    fn a_vfr_capture_is_not_mistaken_for_48p() {
        // bbc-africa_m720p.mov: r_frame_rate 48/1, avg 3959400/162101 (24.4).
        let mut p = make_probe(1266, 720, (48, 1), "progressive");
        p.avg_fps_num = 3959400;
        p.avg_fps_den = 162101;
        assert!((p.frame_rate() - 24.43).abs() < 0.01);
        assert_eq!(p.profile_id(), ProfileId::ProfileA);
    }

    #[test]
    fn ffmpeg_capabilities_are_read_from_filters_and_buildconf() {
        let filters = " TS bwdif             V->V       Deinterlace the input image.\n \
                       .S tonemap           V->V       Conversion to/from different dynamic ranges.\n \
                       .S zscale            V->V       Apply resizing, colorspace and bit depth conversion.\n";
        let caps = parse_ffmpeg_caps(filters, "  --enable-libx264\n  --enable-libzimg\n");
        assert!(caps.zscale_tonemap);
        assert!(!caps.soxr, "the gyan.dev essentials build has no libsoxr");
        let caps = parse_ffmpeg_caps(" TS bwdif  V->V  x\n", "--enable-libsoxr");
        assert!(!caps.zscale_tonemap);
        assert!(caps.soxr);
    }

    #[test]
    fn test_corrupt_audio_detection() {
        let p_corrupt = ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "none".into(),
            audio_sample_rate: 0,
            audio_channels: 0,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "test.mp4".into(),
            ..Default::default()
        };
        assert!(!p_corrupt.has_valid_audio());
    }
}
