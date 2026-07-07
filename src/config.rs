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

fn default_watch() -> String { String::new() }
fn default_target() -> String { String::new() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
}

fn default_web_port() -> u16 { 4353 }
fn default_bind_address() -> String { "127.0.0.1".to_string() }

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

fn default_preset() -> String { "medium".into() }
fn default_threads() -> usize { 0 }
fn default_cpu_cores() -> usize { 0 }
fn default_audio_codec() -> String { "aac".into() }
fn default_audio_bitrate() -> String { "320k".into() }
fn default_tune() -> String { "film".into() }
fn default_probesize() -> String { "500M".into() }
fn default_analyzeduration() -> String { "500M".into() }

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

fn default_enabled() -> bool { true }

impl Default for ProfileConfig {
    fn default() -> Self {
        Self { enabled: true, crf: 24, maxrate: "15M".into(), bufsize: "16M".into() }
    }
}

impl ProfileConfig {
    pub fn profile_a_default() -> Self {
        Self { enabled: true, crf: 24, maxrate: "15M".into(), bufsize: "16M".into() }
    }
    pub fn profile_b_default() -> Self {
        Self { enabled: true, crf: 23, maxrate: "15M".into(), bufsize: "16M".into() }
    }
    pub fn profile_c_default() -> Self {
        Self { enabled: true, crf: 20, maxrate: "5M".into(), bufsize: "6M".into() }
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

fn default_settle() -> u64 { 5 }
fn default_poll() -> u64 { 10 }
fn default_concurrency() -> usize { 2 }
fn default_stable_polls() -> u32 { 2 }
fn default_retry_policy() -> String { "once".into() }
fn default_auto_retry_on_start() -> bool { true }
fn default_max_attempts() -> u32 { 2 }
fn default_retry_delay_ms() -> u64 { 2000 }

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

fn default_log_level() -> String { "info".into() }
fn default_log_file() -> String { "transcode.log".into() }

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".into(), log_file: "transcode.log".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
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
}

fn default_initialized() -> bool { false }

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            paths: PathsConfig { watch_folder: String::new(), target_folder: String::new() },
            server: ServerConfig { web_port: 4353, bind_address: "127.0.0.1".into() },
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
        }
    }
}

impl AppConfig {
    pub fn load(path: Option<&str>) -> Result<(Self, PathBuf), String> {
        let config_path = path
            .map(PathBuf::from)
            .unwrap_or_else(default_config_path);

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

        let port_str = prompt_with_default(
            "Web Monitor Port",
            &config.server.web_port.to_string(),
        )?;
        if let Ok(p) = port_str.parse::<u16>() {
            config.server.web_port = p;
        }

        let preset_opts = "ultrafast|veryfast|faster|fast|medium|slow|slower|veryslow";
        config.encoding.preset = prompt_with_default(
            &format!("x264 preset ({})", preset_opts),
            &config.encoding.preset,
        )?;

        let crf_a = prompt_with_default("Profile A (HD Progressive) CRF", &config.profile_a.crf.to_string())?;
        if let Ok(c) = crf_a.parse() { config.profile_a.crf = c; }

        let crf_b = prompt_with_default("Profile B (HD Interlaced) CRF", &config.profile_b.crf.to_string())?;
        if let Ok(c) = crf_b.parse() { config.profile_b.crf = c; }

        let crf_c = prompt_with_default("Profile C (SD PAL) CRF", &config.profile_c.crf.to_string())?;
        if let Ok(c) = crf_c.parse() { config.profile_c.crf = c; }

        let audio_opts = "aac|pcm_s16le";
        config.encoding.audio_codec = prompt_with_default(
            &format!("Audio codec ({})", audio_opts),
            &config.encoding.audio_codec,
        )?;

        let tune_opts = "film|grain|animation|none";
        config.encoding.tune = prompt_with_default(
            &format!("x264 tune ({})", tune_opts),
            &config.encoding.tune,
        )?;

        let concurrency = prompt_with_default("Max concurrent encodes", &config.ingestion.max_concurrency.to_string())?;
        if let Ok(c) = concurrency.parse() { config.ingestion.max_concurrency = c; }

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
            return Err(format!("Watch folder does not exist: {}", self.paths.watch_folder));
        }
        fs::create_dir_all(&self.paths.target_folder)
            .map_err(|e| format!("Cannot create target folder: {}", e))?;

        let valid_presets = ["ultrafast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow"];
        if !valid_presets.contains(&self.encoding.preset.as_str()) {
            return Err(format!("Invalid preset '{}'. Valid: {:?}", self.encoding.preset, valid_presets));
        }
        let valid_audio = ["aac", "pcm_s16le", "libmp3lame"];
        if !valid_audio.contains(&self.encoding.audio_codec.as_str()) {
            return Err(format!("Invalid audio codec '{}'. Valid: {:?}", self.encoding.audio_codec, valid_audio));
        }

        if self.profile_a.crf > 51 { return Err("Profile A CRF must be 0-51".into()); }
        if self.profile_b.crf > 51 { return Err("Profile B CRF must be 0-51".into()); }
        if self.profile_c.crf > 51 { return Err("Profile C CRF must be 0-51".into()); }

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
        let total = self.encoding.effective_total_threads(self.ingestion.max_concurrency);
        if total > max_cores {
            tracing::warn!(
                "Thread budget oversubscription: effective {} threads across {} encodes on {} logical cores",
                total, self.ingestion.max_concurrency, max_cores
            );
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
    io::stdout().flush().map_err(|e| format!("IO error: {}", e))?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| format!("IO error: {}", e))?;
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
