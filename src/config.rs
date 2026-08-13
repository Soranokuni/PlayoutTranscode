use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn default_config_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_dir.join("config.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    #[serde(default = "default_watch")]
    pub watch_folder: String,
    #[serde(default = "default_target")]
    pub target_folder: String,
}

fn default_watch() -> String {
    String::new()
}
fn default_target() -> String {
    String::new()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
}

fn default_web_port() -> u16 {
    4353
}
fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EncodingConfig {
    #[serde(default = "default_preset")]
    pub preset: String,
    /// Per-encode ffmpeg/x264 threads. 0 = "auto" (computed from `cpu_cores` / `max_concurrency`).
    #[serde(default = "default_threads")]
    pub ffmpeg_threads: usize,
    /// Total CPU core budget shared across all concurrent encodes. 0 = "auto" (half of physical cores).
    #[serde(default = "default_cpu_cores")]
    pub cpu_cores: usize,
    #[serde(default = "default_audio_codec")]
    pub audio_codec: String,
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate: String,
    #[serde(default = "default_tune")]
    pub tune: String,
    #[serde(default = "default_probesize")]
    pub probesize: String,
    #[serde(default = "default_analyzeduration")]
    pub analyzeduration: String,
}

fn default_preset() -> String {
    "medium".into()
}
fn default_threads() -> usize {
    0
}
fn default_cpu_cores() -> usize {
    0
}
fn default_audio_codec() -> String {
    "aac".into()
}
fn default_audio_bitrate() -> String {
    "320k".into()
}
fn default_tune() -> String {
    "film".into()
}
fn default_probesize() -> String {
    "500M".into()
}
fn default_analyzeduration() -> String {
    "500M".into()
}

/// Number of physical/logical cores available on this machine.
pub fn available_logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

impl EncodingConfig {
    /// Returns the per-encode thread count that should be passed to ffmpeg:
    /// - If `ffmpeg_threads > 0`, that value is honored directly (operator override).
    /// - Otherwise, derived from `cpu_cores / max_concurrency`:
    ///     * `cpu_cores == 0` (auto) -> half of available logical cores
    /// - Result is always >= 1.
    pub fn effective_threads_per_encode(&self, max_concurrency: usize) -> usize {
        if self.ffmpeg_threads > 0 {
            return self.ffmpeg_threads;
        }
        let cores = if self.cpu_cores > 0 {
            self.cpu_cores
        } else {
            (available_logical_cores() / 2).max(1)
        };
        if max_concurrency == 0 {
            cores
        } else {
            (cores / max_concurrency).max(1)
        }
    }

    /// Total thread usage across all concurrent encodes — for display and validation.
    pub fn effective_total_threads(&self, max_concurrency: usize) -> usize {
        let per = self.effective_threads_per_encode(max_concurrency);
        per.saturating_mul(max_concurrency.max(1))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub crf: u8,
    pub maxrate: String,
    pub bufsize: String,
}

fn default_enabled() -> bool {
    true
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            crf: 24,
            maxrate: "15M".into(),
            bufsize: "16M".into(),
        }
    }
}

impl ProfileConfig {
    pub fn profile_a_default() -> Self {
        Self {
            enabled: true,
            crf: 24,
            maxrate: "15M".into(),
            bufsize: "16M".into(),
        }
    }
    pub fn profile_b_default() -> Self {
        Self {
            enabled: true,
            crf: 23,
            maxrate: "15M".into(),
            bufsize: "16M".into(),
        }
    }
    pub fn profile_c_default() -> Self {
        Self {
            enabled: true,
            crf: 20,
            maxrate: "5M".into(),
            bufsize: "6M".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionConfig {
    #[serde(default = "default_settle")]
    pub settle_secs: u64,
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_stable_polls")]
    pub stable_polls_min: u32,
    #[serde(default = "default_retry_policy")]
    pub retry_policy: String,
    /// On startup, purge error rows whose source file is still in the watch folder so the
    /// watcher will re-queue them automatically. Rows whose source no longer exists are kept
    /// for operator inspection.
    #[serde(default = "default_auto_retry_on_start")]
    pub auto_retry_on_start: bool,
    /// How many times to retry an encode before giving up and marking the asset `error`.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Delay (ms) between retry attempts for the same input.
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
    #[serde(default)]
    pub clean_source_after_success: bool,
    #[serde(default)]
    pub include_extensions: Vec<String>,
    #[serde(default)]
    pub exclude_extensions: Vec<String>,
}

fn default_settle() -> u64 {
    5
}
fn default_poll() -> u64 {
    10
}
fn default_concurrency() -> usize {
    2
}
fn default_stable_polls() -> u32 {
    2
}
fn default_retry_policy() -> String {
    "once".into()
}
fn default_auto_retry_on_start() -> bool {
    true
}
fn default_max_attempts() -> u32 {
    2
}
fn default_retry_delay_ms() -> u64 {
    2000
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            settle_secs: 5,
            poll_secs: 10,
            max_concurrency: 2,
            stable_polls_min: 2,
            retry_policy: "once".into(),
            auto_retry_on_start: true,
            max_attempts: 2,
            retry_delay_ms: 2000,
            clean_source_after_success: false,
            include_extensions: Vec::new(),
            exclude_extensions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_file")]
    pub log_file: String,
}

fn default_log_level() -> String {
    "info".into()
}
fn default_log_file() -> String {
    "transcode.log".into()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            log_file: "transcode.log".into(),
        }
    }
}

// ============================================================================
// V2 Typed Policy Model Structs (Additive)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    LegacyV1Encode,
    EbuR128,
    AtscA85,
    PassthroughValidate,
    AnalyzeOnly,
}

impl Default for AudioMode {
    fn default() -> Self {
        AudioMode::LegacyV1Encode
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioPolicy {
    #[serde(default)]
    pub mode: AudioMode,
    #[serde(default = "default_audio_codec")]
    pub codec: String,
    #[serde(default = "default_audio_bitrate")]
    pub bitrate: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate_hz: u32,
    #[serde(default = "default_channels")]
    pub channels: u32,
    #[serde(default)]
    pub channel_layout: Option<String>,
    #[serde(default)]
    pub target_lufs: Option<f64>,
    #[serde(default)]
    pub true_peak_dbtp: Option<f64>,
    #[serde(default)]
    pub lra_target: Option<f64>,
    #[serde(default)]
    pub dual_mono: bool,
    #[serde(default)]
    pub preserve_original: bool,
}

fn default_sample_rate() -> u32 {
    48000
}
fn default_channels() -> u32 {
    2
}

impl Default for AudioPolicy {
    fn default() -> Self {
        Self {
            mode: AudioMode::LegacyV1Encode,
            codec: "aac".into(),
            bitrate: "320k".into(),
            sample_rate_hz: 48000,
            channels: 2,
            channel_layout: None,
            target_lufs: None,
            true_peak_dbtp: None,
            lra_target: None,
            dual_mono: false,
            preserve_original: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationPolicy {
    #[serde(default = "default_true")]
    pub enforce_closed_gop: bool,
    #[serde(default = "default_true")]
    pub enforce_faststart: bool,
    #[serde(default = "default_true")]
    pub enforce_48k_audio: bool,
    #[serde(default = "default_dur_tolerance")]
    pub max_duration_delta_ms: i64,
    #[serde(default)]
    pub strict_ready_blocking: bool,
}

fn default_true() -> bool {
    true
}
fn default_dur_tolerance() -> i64 {
    80
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self {
            enforce_closed_gop: true,
            enforce_faststart: true,
            enforce_48k_audio: true,
            max_duration_delta_ms: 80,
            strict_ready_blocking: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoragePolicy {
    #[serde(default)]
    pub atomic_publication: bool,
    #[serde(default = "default_true")]
    pub preserve_subclips_on_purge: bool,
    #[serde(default)]
    pub clean_source_after_success: bool,
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            atomic_publication: false,
            preserve_subclips_on_purge: true,
            clean_source_after_success: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryPolicyV2 {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
    #[serde(default = "default_true")]
    pub auto_retry_on_start: bool,
}

impl Default for RetryPolicyV2 {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            retry_delay_ms: 2000,
            auto_retry_on_start: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolchainPolicy {
    #[serde(default)]
    pub ffmpeg_path: Option<String>,
    #[serde(default)]
    pub ffprobe_path: Option<String>,
    #[serde(default = "default_true")]
    pub verify_on_startup: bool,
}

impl Default for ToolchainPolicy {
    fn default() -> Self {
        Self {
            ffmpeg_path: None,
            ffprobe_path: None,
            verify_on_startup: true,
        }
    }
}

fn default_config_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_config_version")]
    pub version: u32,

    pub paths: PathsConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub encoding: EncodingConfig,
    #[serde(default = "ProfileConfig::profile_a_default")]
    pub profile_a: ProfileConfig,
    #[serde(default = "ProfileConfig::profile_b_default")]
    pub profile_b: ProfileConfig,
    #[serde(default = "ProfileConfig::profile_c_default")]
    pub profile_c: ProfileConfig,
    #[serde(default)]
    pub ingestion: IngestionConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default = "default_initialized")]
    pub initialized: bool,

    // Optional V2 Policy Sections (additive, initialized with V1 migration defaults when version == 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_policy: Option<AudioPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_policy: Option<ValidationPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_policy: Option<StoragePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy_v2: Option<RetryPolicyV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain_policy: Option<ToolchainPolicy>,
}

fn default_initialized() -> bool {
    false
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            paths: PathsConfig {
                watch_folder: String::new(),
                target_folder: String::new(),
            },
            server: ServerConfig {
                web_port: 4353,
                bind_address: "127.0.0.1".into(),
            },
            encoding: EncodingConfig {
                preset: "medium".into(),
                ffmpeg_threads: 0,
                cpu_cores: 0,
                audio_codec: "aac".into(),
                audio_bitrate: "320k".into(),
                tune: "film".into(),
                probesize: "500M".into(),
                analyzeduration: "500M".into(),
            },
            profile_a: ProfileConfig::profile_a_default(),
            profile_b: ProfileConfig::profile_b_default(),
            profile_c: ProfileConfig::profile_c_default(),
            ingestion: IngestionConfig::default(),
            logging: LoggingConfig::default(),
            initialized: false,
            audio_policy: None,
            validation_policy: None,
            storage_policy: None,
            retry_policy_v2: None,
            toolchain_policy: None,
        }
    }
}

impl AppConfig {
    /// Returns effective AudioPolicy derived in-memory or from explicit V2 settings.
    pub fn effective_audio_policy(&self) -> AudioPolicy {
        if let Some(ref ap) = self.audio_policy {
            if ap.codec != self.encoding.audio_codec || ap.bitrate != self.encoding.audio_bitrate {
                tracing::warn!(
                    "Audio policy configuration conflict: explicit audio_policy codec/bitrate ({}/{}) differs from legacy encoding settings ({}/{})",
                    ap.codec, ap.bitrate, self.encoding.audio_codec, self.encoding.audio_bitrate
                );
            }
            ap.clone()
        } else {
            AudioPolicy {
                mode: AudioMode::LegacyV1Encode,
                codec: self.encoding.audio_codec.clone(),
                bitrate: self.encoding.audio_bitrate.clone(),
                sample_rate_hz: 48000,
                channels: 2,
                channel_layout: None,
                target_lufs: None,
                true_peak_dbtp: None,
                lra_target: None,
                dual_mono: false,
                preserve_original: false,
            }
        }
    }

    /// Returns effective ValidationPolicy derived in-memory or from explicit V2 settings.
    pub fn effective_validation_policy(&self) -> ValidationPolicy {
        self.validation_policy.clone().unwrap_or_default()
    }

    /// Returns effective StoragePolicy derived in-memory or from explicit V2 settings.
    pub fn effective_storage_policy(&self) -> StoragePolicy {
        if let Some(ref sp) = self.storage_policy {
            if sp.clean_source_after_success != self.ingestion.clean_source_after_success {
                tracing::warn!(
                    "Storage policy configuration conflict: explicit storage_policy.clean_source_after_success ({}) differs from legacy ingestion setting ({})",
                    sp.clean_source_after_success, self.ingestion.clean_source_after_success
                );
            }
            sp.clone()
        } else {
            StoragePolicy {
                atomic_publication: false,
                preserve_subclips_on_purge: true,
                clean_source_after_success: self.ingestion.clean_source_after_success,
            }
        }
    }

    /// Returns effective RetryPolicyV2 derived in-memory or from explicit V2 settings.
    pub fn effective_retry_policy(&self) -> RetryPolicyV2 {
        if let Some(ref rp) = self.retry_policy_v2 {
            rp.clone()
        } else {
            RetryPolicyV2 {
                max_attempts: self.ingestion.max_attempts,
                retry_delay_ms: self.ingestion.retry_delay_ms,
                auto_retry_on_start: self.ingestion.auto_retry_on_start,
            }
        }
    }

    /// Returns effective ToolchainPolicy derived in-memory or from explicit V2 settings.
    pub fn effective_toolchain_policy(&self) -> ToolchainPolicy {
        self.toolchain_policy.clone().unwrap_or_default()
    }

    pub fn load(path: Option<&str>) -> Result<(Self, PathBuf), String> {
        let config_path = path.map(PathBuf::from).unwrap_or_else(default_config_path);

        if !config_path.exists() {
            let defaults = Self::default();
            defaults.save_to(&config_path)?;
            tracing::info!("Created default config at {}", config_path.display());
            return Ok((defaults, config_path));
        }

        let content = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config '{}': {}", config_path.display(), e))?;

        let config: AppConfig = toml::from_str(&content)
            .map_err(|e| format!("Failed to parse config '{}': {}", config_path.display(), e))?;

        Ok((config, config_path))
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let serialized = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {}", e))?;
        }
        fs::write(path, serialized)
            .map_err(|e| format!("Failed to write config '{}': {}", path.display(), e))?;
        Ok(())
    }

    pub fn run_wizard() -> Result<Self, String> {
        println!("\n=== PlayoutTranscode Configuration Wizard ===\n");
        println!("This wizard will guide you through first-time setup.\n");

        let mut config = Self::default();

        config.paths.watch_folder = prompt(
            "Watch Folder (source media directory)",
            &config.paths.watch_folder,
        )?;
        config.paths.target_folder = prompt(
            "Target Folder (output transcoded media directory)",
            &config.paths.target_folder,
        )?;

        let port_str =
            prompt_with_default("Web Monitor Port", &config.server.web_port.to_string())?;
        if let Ok(p) = port_str.parse::<u16>() {
            config.server.web_port = p;
        }

        let preset_opts = "ultrafast|veryfast|faster|fast|medium|slow|slower|veryslow";
        config.encoding.preset = prompt_with_default(
            &format!("x264 preset ({})", preset_opts),
            &config.encoding.preset,
        )?;

        let crf_a = prompt_with_default(
            "Profile A (HD Progressive) CRF",
            &config.profile_a.crf.to_string(),
        )?;
        if let Ok(c) = crf_a.parse() {
            config.profile_a.crf = c;
        }

        let crf_b = prompt_with_default(
            "Profile B (HD Interlaced) CRF",
            &config.profile_b.crf.to_string(),
        )?;
        if let Ok(c) = crf_b.parse() {
            config.profile_b.crf = c;
        }

        let crf_c =
            prompt_with_default("Profile C (SD PAL) CRF", &config.profile_c.crf.to_string())?;
        if let Ok(c) = crf_c.parse() {
            config.profile_c.crf = c;
        }

        let audio_opts = "aac|pcm_s16le";
        config.encoding.audio_codec = prompt_with_default(
            &format!("Audio codec ({})", audio_opts),
            &config.encoding.audio_codec,
        )?;

        let tune_opts = "film|grain|animation|none";
        config.encoding.tune =
            prompt_with_default(&format!("x264 tune ({})", tune_opts), &config.encoding.tune)?;

        let concurrency = prompt_with_default(
            "Max concurrent encodes",
            &config.ingestion.max_concurrency.to_string(),
        )?;
        if let Ok(c) = concurrency.parse() {
            config.ingestion.max_concurrency = c;
        }

        config.initialized = true;
        let path = default_config_path();
        config.save_to(&path)?;
        println!("\nConfiguration saved to {}\n", path.display());
        println!("Run 'PlayoutTranscode run' to start the service.\n");

        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.paths.watch_folder.trim().is_empty() {
            return Err("Watch folder is not configured".into());
        }
        if self.paths.target_folder.trim().is_empty() {
            return Err("Target folder is not configured".into());
        }
        let watch = std::path::Path::new(&self.paths.watch_folder);
        if !watch.exists() || !watch.is_dir() {
            return Err(format!(
                "Watch folder does not exist: {}",
                self.paths.watch_folder
            ));
        }
        fs::create_dir_all(&self.paths.target_folder)
            .map_err(|e| format!("Cannot create target folder: {}", e))?;

        let valid_presets = [
            "ultrafast",
            "veryfast",
            "faster",
            "fast",
            "medium",
            "slow",
            "slower",
            "veryslow",
        ];
        if !valid_presets.contains(&self.encoding.preset.as_str()) {
            return Err(format!(
                "Invalid preset '{}'. Valid: {:?}",
                self.encoding.preset, valid_presets
            ));
        }
        let valid_audio = ["aac", "pcm_s16le", "libmp3lame"];
        if !valid_audio.contains(&self.encoding.audio_codec.as_str()) {
            return Err(format!(
                "Invalid audio codec '{}'. Valid: {:?}",
                self.encoding.audio_codec, valid_audio
            ));
        }

        if self.profile_a.crf > 51 {
            return Err("Profile A CRF must be 0-51".into());
        }
        if self.profile_b.crf > 51 {
            return Err("Profile B CRF must be 0-51".into());
        }
        if self.profile_c.crf > 51 {
            return Err("Profile C CRF must be 0-51".into());
        }

        if self.ingestion.max_concurrency == 0 {
            return Err("max_concurrency must be at least 1".into());
        }
        if self.ingestion.max_attempts == 0 {
            return Err("max_attempts must be at least 1".into());
        }
        let max_cores = available_logical_cores();
        if self.encoding.cpu_cores > max_cores {
            return Err(format!(
                "cpu_cores ({}) exceeds available logical cores ({})",
                self.encoding.cpu_cores, max_cores
            ));
        }
        // Warn (not fail) if the configured budget can oversubscribe the host.
        let total = self
            .encoding
            .effective_total_threads(self.ingestion.max_concurrency);
        if total > max_cores {
            tracing::warn!(
                "Thread budget oversubscription: effective {} threads across {} encodes on {} logical cores",
                total, self.ingestion.max_concurrency, max_cores
            );
        }

        // Validate V2 AudioPolicy when mode is EbuR128 or AtscA85
        let audio_pol = self.effective_audio_policy();
        if audio_pol.mode == AudioMode::EbuR128 || audio_pol.mode == AudioMode::AtscA85 {
            if let Some(lufs) = audio_pol.target_lufs {
                if !(-70.0..=0.0).contains(&lufs) {
                    return Err(format!(
                        "AudioPolicy target_lufs ({}) must be between -70.0 and 0.0",
                        lufs
                    ));
                }
            }
            if let Some(tp) = audio_pol.true_peak_dbtp {
                if !(-10.0..=0.0).contains(&tp) {
                    return Err(format!(
                        "AudioPolicy true_peak_dbtp ({}) must be between -10.0 and 0.0",
                        tp
                    ));
                }
            }
        }
        if audio_pol.sample_rate_hz == 0 {
            return Err("AudioPolicy sample_rate_hz must be > 0".into());
        }
        if audio_pol.channels == 0 {
            return Err("AudioPolicy channels must be > 0".into());
        }

        // Validate V2 ValidationPolicy
        let val_pol = self.effective_validation_policy();
        if val_pol.max_duration_delta_ms < 0 {
            return Err("ValidationPolicy max_duration_delta_ms must be >= 0".into());
        }

        Ok(())
    }
}

fn prompt(label: &str, default: &str) -> Result<String, String> {
    print!("{}", label);
    if !default.is_empty() {
        print!(" [{}]", default);
    }
    print!(": ");
    io::stdout()
        .flush()
        .map_err(|e| format!("IO error: {}", e))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("IO error: {}", e))?;
    let trimmed = input.trim().to_string();
    if trimmed.is_empty() && !default.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed)
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String, String> {
    prompt(label, default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v1_config_load_and_derive_v2_policies() {
        let v1_toml = r#"
initialized = true

[paths]
watch_folder = "C:/test/in"
target_folder = "C:/test/out"

[server]
web_port = 4353

[encoding]
preset = "fast"
audio_codec = "aac"
audio_bitrate = "320k"

[ingestion]
max_concurrency = 4
auto_retry_on_start = true
clean_source_after_success = true
"#;

        let cfg: AppConfig = toml::from_str(v1_toml).expect("V1 TOML must parse without error");

        assert_eq!(
            cfg.version, 1,
            "Unversioned config must default to version 1"
        );
        assert_eq!(cfg.paths.watch_folder, "C:/test/in");
        assert_eq!(cfg.encoding.preset, "fast");
        assert!(
            cfg.audio_policy.is_none(),
            "Unversioned config must have None for explicit audio_policy"
        );

        let effective_audio = cfg.effective_audio_policy();
        assert_eq!(effective_audio.mode, AudioMode::LegacyV1Encode);
        assert_eq!(effective_audio.codec, "aac");
        assert_eq!(effective_audio.bitrate, "320k");
        assert_eq!(effective_audio.sample_rate_hz, 48000);
        assert_eq!(effective_audio.channels, 2);
        assert_eq!(effective_audio.target_lufs, None);

        let effective_val = cfg.effective_validation_policy();
        assert_eq!(effective_val.enforce_closed_gop, true);
        assert_eq!(effective_val.max_duration_delta_ms, 80);

        let effective_storage = cfg.effective_storage_policy();
        assert_eq!(effective_storage.clean_source_after_success, true);
        assert_eq!(effective_storage.atomic_publication, false);

        let effective_retry = cfg.effective_retry_policy();
        assert_eq!(effective_retry.auto_retry_on_start, true);
        assert_eq!(effective_retry.max_attempts, 2);
    }

    #[test]
    fn test_explicit_v2_config_wins_over_derived_legacy() {
        let v2_toml = r#"
version = 2
initialized = true

[paths]
watch_folder = "C:/test/in"
target_folder = "C:/test/out"

[encoding]
preset = "medium"
audio_codec = "aac"
audio_bitrate = "320k"

[audio_policy]
mode = "ebu_r128"
codec = "pcm_s16le"
bitrate = "1536k"
sample_rate_hz = 48000
channels = 2
target_lufs = -23.0
true_peak_dbtp = -1.0

[storage_policy]
atomic_publication = true
preserve_subclips_on_purge = true
clean_source_after_success = false
"#;

        let cfg: AppConfig = toml::from_str(v2_toml).expect("V2 TOML must parse without error");

        assert_eq!(cfg.version, 2);
        let effective_audio = cfg.effective_audio_policy();
        assert_eq!(effective_audio.mode, AudioMode::EbuR128);
        assert_eq!(effective_audio.codec, "pcm_s16le");
        assert_eq!(effective_audio.target_lufs, Some(-23.0));

        let effective_storage = cfg.effective_storage_policy();
        assert_eq!(effective_storage.atomic_publication, true);
    }

    #[test]
    fn test_validation_rules_for_v2_audio_policy() {
        let mut cfg = AppConfig::default();
        cfg.paths.watch_folder = std::env::temp_dir().to_string_lossy().to_string();
        cfg.paths.target_folder = std::env::temp_dir().to_string_lossy().to_string();

        cfg.audio_policy = Some(AudioPolicy {
            mode: AudioMode::EbuR128,
            codec: "aac".into(),
            bitrate: "320k".into(),
            sample_rate_hz: 48000,
            channels: 2,
            channel_layout: None,
            target_lufs: Some(-90.0), // Invalid (< -70.0)
            true_peak_dbtp: Some(-1.0),
            lra_target: None,
            dual_mono: false,
            preserve_original: false,
        });

        assert!(
            cfg.validate().is_err(),
            "Invalid target_lufs (-90) must fail validation"
        );

        cfg.audio_policy.as_mut().unwrap().target_lufs = Some(-23.0);
        assert!(
            cfg.validate().is_ok(),
            "Valid LUFS (-23) must pass validation"
        );
    }

    #[test]
    fn test_toml_serialization_roundtrip() {
        let mut cfg = AppConfig::default();
        cfg.version = 2;
        cfg.paths.watch_folder = "D:/in".into();
        cfg.paths.target_folder = "D:/out".into();
        cfg.audio_policy = Some(AudioPolicy {
            mode: AudioMode::AtscA85,
            codec: "aac".into(),
            bitrate: "320k".into(),
            sample_rate_hz: 48000,
            channels: 2,
            channel_layout: Some("stereo".into()),
            target_lufs: Some(-24.0),
            true_peak_dbtp: Some(-2.0),
            lra_target: Some(7.0),
            dual_mono: false,
            preserve_original: false,
        });

        let serialized = toml::to_string_pretty(&cfg).expect("Serialization must succeed");
        let deserialized: AppConfig =
            toml::from_str(&serialized).expect("Deserialization must succeed");

        assert_eq!(deserialized.version, 2);
        assert_eq!(deserialized.paths.watch_folder, "D:/in");
        assert_eq!(
            deserialized.effective_audio_policy().mode,
            AudioMode::AtscA85
        );
        assert_eq!(
            deserialized.effective_audio_policy().target_lufs,
            Some(-24.0)
        );
    }

    #[test]
    fn test_unknown_future_fields_ignored() {
        let future_toml = r#"
version = 3
future_unknown_field = "some_value"

[paths]
watch_folder = "C:/test/in"
target_folder = "C:/test/out"

[future_unknown_section]
feature_x = true
"#;

        let cfg: AppConfig =
            toml::from_str(future_toml).expect("Unknown future fields must be ignored");
        assert_eq!(cfg.version, 3);
        assert_eq!(cfg.paths.watch_folder, "C:/test/in");
    }
}
