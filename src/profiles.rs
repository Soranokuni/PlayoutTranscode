use crate::config::{AppConfig, ProfileConfig};
use serde::{Deserialize, Serialize};

pub const TARGET_FPS_NUM: i64 = 25;
pub const TARGET_FPS_DEN: i64 = 1;

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
    interlaced: true,
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
    colorspace: "smpte170m",
    color_trc: "smpte170m",
    color_primaries: "smpte170m",
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

    pub fn build_ffmpeg_args(
        &self,
        config: &AppConfig,
        input_path: &str,
        output_path: &str,
        _source_fps_num: i64,
        _source_fps_den: i64,
    ) -> Vec<String> {
        let profile_cfg = self.config_for(config);

        let fps_num = TARGET_FPS_NUM;
        let fps_den = TARGET_FPS_DEN;

        let gop_frames = compute_gop_size(fps_num, fps_den);

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

        let vf = self.build_vf(fps_num, fps_den);
        args.extend_from_slice(&["-vf".to_string(), vf]);

        args.extend_from_slice(&[
            "-fps_mode".to_string(), "cfr".to_string(),
            "-video_track_timescale".to_string(), format!("{}", fps_den * 1000),
        ]);

        args.extend_from_slice(&[
            "-f".to_string(), "mp4".to_string(),
            "-c:v".to_string(), "libx264".to_string(),
            "-preset".to_string(), config.encoding.preset.clone(),
            "-crf".to_string(), profile_cfg.crf.to_string(),
            "-maxrate".to_string(), profile_cfg.maxrate.clone(),
            "-bufsize".to_string(), profile_cfg.bufsize.clone(),
            "-profile:v".to_string(), self.profile_h264.to_string(),
            "-level".to_string(), self.level_h264.to_string(),
            "-pix_fmt".to_string(), "yuv420p".to_string(),
            "-r".to_string(), format!("{}/{}", fps_num, fps_den),
        ]);

        args.extend_from_slice(&[
            "-colorspace".to_string(), self.colorspace.to_string(),
            "-color_trc".to_string(), self.color_trc.to_string(),
            "-color_primaries".to_string(), self.color_primaries.to_string(),
        ]);

        args.extend_from_slice(&[
            "-g".to_string(), gop_frames.to_string(),
            "-keyint_min".to_string(), gop_frames.to_string(),
            "-sc_threshold".to_string(), "0".to_string(),
        ]);

        let x264_params = if self.interlaced {
            format!(
                "open-gop=0:keyint={}:min-keyint={}:scenecut=0:interlaced=1:pic-struct=1",
                gop_frames, gop_frames
            )
        } else {
            format!(
                "open-gop=0:keyint={}:min-keyint={}:scenecut=0",
                gop_frames, gop_frames
            )
        };
        args.extend_from_slice(&[
            "-x264-params".to_string(), x264_params,
        ]);

        if self.interlaced {
            args.extend_from_slice(&[
                "-top".to_string(), "1".to_string(),
                "-field_order".to_string(), "tt".to_string(),
            ]);
        }

        if !config.encoding.tune.is_empty() && config.encoding.tune != "none" {
            args.extend_from_slice(&[
                "-tune".to_string(), config.encoding.tune.clone(),
            ]);
        }

        args.extend_from_slice(&[
            "-movflags".to_string(), "+faststart".to_string(),
        ]);

        // Always honor the CPU budget. `ffmpeg_threads=0` (auto) is resolved to
        // `cpu_cores / max_concurrency` via config::EncodingConfig::effective_threads_per_encode.
        let per_encode_threads = config.encoding.effective_threads_per_encode(
            config.ingestion.max_concurrency,
        );
        args.extend_from_slice(&[
            "-threads".to_string(), per_encode_threads.to_string(),
        ]);

        args.extend_from_slice(&[
            "-map".to_string(), "0:v:0".to_string(),
            "-map".to_string(), "0:a:0?".to_string(),
        ]);

        let audio_codec = &config.encoding.audio_codec;
        args.extend_from_slice(&[
            "-c:a".to_string(), audio_codec.clone(),
        ]);

        if audio_codec == "pcm_s16le" {
            args.extend_from_slice(&[
                "-ar".to_string(), "48000".to_string(),
                "-ac".to_string(), "2".to_string(),
            ]);
        } else if audio_codec == "libmp3lame" {
            args.extend_from_slice(&[
                "-b:a".to_string(), config.encoding.audio_bitrate.clone(),
                "-ar".to_string(), "48000".to_string(),
                "-ac".to_string(), "2".to_string(),
            ]);
        } else {
            args.extend_from_slice(&[
                "-b:a".to_string(), config.encoding.audio_bitrate.clone(),
                "-ar".to_string(), "48000".to_string(),
                "-ac".to_string(), "2".to_string(),
                "-async".to_string(), "1".to_string(),
            ]);
        }

        args.extend_from_slice(&["-max_muxing_queue_size".to_string(), "4096".to_string()]);

        args.push(output_path.to_string());
        args
    }

    fn build_vf(&self, fps_num: i64, fps_den: i64) -> String {
        let w = self.target_width;
        let h = self.target_height;

        format!(
            "fps={n}/{d},scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p",
            n = fps_num, d = fps_den, w = w, h = h
        )
    }
}

fn compute_gop_size(fps_num: i64, fps_den: i64) -> i64 {
    let fps = if fps_den > 0 { fps_num as f64 / fps_den as f64 } else { 25.0 };
    let gop = (fps * 2.0).round() as i64;
    if gop > 0 { gop } else { 50 }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_profile_c_sd_pal_color() {
        let p = EncodingProfile::by_id(ProfileId::ProfileC);
        assert_eq!(p.colorspace, "smpte170m");
        assert_eq!(p.color_primaries, "smpte170m");
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
    fn test_build_args_uses_configured_crf() {
        let mut config = AppConfig::default();
        config.profile_a.crf = 28;
        config.encoding.preset = "slow".to_string();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        let crf_idx = args.iter().position(|a| a == "-crf").unwrap();
        assert_eq!(args[crf_idx + 1], "28");
        let preset_idx = args.iter().position(|a| a == "-preset").unwrap();
        assert_eq!(args[preset_idx + 1], "slow");
    }

    #[test]
    fn test_build_args_normalizes_to_25fps() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 30000, 1001);
        let r_idx = args.iter().position(|a| a == "-r").unwrap();
        assert_eq!(args[r_idx + 1], "25/1");
        let gop_idx = args.iter().position(|a| a == "-g").unwrap();
        assert_eq!(args[gop_idx + 1], "50");
    }

    #[test]
    fn test_build_args_normalizes_50p_to_25fps() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 50, 1);
        let r_idx = args.iter().position(|a| a == "-r").unwrap();
        assert_eq!(args[r_idx + 1], "25/1");
        let gop_idx = args.iter().position(|a| a == "-g").unwrap();
        assert_eq!(args[gop_idx + 1], "50");
    }

    #[test]
    fn test_build_args_interlaced_also_25fps() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileB);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 50, 1);
        let r_idx = args.iter().position(|a| a == "-r").unwrap();
        assert_eq!(args[r_idx + 1], "25/1");
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert!(args[vf_idx + 1].starts_with("fps=25/1"), "interlaced vf must also normalize fps");
    }

    #[test]
    fn test_build_args_no_threads_does_not_default_to_all_cores() {
        // When ffmpeg_threads=0 (auto), the explicit -threads passed must be derived from
        // cpu_cores/max_concurrency, not ffmpeg's own "use everything" default.
        let mut config = AppConfig::default();
        config.encoding.cpu_cores = 4;
        config.ingestion.max_concurrency = 2;
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        let t_idx = args.iter().position(|a| a == "-threads").unwrap();
        assert_eq!(args[t_idx + 1], "2");
    }

    #[test]
    fn test_build_args_explicit_threads_override() {
        let mut config = AppConfig::default();
        config.encoding.ffmpeg_threads = 6;
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);
        let t_idx = args.iter().position(|a| a == "-threads").unwrap();
        assert_eq!(args[t_idx + 1], "6");
    }
}
