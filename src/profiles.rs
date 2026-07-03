use crate::config::{AppConfig, ProfileConfig};
use serde::{Deserialize, Serialize};

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
    interlaced: false,
    sar: None,
    colorspace: "bt709",
    color_trc: "bt709",
    color_primaries: "bt709",
    profile_h264: "high",
    level_h264: "4.2",
};

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

    pub fn build_ffmpeg_args(&self, config: &AppConfig, input_path: &str, output_path: &str) -> Vec<String> {
        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(), "info".to_string(),
            "-stats".to_string(),
            "-analyzeduration".to_string(), config.encoding.analyzeduration.clone(),
            "-probesize".to_string(), config.encoding.probesize.clone(),
            "-fflags".to_string(), "+genpts".to_string(),
            "-i".to_string(), input_path.to_string(),
            "-map_metadata".to_string(), "-1".to_string(),
            "-map_chapters".to_string(), "-1".to_string(),
        ];

        let vf = self.build_vf();
        args.extend_from_slice(&["-vf".to_string(), vf]);

        args.extend_from_slice(&["-vsync".to_string(), "cfr".to_string()]);
        
        // Video Codec & Format
        args.extend_from_slice(&[
            "-f".to_string(), "mp4".to_string(),
            "-c:v".to_string(), "libx264".to_string(),
            "-preset".to_string(), "fast".to_string(),
            "-crf".to_string(), "20".to_string(),
            "-profile:v".to_string(), "high".to_string(),
            "-level".to_string(), "4.2".to_string(), // Correct: no :v suffix for libx264 encoder level option
            "-pix_fmt".to_string(), "yuv420p".to_string(),
            "-r".to_string(), "25".to_string(),
            "-force_key_frames".to_string(), "expr:gte(t,n_forced*1)".to_string(),
            "-video_track_timescale".to_string(), "90000".to_string(),
        ]);

        // Explicitly force standard BT.709 color properties for HD display compatibility
        args.extend_from_slice(&[
            "-colorspace".to_string(), "bt709".to_string(),
            "-color_trc".to_string(), "bt709".to_string(),
            "-color_primaries".to_string(), "bt709".to_string(),
        ]);

        // Closed GOP seeking optimization:
        // -g 25: forces keyframe every 25 frames (exactly 1 second at 25fps)
        // -keyint_min 25: prevents keyframes from being generated more frequently than 1 second
        // -sc_threshold 0: disables scene-change dynamic keyframes to avoid drift
        // -flags +cgop: forces strictly closed GOPs
        args.extend_from_slice(&[
            "-g".to_string(), "25".to_string(),
            "-keyint_min".to_string(), "25".to_string(),
            "-sc_threshold".to_string(), "0".to_string(),
            "-flags".to_string(), "+cgop".to_string(),
        ]);

        // open-gop=0 disables open GOPs in libx264 params; faststart enables quick media loading
        args.extend_from_slice(&[
            "-x264-params".to_string(), "open-gop=0".to_string(),
            "-movflags".to_string(), "+faststart".to_string(),
        ]);

        if config.encoding.ffmpeg_threads > 0 {
            args.extend_from_slice(&[
                "-threads".to_string(), config.encoding.ffmpeg_threads.to_string(),
            ]);
        }

        args.extend_from_slice(&[
            "-map".to_string(), "0:v:0".to_string(),
            "-map".to_string(), "0:a:0?".to_string(),
        ]);

        // Audio conversion specifications (AAC stereo 256k at 48kHz)
        args.extend_from_slice(&[
            "-c:a".to_string(), "aac".to_string(),
            "-b:a".to_string(), "256k".to_string(),
            "-ar".to_string(), "48000".to_string(),
            "-ac".to_string(), "2".to_string(),
            "-async".to_string(), "1".to_string(),
        ]);

        args.extend_from_slice(&["-max_muxing_queue_size".to_string(), "4096".to_string()]);

        args.push(output_path.to_string());
        args
    }

    fn build_vf(&self) -> String {
        // Smart Retro Scaling Filter graph:
        // 1. fps=25: forces frame rate conversion to constant 25fps (required for 1s keyframe sync)
        // 2. scale=1920:1080:force_original_aspect_ratio=decrease: scales to fit boundaries
        // 3. pad=1920:1080:(ow-iw)/2:(oh-ih)/2: centers and pillarboxes/letterboxes retro/odd content to 1920x1080
        // 4. setsar=1: forces standard 1:1 square pixel aspect ratio
        // 5. format=yuv420p: output standard colorspace
        "fps=25,scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p".to_string()
    }
}

static VALID_COLORSPACE: &[&str] = &["undef", "bt709", "smpte170m", "smpte240m"];

static VALID_COLOR_TRC: &[&str] = &[
    "undef", "bt709", "smpte170m", "smpte240m", "bt470bg", "linear",
    "smpte2084", "bt2020-10", "bt2020-12", "iec61966-2-1", "arib-std-b67",
];

static VALID_COLOR_PRIMARIES: &[&str] = &[
    "undef", "bt709", "smpte170m", "smpte240m", "bt470bg", "film",
    "bt2020", "smpte431", "smpte432", "jedec-p22",
];

pub fn validate_color_constants() -> Result<(), String> {
    for id in [ProfileId::ProfileA, ProfileId::ProfileB, ProfileId::ProfileC] {
        let p = EncodingProfile::by_id(id);
        if !VALID_COLORSPACE.contains(&p.colorspace) {
            return Err(format!("{}: invalid colorspace '{}'", id, p.colorspace));
        }
        if !VALID_COLOR_TRC.contains(&p.color_trc) {
            return Err(format!("{}: invalid color_trc '{}'", id, p.color_trc));
        }
        if !VALID_COLOR_PRIMARIES.contains(&p.color_primaries) {
            return Err(format!("{}: invalid color_primaries '{}'", id, p.color_primaries));
        }
    }
    Ok(())
}
