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
    profile_h264: "main",
    level_h264: "4.1",
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
    profile_h264: "main",
    level_h264: "4.1",
};

const PROFILE_C: EncodingProfile = EncodingProfile {
    id: ProfileId::ProfileC,
    target_width: 720,
    target_height: 576,
    interlaced: false,
    sar: Some("64:45"),
    colorspace: "bt601",
    color_trc: "bt601",
    color_primaries: "bt601",
    profile_h264: "main",
    level_h264: "3.0",
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
        let pc = self.config_for(config);
        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(), "info".to_string(),
            "-stats".to_string(),
            "-analyzeduration".to_string(), "100M".to_string(),
            "-probesize".to_string(), "100M".to_string(),
            "-i".to_string(), input_path.to_string(),
            "-r".to_string(), "25".to_string(),
        ];

        let vf = self.build_vf();
        args.extend_from_slice(&["-vf".to_string(), vf]);

        args.extend_from_slice(&["-c:v".to_string(), "libx264".to_string()]);
        args.extend_from_slice(&["-preset".to_string(), config.encoding.preset.clone()]);

        let tune = config.encoding.tune.clone();
        if tune != "none" {
            args.extend_from_slice(&["-tune".to_string(), tune]);
        }

        args.extend_from_slice(&["-crf".to_string(), pc.crf.to_string()]);
        args.extend_from_slice(&["-maxrate".to_string(), pc.maxrate.clone()]);
        args.extend_from_slice(&["-bufsize".to_string(), pc.bufsize.clone()]);
        args.extend_from_slice(&["-profile:v".to_string(), self.profile_h264.to_string()]);
        args.extend_from_slice(&["-level:v".to_string(), self.level_h264.to_string()]);
        args.extend_from_slice(&["-pix_fmt".to_string(), "yuv420p".to_string()]);

        args.extend_from_slice(&[
            "-colorspace".to_string(), self.colorspace.to_string(),
            "-color_trc".to_string(), self.color_trc.to_string(),
            "-color_primaries".to_string(), self.color_primaries.to_string(),
        ]);

        if self.interlaced {
            args.extend_from_slice(&[
                "-flags".to_string(), "+ilme+ildct+cgop".to_string(),
                "-top".to_string(), "1".to_string(),
                "-field_order".to_string(), "tt".to_string(),
            ]);
        } else {
            args.extend_from_slice(&["-flags".to_string(), "+cgop".to_string()]);
        }

        args.extend_from_slice(&[
            "-g".to_string(), "50".to_string(),
            "-keyint_min".to_string(), "50".to_string(),
            "-sc_threshold".to_string(), "0".to_string(),
        ]);

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

        args.extend_from_slice(&["-c:a".to_string(), config.encoding.audio_codec.clone()]);
        if config.encoding.audio_codec == "aac" || config.encoding.audio_codec == "libmp3lame" {
            args.extend_from_slice(&["-b:a".to_string(), config.encoding.audio_bitrate.clone()]);
        }
        args.extend_from_slice(&["-ar".to_string(), "48000".to_string(), "-ac".to_string(), "2".to_string()]);

        args.push(output_path.to_string());
        args
    }

    fn build_vf(&self) -> String {
        if let Some(sar) = self.sar {
            format!(
                "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,setsar={},format=yuv420p",
                self.target_width, self.target_height,
                self.target_width, self.target_height,
                sar
            )
        } else {
            format!(
                "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format=yuv420p",
                self.target_width, self.target_height,
                self.target_width, self.target_height,
            )
        }
    }
}
