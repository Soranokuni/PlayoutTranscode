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

    #[allow(dead_code)]
    pub fn build_ffmpeg_args(
        &self,
        config: &AppConfig,
        input_path: &str,
        output_path: &str,
        source_fps_num: i64,
        source_fps_den: i64,
    ) -> Vec<String> {
        let policy = config.effective_audio_policy();
        self.build_ffmpeg_args_with_audio(
            config,
            input_path,
            output_path,
            source_fps_num,
            source_fps_den,
            &policy,
            None,
            2,
        )
        .unwrap_or_else(|_| Vec::new())
    }

    pub fn build_ffmpeg_args_with_audio(
        &self,
        config: &AppConfig,
        input_path: &str,
        output_path: &str,
        _source_fps_num: i64,
        _source_fps_den: i64,
        audio_policy: &crate::config::AudioPolicy,
        measured_loudness: Option<&crate::probe::MeasuredLoudness>,
        audio_channels: i64,
    ) -> Result<Vec<String>, String> {
        let profile_cfg = self.config_for(config);

        let fps_num = TARGET_FPS_NUM;
        let fps_den = TARGET_FPS_DEN;

        let gop_frames = compute_gop_size(fps_num, fps_den);

        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "info".to_string(),
            "-stats".to_string(),
            "-analyzeduration".to_string(),
            config.encoding.analyzeduration.clone(),
            "-probesize".to_string(),
            config.encoding.probesize.clone(),
            "-fflags".to_string(),
            "+genpts".to_string(),
            "-i".to_string(),
            input_path.to_string(),
            "-map_metadata".to_string(),
            "-1".to_string(),
            "-map_chapters".to_string(),
            "-1".to_string(),
        ];

        let vf = self.build_vf(fps_num, fps_den);
        args.extend_from_slice(&["-vf".to_string(), vf]);

        args.extend_from_slice(&[
            "-fps_mode".to_string(),
            "cfr".to_string(),
            "-video_track_timescale".to_string(),
            format!("{}", fps_den * 1000),
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
                "open-gop=0:keyint={}:min-keyint={}:scenecut=0:interlaced=1:pic-struct=1",
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
                "-top".to_string(),
                "1".to_string(),
                "-field_order".to_string(),
                "tt".to_string(),
            ]);
        }

        if !config.encoding.tune.is_empty() && config.encoding.tune != "none" {
            args.extend_from_slice(&["-tune".to_string(), config.encoding.tune.clone()]);
        }

        args.extend_from_slice(&["-movflags".to_string(), "+faststart".to_string()]);

        let per_encode_threads = config
            .encoding
            .effective_threads_per_encode(config.ingestion.max_concurrency);
        args.extend_from_slice(&["-threads".to_string(), per_encode_threads.to_string()]);

        args.extend_from_slice(&[
            "-map".to_string(),
            "0:v:0".to_string(),
            "-map".to_string(),
            "0:a:0?".to_string(),
        ]);

        if audio_policy.mode == crate::config::AudioMode::LegacyV1Encode {
            let audio_codec = &config.encoding.audio_codec;
            args.extend_from_slice(&["-c:a".to_string(), audio_codec.clone()]);

            if audio_codec == "pcm_s16le" {
                args.extend_from_slice(&[
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                ]);
            } else if audio_codec == "libmp3lame" {
                args.extend_from_slice(&[
                    "-b:a".to_string(),
                    config.encoding.audio_bitrate.clone(),
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                ]);
            } else {
                args.extend_from_slice(&[
                    "-b:a".to_string(),
                    config.encoding.audio_bitrate.clone(),
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    "2".to_string(),
                    "-async".to_string(),
                    "1".to_string(),
                ]);
            }
        } else {
            let audio_codec = &audio_policy.codec;

            if audio_channels > 0 {
                let downmix_filter =
                    build_downmix_filter(audio_channels, audio_policy.preserve_original)?;

                let af_str = if let Some(ml) = measured_loudness {
                    if ml.is_silent {
                        downmix_filter.trim_end_matches(',').to_string()
                    } else {
                        let is_linear_str = if ml.is_linear { "true" } else { "false" };
                        let loudnorm = format!(
                            "loudnorm=I={:.2}:TP={:.2}:LRA={:.2}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear={}:print_format=summary",
                            ml.target_i, ml.target_tp, ml.target_lra, ml.input_i, ml.input_tp, ml.input_lra, ml.input_thresh, ml.target_offset, is_linear_str
                        );
                        format!("{}{}", downmix_filter, loudnorm)
                    }
                } else {
                    let (target_i, target_tp, target_lra) = resolve_loudness_targets(audio_policy);
                    let loudnorm = format!(
                        "loudnorm=I={:.2}:TP={:.2}:LRA={:.2}:print_format=summary",
                        target_i, target_tp, target_lra
                    );
                    format!("{}{}", downmix_filter, loudnorm)
                };

                if !af_str.is_empty() {
                    args.extend_from_slice(&["-af".to_string(), af_str]);
                }
            }

            args.extend_from_slice(&["-c:a".to_string(), audio_codec.clone()]);

            let out_channels = if audio_policy.preserve_original && audio_channels > 2 {
                audio_channels
            } else {
                2
            };

            if audio_codec == "pcm_s16le" {
                args.extend_from_slice(&[
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    out_channels.to_string(),
                ]);
            } else if audio_codec == "libmp3lame" {
                args.extend_from_slice(&[
                    "-b:a".to_string(),
                    audio_policy.bitrate.clone(),
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    out_channels.to_string(),
                ]);
            } else {
                args.extend_from_slice(&[
                    "-b:a".to_string(),
                    audio_policy.bitrate.clone(),
                    "-ar".to_string(),
                    "48000".to_string(),
                    "-ac".to_string(),
                    out_channels.to_string(),
                    "-async".to_string(),
                    "1".to_string(),
                ]);
            }
        }

        args.extend_from_slice(&["-max_muxing_queue_size".to_string(), "4096".to_string()]);

        args.push(output_path.to_string());
        Ok(args)
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

pub fn resolve_loudness_targets(policy: &crate::config::AudioPolicy) -> (f64, f64, f64) {
    match policy.mode {
        crate::config::AudioMode::EbuR128 => {
            let target_i = policy.target_lufs.unwrap_or(-23.0);
            let target_tp = policy.true_peak_dbtp.unwrap_or(-1.0);
            let target_lra = policy.lra_target.unwrap_or(7.0);
            (target_i, target_tp, target_lra)
        }
        crate::config::AudioMode::AtscA85 => {
            let target_i = policy.target_lufs.unwrap_or(-24.0);
            let target_tp = policy.true_peak_dbtp.unwrap_or(-2.0);
            let target_lra = policy.lra_target.unwrap_or(11.0);
            (target_i, target_tp, target_lra)
        }
        _ => (-23.0, -1.0, 7.0),
    }
}

pub fn build_downmix_filter(channels: i64, preserve_original: bool) -> Result<String, String> {
    match channels {
        0 => Ok(String::new()),
        1 => {
            if preserve_original {
                Ok(String::new())
            } else {
                Ok("pan=stereo|c0=c0|c1=c0,".to_string())
            }
        }
        2 => Ok(String::new()),
        6 => {
            if preserve_original {
                Ok(String::new())
            } else {
                Ok(
                    "pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5,"
                        .to_string(),
                )
            }
        }
        8 => {
            if preserve_original {
                Ok(String::new())
            } else {
                Err("unsupported_audio_channel_layout".to_string())
            }
        }
        _ => Err("unsupported_audio_channel_layout".to_string()),
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
        assert!(
            args[vf_idx + 1].starts_with("fps=25/1"),
            "interlaced vf must also normalize fps"
        );
    }

    #[test]
    fn test_build_args_no_threads_does_not_default_to_all_cores() {
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

    #[test]
    fn test_complete_legacy_v1_argument_identity() {
        let mut config = AppConfig::default();
        config.encoding.cpu_cores = 4;
        config.ingestion.max_concurrency = 2;
        let p = EncodingProfile::by_id(ProfileId::ProfileA);

        let legacy_args = p.build_ffmpeg_args(&config, "in.mov", "out.mp4", 25, 1);

        let policy = config.effective_audio_policy();
        assert_eq!(policy.mode, crate::config::AudioMode::LegacyV1Encode);
        let explicit_args = p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 2)
            .unwrap();

        assert_eq!(
            legacy_args, explicit_args,
            "LegacyV1Encode must be 100% bit-identical to V1 arguments"
        );
        assert!(
            !explicit_args.contains(&"-af".to_string()),
            "Legacy mode must emit no audio filter"
        );

        let expected_tail = vec![
            "-threads".to_string(),
            "2".to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-map".to_string(),
            "0:a:0?".to_string(),
            "-c:a".to_string(),
            "aac".to_string(),
            "-b:a".to_string(),
            "320k".to_string(),
            "-ar".to_string(),
            "48000".to_string(),
            "-ac".to_string(),
            "2".to_string(),
            "-async".to_string(),
            "1".to_string(),
            "-max_muxing_queue_size".to_string(),
            "4096".to_string(),
            "out.mp4".to_string(),
        ];
        let tail_start = explicit_args.len() - expected_tail.len();
        assert_eq!(&explicit_args[tail_start..], &expected_tail[..]);
    }

    #[test]
    fn test_mono_dual_mono_graph() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;

        let args = p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 1)
            .unwrap();
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        let af = &args[af_idx + 1];
        assert!(af.starts_with("pan=stereo|c0=c0|c1=c0,loudnorm="));
    }

    #[test]
    fn test_stereo_graph() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;

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

        let args = p
            .build_ffmpeg_args_with_audio(
                &config,
                "in.mov",
                "out.mp4",
                25,
                1,
                &policy,
                Some(&measured),
                2,
            )
            .unwrap();
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        let af = &args[af_idx + 1];
        assert!(af.starts_with("loudnorm=I=-23.00:TP=-1.00:LRA=7.00:measured_I=-24.00:measured_TP=-2.00:measured_LRA=6.00:measured_thresh=-34.00:offset=1.00:linear=true"));
    }

    #[test]
    fn test_exact_51_downmix_graph() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;
        policy.preserve_original = false;

        let args = p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 6)
            .unwrap();
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        let af = &args[af_idx + 1];
        assert!(af.starts_with("pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5,loudnorm="));
    }

    #[test]
    fn test_51_preservation_mode() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;
        policy.preserve_original = true;

        let args = p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 6)
            .unwrap();
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        let af = &args[af_idx + 1];
        assert!(
            !af.contains("pan=stereo"),
            "Preservation mode must not downmix to stereo"
        );
        assert!(af.starts_with("loudnorm="));
        let ac_idx = args.iter().position(|a| a == "-ac").unwrap();
        assert_eq!(args[ac_idx + 1], "6");
    }

    #[test]
    fn test_unsupported_channel_layout() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;
        policy.preserve_original = false;

        assert!(p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 8)
            .is_err());
        assert!(p
            .build_ffmpeg_args_with_audio(&config, "in.mov", "out.mp4", 25, 1, &policy, None, 4)
            .is_err());
    }

    #[test]
    fn test_silent_audio_omits_loudnorm() {
        let config = AppConfig::default();
        let p = EncodingProfile::by_id(ProfileId::ProfileA);
        let mut policy = crate::config::AudioPolicy::default();
        policy.mode = crate::config::AudioMode::EbuR128;

        let silent_measured = crate::probe::MeasuredLoudness {
            input_i: f64::NEG_INFINITY,
            input_tp: f64::NEG_INFINITY,
            input_lra: 0.0,
            input_thresh: -70.0,
            target_offset: 0.0,
            is_linear: false,
            target_i: -23.0,
            target_tp: -1.0,
            target_lra: 7.0,
            is_silent: true,
            is_short: false,
        };

        let args = p
            .build_ffmpeg_args_with_audio(
                &config,
                "in.mov",
                "out.mp4",
                25,
                1,
                &policy,
                Some(&silent_measured),
                2,
            )
            .unwrap();
        assert!(
            !args.contains(&"-af".to_string()),
            "Silent audio on stereo input must omit loudnorm filter (unity gain)"
        );
    }
}
