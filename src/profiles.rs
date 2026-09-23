use crate::config::{AppConfig, AudioMode, AudioPolicy, ProfileConfig};
use crate::probe::{AudioStreamInfo, ProbeData, ScanType};
use serde::{Deserialize, Serialize};

pub const TARGET_FPS_NUM: i64 = 25;
pub const TARGET_FPS_DEN: i64 = 1;
pub const TARGET_WIDTH: i64 = 1920;
pub const TARGET_HEIGHT: i64 = 1080;

/// Intermediate progressive rate for interlaced output: one picture per
/// field, woven back into 25 interlaced frames by `interlace`.
const FIELD_RATE: i64 = 50;

/// Every resize: lanczos, with exact rounding and full-resolution chroma
/// interpolation (slice 5 #1). swscale's default bicubic softened SD
/// upconversions visibly.
const SCALE_FLAGS: &str = "lanczos+accurate_rnd+full_chroma_int";

/// Last filter of every chain: what the pixels are after the scaler.
const OUTPUT_COLOR_PARAMS: &str =
    "setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileId {
    ProfileA,
    ProfileB,
    ProfileC,
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileId::ProfileA => write!(f, "ProfileA"),
            ProfileId::ProfileB => write!(f, "ProfileB"),
            ProfileId::ProfileC => write!(f, "ProfileC"),
        }
    }
}

/// The descriptor served by `GET /api/v2/profiles`.
///
/// This registry used to list five profiles, two of which (720p50 and ProRes
/// 1080i50) no code path has ever produced, with CRF and rate values that
/// matched nothing the encoder used (slice 5 #13). It now describes the three
/// profiles the builder below actually emits, with the shipped defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BroadcastVideoProfile {
    pub name: String,
    pub description: String,
    pub container: String,
    pub video_codec: String,
    pub pix_fmt: String,
    pub width: i32,
    pub height: i32,
    pub fps_num: i64,
    pub fps_den: i64,
    pub interlaced: bool,
    pub field_order: String,
    pub gop_size_secs: f64,
    pub closed_gop: bool,
    pub colorspace: String,
    pub color_trc: String,
    pub color_primaries: String,
    pub video_profile: Option<String>,
    pub video_level: Option<String>,
    pub crf: Option<u32>,
    pub maxrate: Option<String>,
    pub bufsize: Option<String>,
    pub faststart: bool,
}

impl BroadcastVideoProfile {
    fn describe(id: ProfileId, name: &str, description: &str) -> Self {
        let p = EncodingProfile::by_id(id);
        let cfg = match id {
            ProfileId::ProfileA => ProfileConfig::profile_a_default(),
            ProfileId::ProfileB => ProfileConfig::profile_b_default(),
            ProfileId::ProfileC => ProfileConfig::profile_c_default(),
        };
        Self {
            name: name.to_string(),
            description: description.to_string(),
            container: "mp4".to_string(),
            video_codec: "libx264".to_string(),
            pix_fmt: "yuv420p".to_string(),
            width: p.target_width,
            height: p.target_height,
            fps_num: TARGET_FPS_NUM,
            fps_den: TARGET_FPS_DEN,
            interlaced: p.interlaced,
            field_order: if p.interlaced { "tff" } else { "progressive" }.to_string(),
            gop_size_secs: 2.0,
            closed_gop: true,
            colorspace: p.colorspace.to_string(),
            color_trc: p.color_trc.to_string(),
            color_primaries: p.color_primaries.to_string(),
            video_profile: Some(p.profile_h264.to_string()),
            video_level: Some(p.level_h264.to_string()),
            crf: Some(cfg.crf as u32),
            maxrate: Some(cfg.maxrate),
            bufsize: Some(cfg.bufsize),
            faststart: true,
        }
    }

    pub fn playoutvue_h264_1080p25() -> Self {
        Self::describe(
            ProfileId::ProfileA,
            "playoutvue-h264-1080p25",
            "HD progressive <= 30 fps, normalised to 1080p25 BT.709, DAR-fitted (Profile A)",
        )
    }

    pub fn playoutvue_h264_1080i50() -> Self {
        Self::describe(
            ProfileId::ProfileB,
            "playoutvue-h264-1080i50",
            "1080i50 TFF BT.709: 1080i50 kept field for field; 50p, 576i and 59.94 \
             sources converted to 50 fields/s (Profile B)",
        )
    }

    pub fn playoutvue_h264_1080p25_sd_pal() -> Self {
        Self::describe(
            ProfileId::ProfileC,
            "playoutvue-h264-1080p25-sd-pal",
            "SD progressive <= 30 fps upconverted to 1080p25 BT.709, pillarboxed per DAR (Profile C)",
        )
    }
}

pub fn get_standard_broadcast_profiles() -> Vec<BroadcastVideoProfile> {
    vec![
        BroadcastVideoProfile::playoutvue_h264_1080p25(),
        BroadcastVideoProfile::playoutvue_h264_1080i50(),
        BroadcastVideoProfile::playoutvue_h264_1080p25_sd_pal(),
    ]
}

pub fn find_broadcast_profile(name: &str) -> Option<BroadcastVideoProfile> {
    get_standard_broadcast_profiles().into_iter().find(|p| {
        p.name.eq_ignore_ascii_case(name)
            || match (name.to_lowercase().as_str(), p.name.as_str()) {
                ("profilea" | "profile_a" | "a", "playoutvue-h264-1080p25") => true,
                ("profileb" | "profile_b" | "b", "playoutvue-h264-1080i50") => true,
                ("profilec" | "profile_c" | "c", "playoutvue-h264-1080p25-sd-pal") => true,
                _ => false,
            }
    })
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct EncodingProfile {
    pub id: ProfileId,
    pub target_width: i32,
    pub target_height: i32,
    pub interlaced: bool,
    pub sar: Option<&'static str>,
    pub colorspace: &'static str,
    pub color_trc: &'static str,
    pub color_primaries: &'static str,
    pub profile_h264: &'static str,
    pub level_h264: &'static str,
}

const PROFILE_A: EncodingProfile = EncodingProfile {
    id: ProfileId::ProfileA,
    target_width: 1920,
    target_height: 1080,
    interlaced: false,
    sar: None,
    colorspace: "bt709",
    color_trc: "bt709",
    color_primaries: "bt709",
    profile_h264: "high",
    level_h264: "4.2",
};

const PROFILE_B: EncodingProfile = EncodingProfile {
    id: ProfileId::ProfileB,
    target_width: 1920,
    target_height: 1080,
    interlaced: true,
    sar: None,
    colorspace: "bt709",
    color_trc: "bt709",
    color_primaries: "bt709",
    profile_h264: "high",
    level_h264: "4.2",
};

/// BT.709 like A and B (slice 5 #3). C used to tag `smpte170m` on a
/// 1920x1080 raster: an HD decoder (CasparCG included) reads untagged or
/// 601-tagged HD as 709 regardless, so the tag was either ignored or, where
/// honoured, produced a second, different colour shift. The pixels are now
/// converted 601 -> 709 in the scaler and tagged for what they are.
const PROFILE_C: EncodingProfile = EncodingProfile {
    id: ProfileId::ProfileC,
    target_width: 1920,
    target_height: 1080,
    interlaced: false,
    sar: None,
    colorspace: "bt709",
    color_trc: "bt709",
    color_primaries: "bt709",
    profile_h264: "high",
    level_h264: "4.2",
};

/// How the picture gets from the source's scan and rate to the profile's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPath {
    /// A / C: [bwdif to frames] -> fps=25 -> scale. 25p out.
    Progressive,
    /// B from 1080-line interlaced 25-frame material that needs no vertical
    /// resize: fields are kept exactly as shot (BFF is re-ordered to TFF).
    FieldPassthrough,
    /// B from anything with ~50+ motion samples a second that cannot pass
    /// through: [bwdif one frame per field] -> fps=50 -> scale progressive
    /// 4:2:2 -> `interlace` TFF -> field-aware 4:2:0.
    FieldRateInterlace,
    /// B handed a <= 30 fps progressive source (routing never does this):
    /// frames flagged TFF, i.e. progressive segmented frame.
    SegmentedFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tonemap {
    NotNeeded,
    Applied,
    /// HDR source, but this ffmpeg has no zscale/tonemap: encoded with a
    /// BT.2020 -> BT.709 matrix only, and QC says so.
    Unavailable,
}

/// Where the active picture lands in the 1920x1080 raster.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub crop: Option<&'static str>,
    pub active_height: i64,
    /// Display aspect ratio of the active picture, snapped to 4:3 / 16:9
    /// when within 3%.
    pub dar: f64,
    pub width: i64,
    pub height: i64,
    pub pad_x: i64,
    pub pad_y: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoPlan {
    pub path: VideoPath,
    pub geometry: Geometry,
    pub tonemap: Tonemap,
    pub filter: String,
}

/// `a:b` or `a/b` with both parts positive.
pub fn parse_ratio(s: &str) -> Option<(i64, i64)> {
    let s = s.trim();
    let (n, d) = s.split_once(':').or_else(|| s.split_once('/'))?;
    let n = n.trim().parse::<i64>().ok()?;
    let d = d.trim().parse::<i64>().ok()?;
    (n > 0 && d > 0).then_some((n, d))
}

fn snap_dar(dar: f64) -> f64 {
    for standard in [4.0 / 3.0, 16.0 / 9.0] {
        if (dar / standard - 1.0).abs() < 0.03 {
            return standard;
        }
    }
    dar
}

fn even(x: f64) -> i64 {
    (((x / 2.0).round() as i64) * 2).max(2)
}

/// Fit the source's *display* shape into 1920x1080 (slice 5 #1).
///
/// V1 scaled the stored raster with `force_original_aspect_ratio=decrease`,
/// which ignores the sample aspect ratio: 720x576 4:3 came out 1350x1080
/// (5% too narrow), anamorphic 16:9 SD came out 4:3-shaped with pillars, and
/// HDV 1440x1080 lost a quarter of its width. The DAR is computed here from
/// SAR (or the container DAR when SAR is missing) and the picture is sized to
/// it: 4:3 SD -> 1440x1080 pillarbox, anamorphic 16:9 SD and HDV -> the full
/// 1920x1080, scope -> letterbox.
pub fn fit_geometry(width: i64, height: i64, sar: &str, dar: &str) -> Geometry {
    let (width, height) = if width > 0 && height > 0 {
        (width, height)
    } else {
        (TARGET_WIDTH, TARGET_HEIGHT)
    };
    let frame_dar = match (parse_ratio(sar), parse_ratio(dar)) {
        (Some((n, d)), _) => width as f64 * n as f64 / (height as f64 * d as f64),
        (None, Some((n, d))) => n as f64 / d as f64,
        _ => width as f64 / height as f64,
    };
    let (crop, active_height, active_dar) = match height {
        // D10/IMX: 32 lines of VBI above 576 active lines. The DAR the file
        // carries (4:3 or 16:9) describes the picture, not the 608 raster.
        608 => (Some("crop=iw:576:0:32"), 576, frame_dar),
        // 1088 is codec padding under a 1080 picture: the SAR describes the
        // pixels, so the active DAR is recomputed on 1080 lines.
        1088 => {
            let sar_f = frame_dar * height as f64 / width as f64;
            (Some("crop=iw:1080:0:0"), 1080, width as f64 * sar_f / 1080.0)
        }
        _ => (None, height, frame_dar),
    };
    let dar = snap_dar(active_dar);
    let target_dar = TARGET_WIDTH as f64 / TARGET_HEIGHT as f64;
    let (w, h) = if dar >= target_dar - 1e-9 {
        (TARGET_WIDTH, even(TARGET_WIDTH as f64 / dar).min(TARGET_HEIGHT))
    } else {
        (even(TARGET_HEIGHT as f64 * dar).min(TARGET_WIDTH), TARGET_HEIGHT)
    };
    Geometry {
        crop,
        active_height,
        dar,
        width: w,
        height: h,
        pad_x: ((TARGET_WIDTH - w) / 2) & !1,
        pad_y: ((TARGET_HEIGHT - h) / 2) & !1,
    }
}

/// swscale matrix name for the source (slice 5 #3). Untagged SD is BT.601
/// and untagged HD BT.709 -- swscale's own default for an untagged frame is
/// 601 at every size, which shifted every untagged HD file.
pub fn source_matrix(src: &ProbeData) -> &'static str {
    match src.color_space.as_str() {
        "bt709" => "bt709",
        "smpte170m" | "bt470bg" => "bt601",
        "bt2020nc" | "bt2020c" => "bt2020",
        "smpte240m" => "smpte240m",
        "fcc" => "fcc",
        _ if src.is_sd_raster() => "bt601",
        _ => "bt709",
    }
}

/// `tv` or `pc`. Untagged is limited range unless the pixel format is one of
/// the full-range `yuvj*` formats.
pub fn source_range(src: &ProbeData) -> &'static str {
    match src.color_range.as_str() {
        "pc" | "jpeg" | "full" => "pc",
        "tv" | "mpeg" | "limited" => "tv",
        _ if src.pix_fmt.starts_with("yuvj") => "pc",
        _ => "tv",
    }
}

pub fn is_rgb_pix_fmt(pix_fmt: &str) -> bool {
    [
        "rgb", "bgr", "gbr", "argb", "abgr", "0rgb", "0bgr", "x2rgb", "x2bgr", "pal8",
    ]
    .iter()
    .any(|p| pix_fmt.starts_with(p))
}

/// What the scaler is told about its input's colour.
#[derive(Debug, Clone, Copy)]
enum ColorIn {
    Source(&'static str, &'static str),
    Rgb,
    /// Already BT.709 limited (the tone-map chain's output).
    Bt709Tv,
}

impl ColorIn {
    fn args(self) -> String {
        match self {
            ColorIn::Source(m, r) => format!(":in_color_matrix={}:in_range={}", m, r),
            ColorIn::Rgb => String::new(),
            ColorIn::Bt709Tv => ":in_color_matrix=bt709:in_range=tv".to_string(),
        }
    }
}

/// Resize + colour conversion to BT.709 limited, then centred pad.
fn scale_and_pad(g: &Geometry, interl: bool, color: ColorIn, intermediate: Option<&str>) -> String {
    let mut s = format!(
        "scale=w={}:h={}:flags={}{}{}:out_color_matrix=bt709:out_range=tv",
        g.width,
        g.height,
        SCALE_FLAGS,
        if interl { ":interl=1" } else { "" },
        color.args()
    );
    if let Some(fmt) = intermediate {
        s.push_str(",format=");
        s.push_str(fmt);
    }
    s.push_str(&format!(
        ",pad={}:{}:{}:{},setsar=1",
        TARGET_WIDTH, TARGET_HEIGHT, g.pad_x, g.pad_y
    ));
    s
}

/// HDR (PQ / HLG) -> SDR BT.709 limited (slice 5 #5), yuv444p10 out so the
/// following resize still has full chroma.
fn tonemap_chain(src: &ProbeData) -> String {
    let min = if src.color_space.starts_with("bt2020") {
        src.color_space.as_str()
    } else {
        "bt2020nc"
    };
    let pin = if src.color_primaries.is_empty() {
        "bt2020"
    } else {
        src.color_primaries.as_str()
    };
    let rin = if source_range(src) == "pc" {
        "full"
    } else {
        "limited"
    };
    format!(
        "zscale=tin={}:min={}:pin={}:rin={}:t=linear:npl=100,format=gbrpf32le,\
         zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=limited,\
         format=yuv444p10le",
        src.color_transfer, min, pin, rin
    )
}

fn parity(scan: ScanType) -> &'static str {
    match scan {
        ScanType::Tff => "tff",
        ScanType::Bff => "bff",
        ScanType::Progressive => "auto",
    }
}

/// The `-vf` chain for `profile` from `src`.
pub fn plan_video(profile: &EncodingProfile, src: &ProbeData) -> VideoPlan {
    let scan = src.scan();
    let interlaced_src = scan != ScanType::Progressive;
    let geometry = fit_geometry(
        src.width,
        src.height,
        &src.sample_aspect_ratio,
        &src.display_aspect_ratio,
    );
    let tonemap = if !src.is_hdr() {
        Tonemap::NotNeeded
    } else if src.ffmpeg_caps.map(|c| c.zscale_tonemap).unwrap_or(false) {
        Tonemap::Applied
    } else {
        Tonemap::Unavailable
    };

    let path = if !profile.interlaced {
        VideoPath::Progressive
    } else if src.wants_interlaced_output() {
        let frame_rate = src.frame_rate();
        if interlaced_src
            && (frame_rate - TARGET_FPS_NUM as f64 / TARGET_FPS_DEN as f64).abs() < 0.5
            && geometry.active_height == TARGET_HEIGHT
            && geometry.height == TARGET_HEIGHT
            && tonemap == Tonemap::NotNeeded
        {
            VideoPath::FieldPassthrough
        } else {
            VideoPath::FieldRateInterlace
        }
    } else {
        VideoPath::SegmentedFrame
    };

    let color = if tonemap == Tonemap::Applied {
        ColorIn::Bt709Tv
    } else if is_rgb_pix_fmt(&src.pix_fmt) {
        ColorIn::Rgb
    } else {
        ColorIn::Source(source_matrix(src), source_range(src))
    };

    let mut f: Vec<String> = Vec::new();
    if let Some(c) = geometry.crop {
        f.push(c.to_string());
    }
    let frames_fps = format!("fps={}/{}", TARGET_FPS_NUM, TARGET_FPS_DEN);
    // bwdif, not yadif (slice 5 #6): motion-adaptive with a w3fdif-style
    // spatial interpolator; visibly less line twitter on captions. The
    // parity is the *detected* one, not the flag.
    let deint_frames = format!("bwdif=mode=send_frame:parity={}:deint=all", parity(scan));
    match path {
        VideoPath::Progressive | VideoPath::SegmentedFrame => {
            if interlaced_src {
                f.push(deint_frames);
            }
            f.push(frames_fps);
            if tonemap == Tonemap::Applied {
                f.push(tonemap_chain(src));
            }
            f.push(scale_and_pad(&geometry, false, color, None));
            if path == VideoPath::SegmentedFrame {
                f.push("setfield=tff".to_string());
            }
            f.push("format=yuv420p".to_string());
        }
        VideoPath::FieldPassthrough => {
            f.push(frames_fps);
            // setfield first: the flag may be missing or wrong (idet said
            // otherwise), and fieldorder only acts on frames flagged
            // interlaced.
            f.push(match scan {
                ScanType::Bff => "setfield=bff,fieldorder=tff".to_string(),
                _ => "setfield=tff".to_string(),
            });
            // interl=1: chroma is resampled per field (slice 5 #4).
            f.push(scale_and_pad(&geometry, true, color, None));
            f.push("format=yuv420p".to_string());
        }
        VideoPath::FieldRateInterlace => {
            if interlaced_src {
                f.push(format!(
                    "bwdif=mode=send_field:parity={}:deint=all",
                    parity(scan)
                ));
            }
            f.push(format!("fps={}/1", FIELD_RATE));
            if tonemap == Tonemap::Applied {
                f.push(tonemap_chain(src));
            }
            // Progressive 4:2:2 until the fields are woven, then subsample
            // chroma per field: 4:2:0 taken from progressive frames and woven
            // puts each field's chroma on the wrong lines.
            f.push(scale_and_pad(&geometry, false, color, Some("yuv422p")));
            f.push("interlace=scan=tff:lowpass=complex".to_string());
            f.push(format!(
                "scale=interl=1:flags={}:in_color_matrix=bt709:out_color_matrix=bt709:in_range=tv:out_range=tv",
                SCALE_FLAGS
            ));
            f.push("format=yuv420p".to_string());
        }
    }
    // Tag the frames themselves. The encoder takes colour properties from
    // the frames, and an untagged source's frames stay "unspecified" for
    // transfer and primaries whatever `-color_trc` / `-color_primaries` say:
    // measured, every untagged source came out with only `colorspace` set.
    f.push(OUTPUT_COLOR_PARAMS.to_string());

    VideoPlan {
        path,
        geometry,
        tonemap,
        filter: f.join(","),
    }
}

/// Which source audio feeds the mezzanine's track.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioSourceMap {
    /// No usable audio: stereo silence is synthesised.
    Silence,
    /// One stream, by audio-relative index.
    Stream {
        index: usize,
        channels: i64,
        layout: String,
    },
    /// Two mono streams joined into L/R -- the MXF / XDCAM layout
    /// (slice 5 #2).
    JoinMono { left: usize, right: usize },
}

impl AudioSourceMap {
    fn channels(&self) -> i64 {
        match self {
            AudioSourceMap::Silence | AudioSourceMap::JoinMono { .. } => 2,
            AudioSourceMap::Stream { channels, .. } => *channels,
        }
    }

    fn layout(&self) -> &str {
        match self {
            AudioSourceMap::Stream { layout, .. } => layout,
            _ => "stereo",
        }
    }
}

pub fn select_audio_source(streams: &[AudioStreamInfo], policy: &AudioPolicy) -> AudioSourceMap {
    let valid: Vec<&AudioStreamInfo> = streams
        .iter()
        .filter(|s| s.channels > 0 && s.sample_rate > 0)
        .collect();
    let Some(first) = valid.first() else {
        return AudioSourceMap::Silence;
    };
    if first.channels == 1 && !policy.dual_mono {
        if let Some(second) = valid.get(1).filter(|s| s.channels == 1) {
            return AudioSourceMap::JoinMono {
                left: first.index,
                right: second.index,
            };
        }
    }
    AudioSourceMap::Stream {
        index: first.index,
        channels: first.channels,
        layout: first.channel_layout.clone(),
    }
}

/// Channel order of a named ffmpeg layout, when it has `channels` channels.
fn layout_channels(layout: &str, channels: i64) -> Option<&'static [&'static str]> {
    let names: &'static [&'static str] = match layout {
        "stereo" | "downmix" => &["FL", "FR"],
        "2.1" => &["FL", "FR", "LFE"],
        "3.0" => &["FL", "FR", "FC"],
        "3.0(back)" => &["FL", "FR", "BC"],
        "4.0" => &["FL", "FR", "FC", "BC"],
        "quad" => &["FL", "FR", "BL", "BR"],
        "quad(side)" => &["FL", "FR", "SL", "SR"],
        "3.1" => &["FL", "FR", "FC", "LFE"],
        "5.0" => &["FL", "FR", "FC", "BL", "BR"],
        "5.0(side)" => &["FL", "FR", "FC", "SL", "SR"],
        "4.1" => &["FL", "FR", "FC", "LFE", "BC"],
        "5.1" => &["FL", "FR", "FC", "LFE", "BL", "BR"],
        "5.1(side)" => &["FL", "FR", "FC", "LFE", "SL", "SR"],
        "6.0" => &["FL", "FR", "FC", "BC", "SL", "SR"],
        "6.1" => &["FL", "FR", "FC", "LFE", "BC", "SL", "SR"],
        "7.0" => &["FL", "FR", "FC", "BL", "BR", "SL", "SR"],
        "7.1" => &["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"],
        "7.1(wide)" => &["FL", "FR", "FC", "LFE", "BL", "BR", "FLC", "FRC"],
        "7.1(wide-side)" => &["FL", "FR", "FC", "LFE", "FLC", "FRC", "SL", "SR"],
        _ => return None,
    };
    (names.len() as i64 == channels).then_some(names)
}

/// (left, right) contribution of a named channel to a stereo downmix: ITU-R
/// BS.775 -3 dB for centre and surrounds, LFE dropped.
fn stereo_weights(name: &str) -> (f64, f64) {
    const H: f64 = std::f64::consts::FRAC_1_SQRT_2;
    match name {
        "FL" | "DL" => (1.0, 0.0),
        "FR" | "DR" => (0.0, 1.0),
        "FC" => (H, H),
        "BL" | "SL" | "FLC" => (H, 0.0),
        "BR" | "SR" | "FRC" => (0.0, H),
        "BC" => (0.5, 0.5),
        _ => (0.0, 0.0),
    }
}

/// The downmix of a `channels`-channel stream to stereo, or `None` when none
/// is needed.
///
/// Never an error any more (slice 5 #10): 3, 5, 7 and >8 channels used to
/// fail the job, and 8 channels took c0/c1 even when ffprobe said 7.1,
/// dropping the centre -- the dialogue. A known layout gets a matrix
/// downmix normalised so neither side can clip; an unknown one is treated as
/// discrete tracks, with 1/2 as the programme L/R.
pub fn stereo_downmix(channels: i64, layout: &str, preserve_original: bool) -> Option<String> {
    match channels {
        i64::MIN..=0 | 2 => None,
        1 => Some("pan=stereo|c0=c0|c1=c0".to_string()),
        _ if preserve_original => None,
        _ => {
            let Some(names) = layout_channels(layout, channels) else {
                return Some("pan=stereo|c0=c0|c1=c1".to_string());
            };
            let side = |pick: fn((f64, f64)) -> f64| -> String {
                let weights: Vec<(usize, f64)> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (i, pick(stereo_weights(n))))
                    .filter(|(_, w)| *w > 0.0)
                    .collect();
                let sum: f64 = weights.iter().map(|(_, w)| w).sum();
                weights
                    .iter()
                    .map(|(i, w)| format!("{:.4}*c{}", w / sum, i))
                    .collect::<Vec<_>>()
                    .join("+")
            };
            Some(format!(
                "pan=stereo|FL={}|FR={}",
                side(|w| w.0),
                side(|w| w.1)
            ))
        }
    }
}

/// Output channels when the source is not preserved: 1 or 2.
pub fn policy_output_channels(policy: &AudioPolicy) -> i64 {
    match policy.channel_layout.as_deref().map(str::trim) {
        Some("mono") => 1,
        Some("stereo") => 2,
        _ if policy.channels == 1 => 1,
        _ => 2,
    }
}

pub fn output_audio_channels(src: &AudioSourceMap, policy: &AudioPolicy) -> i64 {
    match src {
        AudioSourceMap::Stream { channels, .. } if policy.preserve_original && *channels > 2 => {
            *channels
        }
        _ => policy_output_channels(policy),
    }
}

/// The channel count the mezzanine should have for this source, for QC.
pub fn expected_output_audio_channels(source: &ProbeData, policy: &AudioPolicy) -> i64 {
    output_audio_channels(
        &select_audio_source(&source.audio_stream_list(), policy),
        policy,
    )
}

/// Downmix + optional mono fold, shared by the encode and the analysis pass
/// so the loudness pass measures what the encode will carry.
fn channel_filters(src: &AudioSourceMap, policy: &AudioPolicy) -> Vec<String> {
    let mut f = Vec::new();
    let downmix = stereo_downmix(src.channels(), src.layout(), policy.preserve_original);
    let stereo_after = downmix.is_some() || src.channels() <= 2;
    if let Some(d) = downmix {
        f.push(d);
    }
    if stereo_after && output_audio_channels(src, policy) == 1 {
        f.push("pan=mono|c0=0.5*c0+0.5*c1".to_string());
    }
    f
}

fn audio_graph(src: &AudioSourceMap, filters: Vec<String>) -> Option<String> {
    let (inputs, mut chain) = match src {
        AudioSourceMap::Silence => return None,
        AudioSourceMap::Stream { index, .. } => (format!("[0:a:{}]", index), Vec::new()),
        AudioSourceMap::JoinMono { left, right } => (
            format!("[0:a:{}][0:a:{}]", left, right),
            vec!["join=inputs=2:channel_layout=stereo:map=0.0-FL|1.0-FR".to_string()],
        ),
    };
    chain.extend(filters);
    if chain.is_empty() {
        chain.push("anull".to_string());
    }
    Some(format!("{}{}[aout]", inputs, chain.join(",")))
}

/// `-filter_complex` graph for the loudness analysis pass: same source
/// selection and downmix as the encode, then `loudnorm`. `None` = no audio.
pub fn audio_analysis_graph(
    src: &AudioSourceMap,
    policy: &AudioPolicy,
    loudnorm: &str,
) -> Option<String> {
    let mut filters = channel_filters(src, policy);
    filters.push(loudnorm.to_string());
    audio_graph(src, filters)
}

/// The encode's loudnorm, or `None` when the policy leaves the level alone
/// or the programme is silent.
fn loudnorm_filter(
    policy: &AudioPolicy,
    measured: Option<&crate::probe::MeasuredLoudness>,
) -> Option<String> {
    if !policy.mode.normalizes() {
        return None;
    }
    match measured {
        Some(ml) if ml.is_silent => None,
        Some(ml) => Some(format!(
            "loudnorm=I={:.2}:TP={:.2}:LRA={:.2}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear={}:print_format=summary",
            ml.target_i, ml.target_tp, ml.target_lra, ml.input_i, ml.input_tp, ml.input_lra,
            ml.input_thresh, ml.target_offset, ml.is_linear
        )),
        None => {
            let (i, tp, lra) = resolve_loudness_targets(policy);
            Some(format!(
                "loudnorm=I={:.2}:TP={:.2}:LRA={:.2}:print_format=summary",
                i, tp, lra
            ))
        }
    }
}

/// `-map` / `-filter_complex` / `-c:a` arguments for the audio track.
fn audio_args(
    config: &AppConfig,
    source: &ProbeData,
    policy: &AudioPolicy,
    measured: Option<&crate::probe::MeasuredLoudness>,
) -> Vec<String> {
    let legacy = policy.mode == AudioMode::LegacyV1Encode;
    let sample_rate: i64 = if legacy {
        48000
    } else {
        policy.sample_rate_hz as i64
    };
    let src = select_audio_source(&source.audio_stream_list(), policy);
    let out_channels = output_audio_channels(&src, policy);
    let soxr = source.ffmpeg_caps.map(|c| c.soxr).unwrap_or(false);
    // soxr where the build has it (the gyan.dev essentials build does not);
    // swr otherwise, which is what `-ar` always used.
    let resample = format!(
        "aresample={}osr={}",
        if soxr { "resampler=soxr:" } else { "" },
        sample_rate
    );

    let mut args = Vec::new();
    match audio_graph(&src, Vec::new()) {
        None => {
            args.extend(["-map".to_string(), "0:v:0".to_string()]);
            args.extend(["-map".to_string(), "1:a:0".to_string(), "-shortest".to_string()]);
        }
        Some(_) => {
            let mut filters = Vec::new();
            // `-async 1` is gone (slice 5 #11): it is a deprecated alias that
            // only ever acted on the AAC branch. Drift/gap compensation now
            // happens on every path, PCM included, and the first sample is
            // pinned to t=0 so the track starts with the picture.
            filters.push(format!(
                "{}:async=1:min_hard_comp=0.100000:first_pts=0",
                resample
            ));
            filters.extend(channel_filters(&src, policy));
            if let Some(ln) = loudnorm_filter(policy, measured) {
                filters.push(ln);
                // loudnorm's dynamic mode emits 192 kHz.
                filters.push(resample.clone());
            }
            let layout = match out_channels {
                1 => Some("mono".to_string()),
                2 => Some("stereo".to_string()),
                n => policy
                    .channel_layout
                    .clone()
                    .filter(|l| layout_channels(l, n).is_some()),
            };
            filters.push(match layout {
                Some(l) => format!("aformat=sample_rates={}:channel_layouts={}", sample_rate, l),
                None => format!("aformat=sample_rates={}", sample_rate),
            });
            let graph = audio_graph(&src, filters).unwrap_or_default();
            args.extend(["-filter_complex".to_string(), graph]);
            args.extend(["-map".to_string(), "0:v:0".to_string()]);
            args.extend(["-map".to_string(), "[aout]".to_string()]);
        }
    }

    let (codec, bitrate) = if legacy {
        (&config.encoding.audio_codec, &config.encoding.audio_bitrate)
    } else {
        (&policy.codec, &policy.bitrate)
    };
    args.extend(["-c:a".to_string(), codec.clone()]);
    if codec != "pcm_s16le" {
        args.extend(["-b:a".to_string(), bitrate.clone()]);
    }
    args.extend([
        "-ar".to_string(),
        sample_rate.to_string(),
        "-ac".to_string(),
        out_channels.to_string(),
    ]);
    args
}

/// MP4 video track timescale: a multiple of the frame-rate numerator, so
/// every frame duration is an exact integer number of ticks (slice 5 #13).
/// `fps_den * 1000` was right for 25/1 by accident and wrong for 30000/1001
/// (33.4 ticks per frame at 1001000). 25/1 still gets 1000.
pub fn video_track_timescale(fps_num: i64, fps_den: i64) -> i64 {
    if fps_num <= 0 || fps_den <= 0 {
        return 1000;
    }
    let k = (1000 + fps_num - 1) / fps_num;
    fps_num * k.max(1)
}

impl EncodingProfile {
    pub fn by_id(id: ProfileId) -> &'static Self {
        match id {
            ProfileId::ProfileA => &PROFILE_A,
            ProfileId::ProfileB => &PROFILE_B,
            ProfileId::ProfileC => &PROFILE_C,
        }
    }

    pub fn config_for(&self, config: &AppConfig) -> ProfileConfig {
        match self.id {
            ProfileId::ProfileA => config.profile_a.clone(),
            ProfileId::ProfileB => config.profile_b.clone(),
            ProfileId::ProfileC => config.profile_c.clone(),
        }
    }

    /// Arguments for a clean 1920x1080 BT.709 stereo source at the given
    /// rate. For tests and diagnostics; the pipeline always passes the real
    /// source probe to [`Self::build_ffmpeg_args_with_audio`].
    pub fn build_ffmpeg_args(
        &self,
        config: &AppConfig,
        input_path: &str,
        output_path: &str,
        source_fps_num: i64,
        source_fps_den: i64,
    ) -> Vec<String> {
        let source = ProbeData {
            duration_secs: 10.0,
            width: TARGET_WIDTH,
            height: TARGET_HEIGHT,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            audio_channel_layout: "stereo".into(),
            fps_num: source_fps_num,
            fps_den: source_fps_den,
            field_order: "progressive".into(),
            sample_aspect_ratio: "1:1".into(),
            display_aspect_ratio: "16:9".into(),
            pix_fmt: "yuv420p".into(),
            color_space: "bt709".into(),
            color_transfer: "bt709".into(),
            color_primaries: "bt709".into(),
            color_range: "tv".into(),
            input_path: input_path.to_string(),
            ..Default::default()
        };
        let policy = config.effective_audio_policy();
        self.build_ffmpeg_args_with_audio(config, input_path, output_path, &source, &policy, None)
            .unwrap_or_else(|_| Vec::new())
    }

    pub fn build_ffmpeg_args_with_audio(
        &self,
        config: &AppConfig,
        input_path: &str,
        output_path: &str,
        source: &ProbeData,
        audio_policy: &AudioPolicy,
        measured_loudness: Option<&crate::probe::MeasuredLoudness>,
    ) -> Result<Vec<String>, String> {
        let profile_cfg = self.config_for(config);

        let fps_num = TARGET_FPS_NUM;
        let fps_den = TARGET_FPS_DEN;
        let gop_frames = compute_gop_size(fps_num, fps_den);

        let per_encode_threads = config
            .encoding
            .effective_threads_per_encode(config.ingestion.max_concurrency);

        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "info".to_string(),
            // `-nostats -progress pipe:1`, not `-stats` (T2-11). The human
            // status line goes to stderr terminated by a carriage return, not a
            // newline, so a line-oriented reader saw the whole encode as one
            // enormous line and the UI progress bar did not move until it
            // finished (F-21). `-progress` writes newline-terminated key=value
            // blocks to stdout, which is designed to be parsed.
            "-nostats".to_string(),
            "-progress".to_string(),
            "pipe:1".to_string(),
            // Filter threads default to every logical core *per ffmpeg*; with
            // two concurrent encodes that is 2x oversubscription on top of
            // x264's own threads (slice 5 #13). Bounded to this encode's share.
            "-filter_threads".to_string(),
            per_encode_threads.to_string(),
            "-filter_complex_threads".to_string(),
            "1".to_string(),
            "-analyzeduration".to_string(),
            config.encoding.analyzeduration.clone(),
            "-probesize".to_string(),
            config.encoding.probesize.clone(),
            // discardcorrupt: a damaged packet from a flaky capture is dropped
            // (and the frame duplicated by fps=) instead of decoded as garbage.
            "-fflags".to_string(),
            "+genpts+discardcorrupt".to_string(),
            "-i".to_string(),
            input_path.to_string(),
        ];

        let audio_src = select_audio_source(&source.audio_stream_list(), audio_policy);
        if audio_src == AudioSourceMap::Silence {
            let layout = if policy_output_channels(audio_policy) == 1 {
                "mono"
            } else {
                "stereo"
            };
            let rate = if audio_policy.mode == AudioMode::LegacyV1Encode {
                48000
            } else {
                audio_policy.sample_rate_hz
            };
            args.extend_from_slice(&[
                "-f".to_string(),
                "lavfi".to_string(),
                "-i".to_string(),
                format!("anullsrc=channel_layout={}:sample_rate={}", layout, rate),
            ]);
        }

        args.extend_from_slice(&[
            "-map_metadata".to_string(),
            "-1".to_string(),
            "-map_chapters".to_string(),
            "-1".to_string(),
        ]);

        let video = plan_video(self, source);
        args.extend_from_slice(&["-vf".to_string(), video.filter]);

        args.extend_from_slice(&[
            "-fps_mode".to_string(),
            "cfr".to_string(),
            "-video_track_timescale".to_string(),
            video_track_timescale(fps_num, fps_den).to_string(),
        ]);

        args.extend_from_slice(&[
            "-f".to_string(),
            "mp4".to_string(),
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            config.encoding.preset.clone(),
            "-crf".to_string(),
            profile_cfg.crf.to_string(),
            "-maxrate".to_string(),
            profile_cfg.maxrate.clone(),
            "-bufsize".to_string(),
            profile_cfg.bufsize.clone(),
            "-profile:v".to_string(),
            self.profile_h264.to_string(),
            "-level".to_string(),
            self.level_h264.to_string(),
            "-pix_fmt".to_string(),
            "yuv420p".to_string(),
            "-r".to_string(),
            format!("{}/{}", fps_num, fps_den),
        ]);

        args.extend_from_slice(&[
            "-colorspace".to_string(),
            self.colorspace.to_string(),
            "-color_trc".to_string(),
            self.color_trc.to_string(),
            "-color_primaries".to_string(),
            self.color_primaries.to_string(),
            "-color_range".to_string(),
            "tv".to_string(),
        ]);

        args.extend_from_slice(&[
            "-g".to_string(),
            gop_frames.to_string(),
            "-keyint_min".to_string(),
            gop_frames.to_string(),
            "-sc_threshold".to_string(),
            "0".to_string(),
        ]);

        let x264_params = if self.interlaced {
            format!(
                "open-gop=0:keyint={}:min-keyint={}:scenecut=0:interlaced=1:tff=1:pic-struct=1",
                gop_frames, gop_frames
            )
        } else {
            format!(
                "open-gop=0:keyint={}:min-keyint={}:scenecut=0",
                gop_frames, gop_frames
            )
        };
        args.extend_from_slice(&["-x264-params".to_string(), x264_params]);

        if self.interlaced {
            args.extend_from_slice(&[
                "-flags".to_string(),
                "+ilme+ildct".to_string(),
                "-field_order".to_string(),
                "tt".to_string(),
            ]);
        }

        if !config.encoding.tune.is_empty() && config.encoding.tune != "none" {
            args.extend_from_slice(&["-tune".to_string(), config.encoding.tune.clone()]);
        }

        args.extend_from_slice(&["-movflags".to_string(), "+faststart".to_string()]);

        args.extend_from_slice(&["-threads".to_string(), per_encode_threads.to_string()]);

        args.extend(audio_args(config, source, audio_policy, measured_loudness));

        args.extend_from_slice(&["-max_muxing_queue_size".to_string(), "4096".to_string()]);

        args.push(output_path.to_string());
        Ok(args)
    }
}

/// (integrated, true peak, LRA) the policy asks for. `passthrough_validate`
/// and `analyze_only` measure against the EBU targets unless overridden.
pub fn resolve_loudness_targets(policy: &AudioPolicy) -> (f64, f64, f64) {
    match policy.mode {
        AudioMode::AtscA85 => (
            policy.target_lufs.unwrap_or(-24.0),
            policy.true_peak_dbtp.unwrap_or(-2.0),
            policy.lra_target.unwrap_or(11.0),
        ),
        _ => (
            policy.target_lufs.unwrap_or(-23.0),
            policy.true_peak_dbtp.unwrap_or(-1.0),
            policy.lra_target.unwrap_or(7.0),
        ),
    }
}

fn compute_gop_size(fps_num: i64, fps_den: i64) -> i64 {
    let fps = if fps_den > 0 {
        fps_num as f64 / fps_den as f64
    } else {
        25.0
    };
    let gop = (fps * 2.0).round() as i64;
    if gop > 0 {
        gop
    } else {
        50
    }
}

static VALID_COLORSPACE: &[&str] = &["undef", "bt709", "smpte170m", "smpte240m"];

static VALID_COLOR_TRC: &[&str] = &[
    "undef",
    "bt709",
    "smpte170m",
    "smpte240m",
    "bt470bg",
    "linear",
    "smpte2084",
    "bt2020-10",
    "bt2020-12",
    "iec61966-2-1",
    "arib-std-b67",
];

static VALID_COLOR_PRIMARIES: &[&str] = &[
    "undef",
    "bt709",
    "smpte170m",
    "smpte240m",
    "bt470bg",
    "film",
    "bt2020",
    "smpte431",
    "smpte432",
    "jedec-p22",
];

pub fn validate_color_constants() -> Result<(), String> {
    for id in [
        ProfileId::ProfileA,
        ProfileId::ProfileB,
        ProfileId::ProfileC,
    ] {
        let p = EncodingProfile::by_id(id);
        if !VALID_COLORSPACE.contains(&p.colorspace) {
            return Err(format!("{}: invalid colorspace '{}'", id, p.colorspace));
        }
        if !VALID_COLOR_TRC.contains(&p.color_trc) {
            return Err(format!("{}: invalid color_trc '{}'", id, p.color_trc));
        }
        if !VALID_COLOR_PRIMARIES.contains(&p.color_primaries) {
            return Err(format!(
                "{}: invalid color_primaries '{}'",
                id, p.color_primaries
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{FfmpegCaps, IdetCounts};

    fn ebu() -> AudioPolicy {
        AudioPolicy {
            mode: AudioMode::EbuR128,
            ..Default::default()
        }
    }

    fn source(width: i64, height: i64, fps: (i64, i64), field_order: &str) -> ProbeData {
        ProbeData {
            duration_secs: 10.0,
            width,
            height,
            fps_num: fps.0,
            fps_den: fps.1,
            field_order: field_order.into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            audio_channel_layout: "stereo".into(),
            pix_fmt: "yuv420p".into(),
            input_path: "in.mov".into(),
            ..Default::default()
        }
    }

    fn args_for(p: ProfileId, src: &ProbeData, policy: &AudioPolicy) -> Vec<String> {
        EncodingProfile::by_id(p)
            .build_ffmpeg_args_with_audio(&AppConfig::default(), "in.mov", "out.mp4", src, policy, None)
            .unwrap()
    }

    fn value_of<'a>(args: &'a [String], key: &str) -> &'a str {
        let i = args.iter().position(|a| a == key).unwrap_or_else(|| panic!("{key} missing: {args:?}"));
        &args[i + 1]
    }

    fn with_streams(mut p: ProbeData, streams: &[(i64, &str)]) -> ProbeData {
        p.audio_streams = streams
            .iter()
            .enumerate()
            .map(|(index, (channels, layout))| AudioStreamInfo {
                index,
                codec: "pcm_s24le".into(),
                channels: *channels,
                channel_layout: layout.to_string(),
                sample_rate: 48000,
            })
            .collect();
        p
    }

    #[test]
    fn test_profile_a_progressive() {
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        assert!(!p.interlaced);
        assert_eq!(p.colorspace, "bt709");
    }

    #[test]
    fn test_profile_b_interlaced() {
        let p = EncodingProfile::by_id(ProfileId::ProfileB);
        assert!(p.interlaced);
        assert_eq!(p.colorspace, "bt709");
    }

    #[test]
    fn every_profile_is_tagged_bt709_including_c() {
        for id in [ProfileId::ProfileA, ProfileId::ProfileB, ProfileId::ProfileC] {
            let p = EncodingProfile::by_id(id);
            assert_eq!(
                (p.colorspace, p.color_trc, p.color_primaries),
                ("bt709", "bt709", "bt709"),
                "{id}"
            );
        }
    }

    #[test]
    fn test_gop_size_25fps() {
        assert_eq!(compute_gop_size(25, 1), 50);
    }

    #[test]
    fn test_gop_size_2997fps() {
        assert_eq!(compute_gop_size(30000, 1001), 60);
    }

    #[test]
    fn test_validate_color_constants_ok() {
        assert!(validate_color_constants().is_ok());
    }

    #[test]
    fn the_track_timescale_divides_every_frame_exactly() {
        assert_eq!(video_track_timescale(25, 1), 1000, "unchanged for 25/1");
        assert_eq!(video_track_timescale(50, 1), 1000);
        assert_eq!(video_track_timescale(30000, 1001), 30000);
        assert_eq!(video_track_timescale(24000, 1001), 24000);
        for (n, d) in [(25, 1), (30000, 1001), (24000, 1001), (60000, 1001), (24, 1)] {
            let ts = video_track_timescale(n, d);
            assert_eq!((ts * d) % n, 0, "{n}/{d}: {ts}");
        }
    }

    #[test]
    fn test_build_args_uses_configured_crf() {
        let mut config = AppConfig::default();
        config.profile_a.crf = 28;
        config.encoding.preset = "slow".to_string();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        assert_eq!(value_of(&args, "-crf"), "28");
        assert_eq!(value_of(&args, "-preset"), "slow");
    }

    #[test]
    fn test_build_args_normalizes_to_25fps() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 30000, 1001);
        assert_eq!(value_of(&args, "-r"), "25/1");
        assert_eq!(value_of(&args, "-g"), "50");
        assert!(value_of(&args, "-vf").starts_with("fps=25/1,"));
    }

    #[test]
    fn profile_b_reports_25_frames_whatever_the_source_rate() {
        // B's contract rational is 25/1 (interlaced frames), as it always was.
        let src = source(1920, 1080, (50, 1), "progressive");
        let args = args_for(ProfileId::ProfileB, &src, &ebu());
        assert_eq!(value_of(&args, "-r"), "25/1");
        assert_eq!(value_of(&args, "-g"), "50");
        assert_eq!(value_of(&args, "-flags"), "+ilme+ildct");
        assert_eq!(value_of(&args, "-field_order"), "tt");
        assert!(value_of(&args, "-x264-params").contains("interlaced=1:tff=1"));
        assert!(
            !args.contains(&"-top".to_string()),
            "Obsolete -top option must never be passed to ffmpeg encoder"
        );
    }

    #[test]
    fn test_build_args_no_threads_does_not_default_to_all_cores() {
        let mut config = AppConfig::default();
        config.encoding.cpu_cores = 4;
        config.ingestion.max_concurrency = 2;
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        assert_eq!(value_of(&args, "-threads"), "2");
        assert_eq!(value_of(&args, "-filter_threads"), "2", "filters bounded too");
    }

    #[test]
    fn test_build_args_explicit_threads_override() {
        let mut config = AppConfig::default();
        config.encoding.ffmpeg_threads = 6;
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        assert_eq!(value_of(&args, "-threads"), "6");
    }

    #[test]
    fn input_hygiene_flags_are_present() {
        let args = EncodingProfile::by_id(ProfileId::ProfileA).build_ffmpeg_args(
            &AppConfig::default(),
            "in.mov",
            "out.mp4",
            25,
            1,
        );
        assert_eq!(value_of(&args, "-fflags"), "+genpts+discardcorrupt");
        let fflags = args.iter().position(|a| a == "-fflags").unwrap();
        let input = args.iter().position(|a| a == "in.mov").unwrap();
        assert!(fflags < input, "-fflags is an input option");
    }

    // ---- geometry (slice 5 #1) ----

    #[test]
    fn dar_geometry_fits_the_display_shape_not_the_raster() {
        // 720x576 4:3 (SAR 16:15) -> 1440x1080 pillarbox.
        let g = fit_geometry(720, 576, "16:15", "4:3");
        assert_eq!((g.width, g.height, g.pad_x, g.pad_y), (1440, 1080, 240, 0));
        // H.264 SD 4:3 carries SAR 12:11 (DAR 15:11): snapped to 4:3.
        assert_eq!(fit_geometry(720, 576, "12:11", "").width, 1440);
        // Anamorphic 16:9 SD fills the frame (user decision: full-width 16:9).
        let g = fit_geometry(720, 576, "64:45", "16:9");
        assert_eq!((g.width, g.height), (1920, 1080));
        // HDV 1440x1080 SAR 4:3 -> 1920x1080.
        let g = fit_geometry(1440, 1080, "4:3", "16:9");
        assert_eq!((g.width, g.height), (1920, 1080));
        // 1080 square pixels -> untouched.
        assert_eq!(fit_geometry(1920, 1080, "1:1", "16:9").width, 1920);
        // Scope 2.39:1 -> letterbox with even dimensions and offsets.
        let g = fit_geometry(1920, 804, "1:1", "");
        assert_eq!((g.width, g.height), (1920, 804));
        assert_eq!(g.pad_y, 138);
        // No SAR, container DAR only.
        assert_eq!(fit_geometry(720, 576, "", "4:3").width, 1440);
        // Nothing at all: the raster shape.
        assert_eq!(fit_geometry(1280, 720, "", "").width, 1920);
    }

    #[test]
    fn imx_608_and_1088_are_cropped_to_their_active_picture() {
        // Quantel_widescreen_test.mxf: 720x608 SAR 608:405 (16:9 of the raster).
        let g = fit_geometry(720, 608, "608:405", "16:9");
        assert_eq!(g.crop, Some("crop=iw:576:0:32"));
        assert_eq!((g.width, g.height), (1920, 1080));
        // D10 4:3: SAR 152:135.
        assert_eq!(fit_geometry(720, 608, "152:135", "4:3").width, 1440);
        // 1920x1088 square pixels: the padding goes, the picture is 16:9.
        let g = fit_geometry(1920, 1088, "1:1", "");
        assert_eq!(g.crop, Some("crop=iw:1080:0:0"));
        assert_eq!((g.width, g.height), (1920, 1080));
    }

    #[test]
    fn dimensions_are_always_even() {
        for (w, h, sar) in [(720, 576, "59:54"), (704, 480, "10:11"), (1266, 720, ""), (853, 480, "1:1")] {
            let g = fit_geometry(w, h, sar, "");
            assert_eq!(g.width % 2, 0, "{w}x{h}");
            assert_eq!(g.height % 2, 0, "{w}x{h}");
            assert_eq!(g.pad_x % 2, 0);
            assert!(g.width <= 1920 && g.height <= 1080);
        }
    }

    // ---- colour (slice 5 #3, #5) ----

    #[test]
    fn colour_matrix_follows_the_source_tag_then_the_raster() {
        let mut sd = source(720, 576, (25, 1), "progressive");
        assert_eq!(source_matrix(&sd), "bt601", "untagged SD is 601");
        sd.color_space = "bt709".into();
        assert_eq!(source_matrix(&sd), "bt709", "a tag wins over the raster");
        let mut hd = source(1920, 1080, (25, 1), "progressive");
        assert_eq!(source_matrix(&hd), "bt709", "untagged HD is 709");
        hd.color_space = "smpte170m".into();
        assert_eq!(source_matrix(&hd), "bt601");
        hd.color_space = "bt470bg".into();
        assert_eq!(source_matrix(&hd), "bt601");

        let mut full = source(1920, 1080, (25, 1), "progressive");
        assert_eq!(source_range(&full), "tv");
        full.pix_fmt = "yuvj420p".into();
        assert_eq!(source_range(&full), "pc");
        full.color_range = "tv".into();
        assert_eq!(source_range(&full), "tv");
    }

    #[test]
    fn the_scaler_converts_to_bt709_limited_from_the_source_matrix() {
        let sd = ProbeData {
            sample_aspect_ratio: "16:15".into(),
            ..source(720, 576, (25, 1), "progressive")
        };
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileC), &sd).filter;
        assert_eq!(
            vf,
            "fps=25/1,scale=w=1440:h=1080:flags=lanczos+accurate_rnd+full_chroma_int\
             :in_color_matrix=bt601:in_range=tv:out_color_matrix=bt709:out_range=tv,\
             pad=1920:1080:240:0,setsar=1,format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709"
        );

        let mut rgb = source(1920, 1080, (25, 1), "progressive");
        rgb.pix_fmt = "rgb24".into();
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &rgb).filter;
        assert!(!vf.contains("in_color_matrix"), "{vf}");
        assert!(vf.contains("out_color_matrix=bt709:out_range=tv"));
    }

    #[test]
    fn hdr_is_tone_mapped_when_zscale_exists_and_flagged_when_not() {
        let mut hdr = source(3840, 2160, (25, 1), "progressive");
        hdr.color_transfer = "smpte2084".into();
        hdr.color_space = "bt2020nc".into();
        hdr.color_primaries = "bt2020".into();
        hdr.ffmpeg_caps = Some(FfmpegCaps { zscale_tonemap: true, soxr: false });
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &hdr);
        assert_eq!(plan.tonemap, Tonemap::Applied);
        assert!(plan.filter.contains(
            "zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:rin=limited:t=linear:npl=100,format=gbrpf32le,\
             zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=limited"
        ), "{}", plan.filter);
        assert!(plan.filter.contains(":in_color_matrix=bt709:in_range=tv"));

        hdr.ffmpeg_caps = Some(FfmpegCaps::default());
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &hdr);
        assert_eq!(plan.tonemap, Tonemap::Unavailable);
        assert!(!plan.filter.contains("zscale"));
        assert!(plan.filter.contains("in_color_matrix=bt2020"), "matrix-only fallback");
    }

    // ---- scan / rate (slice 5 #4, #6; slice 6c) ----

    #[test]
    fn interlaced_1080i50_passes_through_field_for_field() {
        let src = source(1920, 1080, (25, 1), "tt");
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &src);
        assert_eq!(plan.path, VideoPath::FieldPassthrough);
        assert!(plan.filter.starts_with("fps=25/1,setfield=tff,scale=w=1920:h=1080"));
        assert!(plan.filter.contains(":interl=1"), "interlaced chroma: {}", plan.filter);
        assert!(!plan.filter.contains("bwdif") && !plan.filter.contains("yadif"));

        // BFF is re-ordered to TFF, never deinterlaced.
        let bff = source(1920, 1080, (25, 1), "bb");
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &bff).filter;
        assert!(vf.contains("setfield=bff,fieldorder=tff,scale="), "{vf}");
    }

    #[test]
    fn progressive_50p_becomes_true_interlaced_50_fields() {
        let src = source(1920, 1080, (50, 1), "progressive");
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &src);
        assert_eq!(plan.path, VideoPath::FieldRateInterlace);
        assert!(plan.filter.starts_with("fps=50/1,scale=w=1920:h=1080"), "{}", plan.filter);
        assert!(plan.filter.contains(",format=yuv422p,pad="));
        assert!(plan.filter.ends_with(
            "interlace=scan=tff:lowpass=complex,scale=interl=1:flags=lanczos+accurate_rnd+full_chroma_int\
             :in_color_matrix=bt709:out_color_matrix=bt709:in_range=tv:out_range=tv,format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709"
        ), "{}", plan.filter);

        // 720p50 the same, upscaled progressive before weaving.
        let hd720 = source(1280, 720, (50, 1), "progressive");
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &hd720).filter;
        assert!(vf.starts_with("fps=50/1,scale=w=1920:h=1080"));
    }

    #[test]
    fn sd_576i_is_split_to_fields_scaled_then_reinterlaced() {
        // burosch2.mpg: 720x576 SAR 64:45, BFF.
        let mut src = source(720, 576, (25, 1), "bb");
        src.sample_aspect_ratio = "64:45".into();
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &src).filter;
        assert!(vf.starts_with("bwdif=mode=send_field:parity=bff:deint=all,fps=50/1,scale=w=1920:h=1080"), "{vf}");
        assert!(vf.contains("in_color_matrix=bt601"));
        assert!(vf.contains("interlace=scan=tff:lowpass=complex"));

        // 4:3 SD: pillarboxed at 1440 before weaving.
        let mut src43 = source(720, 576, (25, 1), "tt");
        src43.sample_aspect_ratio = "16:15".into();
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &src43).filter;
        assert!(vf.contains("scale=w=1440:h=1080") && vf.contains("pad=1920:1080:240:0"), "{vf}");
    }

    #[test]
    fn sources_at_5994_are_converted_to_50_fields() {
        let p5994 = source(1920, 1080, (60000, 1001), "progressive");
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &p5994).filter;
        assert!(vf.starts_with("fps=50/1,"), "{vf}");
        let i5994 = source(1920, 1080, (30000, 1001), "tt");
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &i5994);
        assert_eq!(plan.path, VideoPath::FieldRateInterlace, "29.97i cannot pass through");
        assert!(plan.filter.starts_with("bwdif=mode=send_field:parity=tff:deint=all,fps=50/1,"));
    }

    #[test]
    fn progressive_25p_stays_progressive_with_no_deinterlacer() {
        for fps in [(25, 1), (24000, 1001), (30000, 1001)] {
            let src = source(1920, 1080, fps, "progressive");
            let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &src);
            assert_eq!(plan.path, VideoPath::Progressive);
            assert!(plan.filter.starts_with("fps=25/1,scale="), "{}", plan.filter);
            let args = args_for(ProfileId::ProfileA, &src, &ebu());
            assert!(!args.contains(&"-field_order".to_string()));
        }
    }

    #[test]
    fn psf_detected_by_idet_is_not_deinterlaced() {
        let mut psf = source(1920, 1080, (25, 1), "tt");
        psf.idet = Some(IdetCounts { tff: 2, bff: 0, progressive: 590, undetermined: 8 });
        assert_eq!(psf.profile_id(), ProfileId::ProfileA);
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &psf).filter;
        assert!(!vf.contains("bwdif"), "{vf}");
    }

    #[test]
    fn an_interlaced_source_forced_into_a_progressive_profile_uses_bwdif() {
        let src = source(1920, 1080, (25, 1), "tt");
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileA), &src).filter;
        assert!(vf.starts_with("bwdif=mode=send_frame:parity=tff:deint=all,fps=25/1,"), "{vf}");
        assert!(!vf.contains("yadif"));
    }

    #[test]
    fn geometry_cropping_for_608_and_1088() {
        let mut imx = source(720, 608, (25, 1), "progressive");
        imx.sample_aspect_ratio = "152:135".into();
        let vf = plan_video(EncodingProfile::by_id(ProfileId::ProfileC), &imx).filter;
        assert!(vf.starts_with("crop=iw:576:0:32,fps=25/1,scale=w=1440:h=1080"), "{vf}");

        let hd = source(1920, 1088, (25, 1), "tt");
        let plan = plan_video(EncodingProfile::by_id(ProfileId::ProfileB), &hd);
        assert_eq!(plan.path, VideoPath::FieldPassthrough, "1088 crop keeps field parity");
        assert!(plan.filter.starts_with("crop=iw:1080:0:0,fps=25/1,setfield=tff,"));
    }

    // ---- audio (slice 5 #2, #9, #10, #11) ----

    #[test]
    fn two_mono_mxf_tracks_are_joined_into_stereo() {
        let src = with_streams(
            source(1920, 1080, (25, 1), "tt"),
            &[(1, ""), (1, ""), (1, ""), (1, ""), (1, ""), (1, ""), (1, ""), (1, "")],
        );
        let args = args_for(ProfileId::ProfileB, &src, &ebu());
        let graph = value_of(&args, "-filter_complex");
        assert!(
            graph.starts_with("[0:a:0][0:a:1]join=inputs=2:channel_layout=stereo:map=0.0-FL|1.0-FR,"),
            "{graph}"
        );
        assert!(graph.ends_with("[aout]"));
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[aout]"));

        // dual_mono: track 1 on both sides instead.
        let policy = AudioPolicy { dual_mono: true, ..ebu() };
        let graph = value_of(&args_for(ProfileId::ProfileB, &src, &policy), "-filter_complex").to_string();
        assert!(graph.starts_with("[0:a:0]aresample="), "{graph}");
        assert!(graph.contains("pan=stereo|c0=c0|c1=c0"));
    }

    #[test]
    fn the_analysis_pass_hears_the_same_join_and_downmix() {
        let policy = ebu();
        let join = AudioSourceMap::JoinMono { left: 0, right: 1 };
        assert_eq!(
            audio_analysis_graph(&join, &policy, "loudnorm=print_format=json").unwrap(),
            "[0:a:0][0:a:1]join=inputs=2:channel_layout=stereo:map=0.0-FL|1.0-FR,loudnorm=print_format=json[aout]"
        );
        let s51 = AudioSourceMap::Stream { index: 0, channels: 6, layout: "5.1".into() };
        assert_eq!(
            audio_analysis_graph(&s51, &policy, "loudnorm").unwrap(),
            "[0:a:0]pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5,loudnorm[aout]"
        );
        assert_eq!(audio_analysis_graph(&AudioSourceMap::Silence, &policy, "loudnorm"), None);
    }

    #[test]
    fn downmix_selection_by_layout() {
        assert_eq!(stereo_downmix(2, "stereo", false), None);
        assert_eq!(stereo_downmix(1, "mono", false).as_deref(), Some("pan=stereo|c0=c0|c1=c0"));
        assert_eq!(
            stereo_downmix(6, "5.1(side)", false).as_deref(),
            Some("pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5")
        );
        // 7.1 keeps its centre now.
        let d71 = stereo_downmix(8, "7.1", false).unwrap();
        assert_eq!(
            d71,
            "pan=stereo|FL=0.3204*c0+0.2265*c2+0.2265*c4+0.2265*c6|FR=0.3204*c1+0.2265*c2+0.2265*c5+0.2265*c7"
        );
        // Discrete / unknown: tracks 1 and 2 are the programme.
        for n in [3, 4, 5, 6, 7, 8, 12, 16] {
            assert_eq!(stereo_downmix(n, "", false).as_deref(), Some("pan=stereo|c0=c0|c1=c1"), "{n}");
        }
        // Layout name with the wrong channel count is not trusted.
        assert_eq!(stereo_downmix(8, "5.1", false).as_deref(), Some("pan=stereo|c0=c0|c1=c1"));
        assert_eq!(stereo_downmix(5, "5.0", false).unwrap(), "pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c3|FR=0.4142*c1+0.2929*c2+0.2929*c4");
        assert_eq!(stereo_downmix(6, "5.1", true), None, "preserve_original");
    }

    #[test]
    fn odd_channel_counts_no_longer_fail_the_job() {
        for n in [3, 5, 7, 10] {
            let src = with_streams(source(1920, 1080, (25, 1), "progressive"), &[(n, "")]);
            let args = EncodingProfile::by_id(ProfileId::ProfileA)
                .build_ffmpeg_args_with_audio(&AppConfig::default(), "in.mov", "out.mp4", &src, &ebu(), None);
            assert!(args.is_ok(), "{n} channels");
            assert_eq!(value_of(&args.unwrap(), "-ac"), "2");
        }
    }

    #[test]
    fn multichannel_preservation_keeps_the_channel_count() {
        let src = with_streams(source(1920, 1080, (25, 1), "progressive"), &[(6, "5.1")]);
        let policy = AudioPolicy { preserve_original: true, channel_layout: Some("5.1".into()), ..ebu() };
        let args = args_for(ProfileId::ProfileA, &src, &policy);
        let graph = value_of(&args, "-filter_complex");
        assert!(!graph.contains("pan=stereo"), "{graph}");
        assert!(graph.contains("aformat=sample_rates=48000:channel_layouts=5.1"));
        assert_eq!(value_of(&args, "-ac"), "6");
        assert_eq!(expected_output_audio_channels(&src, &policy), 6);
    }

    #[test]
    fn every_audio_path_resamples_with_async_compensation_and_no_async_flag() {
        for policy in [ebu(), AudioPolicy { mode: AudioMode::LegacyV1Encode, ..Default::default() }] {
            for codec in ["aac", "pcm_s16le", "libmp3lame"] {
                let mut config = AppConfig::default();
                config.encoding.audio_codec = codec.into();
                let policy = AudioPolicy { codec: codec.into(), ..policy.clone() };
                let src = source(1920, 1080, (25, 1), "progressive");
                let args = EncodingProfile::by_id(ProfileId::ProfileA)
                    .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", &src, &policy, None)
                    .unwrap();
                assert!(!args.contains(&"-async".to_string()), "{codec}");
                let graph = value_of(&args, "-filter_complex");
                assert!(
                    graph.starts_with("[0:a:0]aresample=osr=48000:async=1:min_hard_comp=0.100000:first_pts=0,"),
                    "{codec}: {graph}"
                );
                assert_eq!(value_of(&args, "-c:a"), codec);
                assert_eq!(args.contains(&"-b:a".to_string()), codec != "pcm_s16le");
            }
        }
    }

    #[test]
    fn soxr_is_used_only_when_the_build_has_it() {
        let mut src = source(1920, 1080, (25, 1), "progressive");
        src.ffmpeg_caps = Some(FfmpegCaps { zscale_tonemap: true, soxr: true });
        let graph = value_of(&args_for(ProfileId::ProfileA, &src, &ebu()), "-filter_complex").to_string();
        assert!(graph.starts_with("[0:a:0]aresample=resampler=soxr:osr=48000:async=1"), "{graph}");
    }

    #[test]
    fn only_normalising_modes_apply_loudnorm() {
        // Slice 5 #9: passthrough_validate and analyze_only reached loudnorm
        // through a `_ =>` fallback.
        let src = source(1920, 1080, (25, 1), "progressive");
        for (mode, expect) in [
            (AudioMode::EbuR128, true),
            (AudioMode::AtscA85, true),
            (AudioMode::PassthroughValidate, false),
            (AudioMode::AnalyzeOnly, false),
            (AudioMode::LegacyV1Encode, false),
        ] {
            let policy = AudioPolicy { mode, ..Default::default() };
            let graph = value_of(&args_for(ProfileId::ProfileA, &src, &policy), "-filter_complex").to_string();
            assert_eq!(graph.contains("loudnorm"), expect, "{mode:?}: {graph}");
        }
    }

    #[test]
    fn the_measured_second_pass_is_linear_and_returns_to_48k() {
        let measured = crate::probe::MeasuredLoudness {
            input_i: -24.0,
            input_tp: -2.0,
            input_lra: 6.0,
            input_thresh: -34.0,
            target_offset: 1.0,
            is_linear: true,
            target_i: -23.0,
            target_tp: -1.0,
            target_lra: 7.0,
            is_silent: false,
            is_short: false,
        };
        let src = source(1920, 1080, (25, 1), "progressive");
        let args = EncodingProfile::by_id(ProfileId::ProfileA)
            .build_ffmpeg_args_with_audio(&AppConfig::default(), "in.mov", "out.mp4", &src, &ebu(), Some(&measured))
            .unwrap();
        let graph = value_of(&args, "-filter_complex");
        assert!(graph.contains("loudnorm=I=-23.00:TP=-1.00:LRA=7.00:measured_I=-24.00:measured_TP=-2.00:measured_LRA=6.00:measured_thresh=-34.00:offset=1.00:linear=true:print_format=summary,aresample=osr=48000,aformat=sample_rates=48000:channel_layouts=stereo[aout]"), "{graph}");

        let silent = crate::probe::MeasuredLoudness { is_silent: true, is_linear: false, ..measured };
        let args = EncodingProfile::by_id(ProfileId::ProfileA)
            .build_ffmpeg_args_with_audio(&AppConfig::default(), "in.mov", "out.mp4", &src, &ebu(), Some(&silent))
            .unwrap();
        assert!(!value_of(&args, "-filter_complex").contains("loudnorm"), "silence gets unity gain");
    }

    #[test]
    fn test_missing_or_corrupt_audio_injects_anullsrc() {
        for (channels, rate) in [(0, 0), (0, 48000), (2, 0), (3, 0), (6, 0)] {
            let mut src = source(1920, 1080, (25, 1), "progressive");
            src.audio_channels = channels;
            src.audio_sample_rate = rate;
            let args = args_for(ProfileId::ProfileA, &src, &ebu());

            let lavfi_pos = args.iter().position(|a| a == "-f").unwrap();
            assert_eq!(args[lavfi_pos + 1], "lavfi");
            assert_eq!(args[lavfi_pos + 3], "anullsrc=channel_layout=stereo:sample_rate=48000");
            assert!(args.contains(&"-shortest".to_string()));
            assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "1:a:0"));
            // Synthetic silence gets no filter at all.
            assert!(!args.contains(&"-filter_complex".to_string()));
            assert_eq!(value_of(&args, "-ac"), "2");
        }
    }

    #[test]
    fn mono_output_is_honoured() {
        let src = source(1920, 1080, (25, 1), "progressive");
        let policy = AudioPolicy { channels: 1, ..ebu() };
        let args = args_for(ProfileId::ProfileA, &src, &policy);
        assert!(value_of(&args, "-filter_complex").contains("pan=mono|c0=0.5*c0+0.5*c1"));
        assert_eq!(value_of(&args, "-ac"), "1");
    }

    #[test]
    fn test_color_range_tv_enforced() {
        let args = args_for(ProfileId::ProfileC, &source(720, 576, (25, 1), "progressive"), &ebu());
        assert_eq!(value_of(&args, "-color_range"), "tv");
        assert_eq!(value_of(&args, "-colorspace"), "bt709");
        assert_eq!(value_of(&args, "-color_primaries"), "bt709");
        assert_eq!(value_of(&args, "-color_trc"), "bt709");
    }

    #[test]
    fn test_broadcast_profiles_registry() {
        let profiles = get_standard_broadcast_profiles();
        assert_eq!(profiles.len(), 3, "only what the builder produces");
        let a = find_broadcast_profile("playoutvue-h264-1080p25").unwrap();
        assert_eq!((a.width, a.height, a.fps_num, a.fps_den), (1920, 1080, 25, 1));
        assert!(!a.interlaced);
        assert_eq!(a.crf, Some(ProfileConfig::profile_a_default().crf as u32));
        let b = find_broadcast_profile("playoutvue-h264-1080i50").unwrap();
        assert!(b.interlaced);
        assert_eq!(b.field_order, "tff");
        assert_eq!((b.fps_num, b.fps_den), (25, 1));
        assert!(find_broadcast_profile("playoutvue-prores-1080i50").is_none());
        assert!(find_broadcast_profile("playoutvue-h264-720p50").is_none());
    }

    #[test]
    fn test_broadcast_profile_alias_lookup() {
        assert_eq!(find_broadcast_profile("ProfileA").unwrap().name, "playoutvue-h264-1080p25");
        assert_eq!(find_broadcast_profile("ProfileB").unwrap().name, "playoutvue-h264-1080i50");
        let c = find_broadcast_profile("ProfileC").unwrap();
        assert_eq!(c.name, "playoutvue-h264-1080p25-sd-pal");
        assert_eq!(c.colorspace, "bt709");
    }
}
