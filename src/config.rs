use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn default_config_path() -> PathBuf {
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
    /// Extra browser origins allowed by CORS, e.g. the Vue dev server
    /// (`http://localhost:5173`). Loopback origins on `web_port` are always
    /// allowed; everything else is rejected.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Shared secret required on every `/api/**` request except the health
    /// endpoints. Empty means "loopback-only, no auth", which is only legal
    /// when `bind_address` is a loopback address.
    ///
    /// Generate one with `PlayoutTranscode gen-token`. Never logged, and
    /// reported by `GET /api/config` as `api_token_set` only.
    #[serde(default)]
    pub api_token: String,
}

/// `true` when `s` is an FFmpeg size/rate literal: digits, optional decimal
/// part, optional `k`/`M`/`G` suffix. Anything else is passed straight to
/// FFmpeg on the command line and breaks every subsequent encode (F-03).
pub fn is_valid_ffmpeg_quantity(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 32 {
        return false;
    }
    let body = match s.chars().last() {
        Some(c) if matches!(c, 'k' | 'K' | 'm' | 'M' | 'g' | 'G') => &s[..s.len() - c.len_utf8()],
        _ => s,
    };
    if body.is_empty() {
        return false;
    }
    let mut seen_dot = false;
    for ch in body.chars() {
        if ch == '.' {
            if seen_dot {
                return false;
            }
            seen_dot = true;
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    body.chars().any(|c| c.is_ascii_digit())
}

/// Lower-cased, separator-normalised form used for the "is this a forbidden
/// or overlapping directory" comparisons. Windows paths are case-insensitive
/// and accept either separator, so compare on a normalised form rather than
/// on the raw string.
fn normalize_dir(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// `true` when a normalised path is a drive root (`c:`) or the POSIX root
/// (which normalises to the empty string).
fn is_drive_or_fs_root(norm: &str) -> bool {
    norm.is_empty() || (norm.len() <= 2 && norm.ends_with(':'))
}

/// `true` when `child` is `parent` or lives underneath it.
fn is_within(child: &str, parent: &str) -> bool {
    child == parent || child.starts_with(&format!("{}/", parent))
}

/// Directories the service must never be pointed at, as `(root, exact_only)`
/// pairs.
///
/// `PUT /api/config` is reachable without credentials, and
/// `clean_source_after_success` deletes sources after a successful encode, so
/// `watch_folder = C:\Users` was a remote-driven mass-deletion primitive
/// (F-03). System trees are forbidden outright; the profile containers are
/// forbidden only as exact roots, because `C:\Users\op\Media\Ingest` is a
/// perfectly ordinary place to put a watch folder.
fn forbidden_roots() -> Vec<(String, bool)> {
    let mut roots: Vec<(String, bool)> = Vec::new();
    for var in ["SystemRoot", "windir", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                roots.push((normalize_dir(Path::new(&v)), false));
            }
        }
    }
    for var in ["ProgramData", "USERPROFILE", "PUBLIC"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                let norm = normalize_dir(Path::new(&v));
                // Also forbid the container itself (`C:/Users`).
                if let Some(parent) = Path::new(&norm).parent() {
                    let parent = normalize_dir(parent);
                    if !is_drive_or_fs_root(&parent) {
                        roots.push((parent, true));
                    }
                }
                roots.push((norm, true));
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push((normalize_dir(dir), false));
        }
    }
    if !cfg!(windows) {
        for r in ["/bin", "/boot", "/dev", "/etc", "/proc", "/sys", "/usr"] {
            roots.push((r.to_string(), false));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Reject a media root that is a drive/filesystem root, a system location, or
/// the exe directory.
fn validate_media_root(label: &str, raw: &str) -> Result<String, String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!("{} must be an absolute path: '{}'", label, raw));
    }
    let norm = normalize_dir(path);
    // A drive root normalises to "c:" and a POSIX root to "".
    if is_drive_or_fs_root(&norm) {
        return Err(format!("{} must not be a filesystem or drive root", label));
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(format!("{} must not contain '.' or '..': '{}'", label, raw));
    }
    for (root, exact_only) in forbidden_roots() {
        let hit = if exact_only {
            norm == root
        } else {
            is_within(&norm, &root)
        };
        if hit {
            return Err(format!(
                "{} must not be a system, program or profile directory ('{}')",
                label, raw
            ));
        }
    }
    Ok(norm)
}

/// Minimum length for `server.api_token`. `gen_api_token` produces 43
/// characters (32 random bytes, URL-safe base64, unpadded).
pub const MIN_API_TOKEN_LEN: usize = 32;

/// Generate a fresh API token: 32 bytes of OS randomness, URL-safe base64.
///
/// Hand-rolled rather than pulling in `rand`/`base64` for one call site; the
/// entropy comes from the OS via `getrandom`-equivalent APIs.
pub fn gen_api_token() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = os_random_bytes(32);
    let mut out = String::with_capacity(43);
    // Standard base64url over 32 bytes = 42 full chars + 1 from the remainder.
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        let chars = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for &c in chars.iter().take(keep) {
            out.push(ALPHABET[c as usize] as char);
        }
    }
    out
}

fn os_random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    #[cfg(windows)]
    {
        // BCryptGenRandom via the documented "use system preferred RNG" flag.
        extern "system" {
            fn BCryptGenRandom(
                h_algorithm: *mut std::ffi::c_void,
                pb_buffer: *mut u8,
                cb_buffer: u32,
                dw_flags: u32,
            ) -> i32;
        }
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert!(status >= 0, "BCryptGenRandom failed with status {}", status);
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut buf))
            .expect("read /dev/urandom");
    }
    buf
}

/// Constant-time comparison, so a wrong token cannot be recovered by timing
/// how far the comparison got.
pub fn tokens_match(expected: &str, provided: &str) -> bool {
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// True when `bind_address` only accepts connections from this machine.
pub fn is_loopback_bind(bind_address: &str) -> bool {
    let host = bind_address
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
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
                allowed_origins: Vec::new(),
                api_token: String::new(),
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
        // Write-then-rename so a crash or a full disk mid-write cannot leave a
        // truncated config.toml behind (same pattern as `write_sidecar_payload`).
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, serialized)
            .map_err(|e| format!("Failed to write config '{}': {}", tmp.display(), e))?;
        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(format!(
                "Failed to replace config '{}': {}",
                path.display(),
                e
            ));
        }
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
        // Binding anywhere other than loopback exposes every mutating route to
        // the LAN, so it is allowed only with an API token set (F-02).
        if !is_loopback_bind(&self.server.bind_address) && self.server.api_token.trim().is_empty() {
            return Err(format!(
                "server.bind_address '{}' is not a loopback address; a non-loopback bind                  requires server.api_token (run `PlayoutTranscode gen-token`)",
                self.server.bind_address
            ));
        }
        if !self.server.api_token.is_empty() {
            let t = self.server.api_token.trim();
            if t.len() < MIN_API_TOKEN_LEN {
                return Err(format!(
                    "server.api_token must be at least {} characters",
                    MIN_API_TOKEN_LEN
                ));
            }
            if t.len() != self.server.api_token.len() {
                return Err("server.api_token must not have leading or trailing whitespace".into());
            }
            if !t.chars().all(|c| c.is_ascii_graphic()) {
                return Err("server.api_token must be printable ASCII with no spaces".into());
            }
        }
        for origin in &self.server.allowed_origins {
            if !origin.starts_with("http://") && !origin.starts_with("https://") {
                return Err(format!(
                    "server.allowed_origins entry '{}' must be a full origin, e.g. http://localhost:5173",
                    origin
                ));
            }
            if origin.ends_with('/') || origin.matches('/').count() != 2 {
                return Err(format!(
                    "server.allowed_origins entry '{}' must be scheme://host[:port] with no path",
                    origin
                ));
            }
        }
        if self.paths.watch_folder.trim().is_empty() {
            return Err("Watch folder is not configured".into());
        }
        if self.paths.target_folder.trim().is_empty() {
            return Err("Target folder is not configured".into());
        }
        let watch_norm = validate_media_root("watch_folder", self.paths.watch_folder.trim())?;
        let target_norm = validate_media_root("target_folder", self.paths.target_folder.trim())?;

        // Overlap makes every published mezzanine look like a new source, so
        // the service re-ingests its own output forever (F-14).
        if is_within(&target_norm, &watch_norm) || is_within(&watch_norm, &target_norm) {
            return Err(
                "watch_folder and target_folder must not overlap; publishing into the watch \
                 folder causes an endless re-ingest loop"
                    .into(),
            );
        }
        // If both already exist, compare their canonical forms too, so symlinks
        // and 8.3 short names cannot hide the overlap.
        let watch = Path::new(self.paths.watch_folder.trim());
        let target = Path::new(self.paths.target_folder.trim());
        if let (Ok(cw), Ok(ct)) = (watch.canonicalize(), target.canonicalize()) {
            let cw = normalize_dir(&cw);
            let ct = normalize_dir(&ct);
            if is_within(&ct, &cw) || is_within(&cw, &ct) {
                return Err(
                    "watch_folder and target_folder resolve to overlapping directories".into(),
                );
            }
        }
        if !watch.exists() || !watch.is_dir() {
            return Err(format!(
                "Watch folder does not exist: {}",
                self.paths.watch_folder
            ));
        }
        // `validate()` is pure: the target directory is created by
        // `start_processing_loop`, not as a side effect of validating an
        // unauthenticated `PUT /api/config` body.

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


        // Every string below is handed to FFmpeg on the command line. An
        // unvalidated value does not fail here, it fails on every encode from
        // then on (F-03).
        let valid_tunes = [
            "film",
            "animation",
            "grain",
            "stillimage",
            "fastdecode",
            "zerolatency",
            "none",
            "",
        ];
        if !valid_tunes.contains(&self.encoding.tune.trim()) {
            return Err(format!(
                "Invalid encoding.tune '{}'. Valid: {:?}",
                self.encoding.tune, valid_tunes
            ));
        }
        for (label, value) in [
            ("encoding.probesize", &self.encoding.probesize),
            ("encoding.analyzeduration", &self.encoding.analyzeduration),
            ("encoding.audio_bitrate", &self.encoding.audio_bitrate),
            ("profile_a.maxrate", &self.profile_a.maxrate),
            ("profile_a.bufsize", &self.profile_a.bufsize),
            ("profile_b.maxrate", &self.profile_b.maxrate),
            ("profile_b.bufsize", &self.profile_b.bufsize),
            ("profile_c.maxrate", &self.profile_c.maxrate),
            ("profile_c.bufsize", &self.profile_c.bufsize),
        ] {
            if !is_valid_ffmpeg_quantity(value) {
                return Err(format!(
                    "{} '{}' is not a valid FFmpeg quantity (e.g. 15M, 320k, 5000000)",
                    label, value
                ));
            }
        }

        if self.ingestion.settle_secs > 3600 {
            return Err("ingestion.settle_secs must be <= 3600".into());
        }
        if self.ingestion.poll_secs == 0 || self.ingestion.poll_secs > 3600 {
            return Err("ingestion.poll_secs must be between 1 and 3600".into());
        }
        if self.ingestion.max_concurrency > 64 {
            return Err("ingestion.max_concurrency must be <= 64".into());
        }
        if self.ingestion.max_attempts > 10 {
            return Err("ingestion.max_attempts must be <= 10".into());
        }
        if self.ingestion.retry_delay_ms > 600_000 {
            return Err("ingestion.retry_delay_ms must be <= 600000".into());
        }
        if self.ingestion.stable_polls_min > 100 {
            return Err("ingestion.stable_polls_min must be <= 100".into());
        }

        if self.server.web_port == 0 {
            return Err("server.web_port must not be 0".into());
        }

        let valid_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_levels.contains(&self.logging.level.trim().to_lowercase().as_str()) {
            return Err(format!(
                "Invalid logging.level '{}'. Valid: {:?}",
                self.logging.level, valid_levels
            ));
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
        let valid_audio_codecs = ["aac", "pcm_s16le", "libmp3lame"];
        if !valid_audio_codecs.contains(&audio_pol.codec.trim()) {
            return Err(format!(
                "Invalid audio_policy.codec '{}'. Valid: {:?}",
                audio_pol.codec, valid_audio_codecs
            ));
        }
        if !is_valid_ffmpeg_quantity(&audio_pol.bitrate) {
            return Err(format!(
                "audio_policy.bitrate '{}' is not a valid FFmpeg quantity",
                audio_pol.bitrate
            ));
        }
        if let Some(layout) = audio_pol.channel_layout.as_deref() {
            let valid_layouts = ["mono", "stereo", "5.1", "5.1(side)", "7.1"];
            if !valid_layouts.contains(&layout.trim()) {
                return Err(format!(
                    "Invalid audio_policy.channel_layout '{}'. Valid: {:?}",
                    layout, valid_layouts
                ));
            }
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
        // Distinct, non-overlapping roots: T0-6 rejects watch == target.
        let base = std::env::temp_dir().join("pt-audio-policy-validate");
        let watch = base.join("watch");
        fs::create_dir_all(&watch).unwrap();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = base.join("target").to_string_lossy().to_string();

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

#[cfg(test)]
mod bind_tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("127.1.2.3"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("[::1]"));
        assert!(is_loopback_bind("localhost"));
        assert!(is_loopback_bind(" LocalHost "));

        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("192.168.1.10"));
        assert!(!is_loopback_bind("::"));
        assert!(!is_loopback_bind(""));
    }

    fn base_config(dir: &std::path::Path) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.paths.watch_folder = dir.join("watch").to_string_lossy().to_string();
        cfg.paths.target_folder = dir.join("target").to_string_lossy().to_string();
        fs::create_dir_all(&cfg.paths.watch_folder).unwrap();
        cfg
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pt-bind-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn non_loopback_bind_is_rejected() {
        let dir = tmp_dir("nonloop");
        let mut cfg = base_config(&dir);
        cfg.server.bind_address = "0.0.0.0".into();
        let err = cfg.validate().expect_err("0.0.0.0 must not validate");
        assert!(err.contains("loopback"), "unexpected message: {}", err);

        cfg.server.bind_address = "127.0.0.1".into();
        assert!(cfg.validate().is_ok(), "loopback bind must validate");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowed_origins_must_be_bare_origins() {
        let dir = tmp_dir("origins");
        let mut cfg = base_config(&dir);
        cfg.server.allowed_origins = vec!["localhost:5173".into()];
        assert!(cfg.validate().is_err());

        cfg.server.allowed_origins = vec!["http://localhost:5173/admin".into()];
        assert!(cfg.validate().is_err());

        cfg.server.allowed_origins = vec!["http://localhost:5173".into()];
        assert!(cfg.validate().is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pt-val-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A config that validates cleanly, so each negative test below changes
    /// exactly one thing.
    fn good_config(dir: &Path) -> AppConfig {
        let mut cfg = AppConfig::default();
        let watch = dir.join("watch");
        fs::create_dir_all(&watch).unwrap();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = dir.join("target").to_string_lossy().to_string();
        cfg
    }

    #[test]
    fn baseline_config_is_valid() {
        let dir = tmp_dir("baseline");
        assert_eq!(good_config(&dir).validate(), Ok(()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_quantity_grammar() {
        for ok in ["15M", "320k", "5000000", "1.5M", "2G", "8m", "512K"] {
            assert!(is_valid_ffmpeg_quantity(ok), "{} should be valid", ok);
        }
        for bad in [
            "",
            "M",
            "15 M",
            "15MB",
            "-5M",
            "1.2.3M",
            "15M; rm -rf /",
            "$(whoami)",
            "0x10",
            "abc",
        ] {
            assert!(!is_valid_ffmpeg_quantity(bad), "{} should be invalid", bad);
        }
    }

    #[test]
    fn relative_and_root_media_paths_are_rejected() {
        let dir = tmp_dir("paths");

        let mut cfg = good_config(&dir);
        cfg.paths.watch_folder = "media/in".into();
        assert!(cfg.validate().is_err(), "relative watch_folder");

        let mut cfg = good_config(&dir);
        cfg.paths.watch_folder = if cfg!(windows) { "C:\\" } else { "/" }.into();
        assert!(cfg.validate().is_err(), "drive root watch_folder");

        let mut cfg = good_config(&dir);
        cfg.paths.target_folder = if cfg!(windows) { "D:\\" } else { "/" }.into();
        assert!(cfg.validate().is_err(), "drive root target_folder");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_and_profile_roots_are_rejected() {
        let dir = tmp_dir("system");

        if let Ok(sysroot) = std::env::var("SystemRoot") {
            let mut cfg = good_config(&dir);
            cfg.paths.watch_folder = sysroot.clone();
            assert!(cfg.validate().is_err(), "SystemRoot watch_folder");

            let mut cfg = good_config(&dir);
            cfg.paths.target_folder = format!("{}\\Temp\\out", sysroot);
            assert!(cfg.validate().is_err(), "inside SystemRoot target_folder");
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let mut cfg = good_config(&dir);
            cfg.paths.watch_folder = profile.clone();
            assert!(cfg.validate().is_err(), "user profile root watch_folder");

            // C:\Users — the F-03 mass-deletion example.
            if let Some(parent) = Path::new(&profile).parent() {
                let mut cfg = good_config(&dir);
                cfg.paths.watch_folder = parent.to_string_lossy().to_string();
                assert!(cfg.validate().is_err(), "profile container watch_folder");
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_watch_and_target_are_rejected() {
        let dir = tmp_dir("overlap");

        // Equal.
        let mut cfg = good_config(&dir);
        cfg.paths.target_folder = cfg.paths.watch_folder.clone();
        assert!(cfg.validate().is_err(), "watch == target");

        // Target inside watch — the F-14 re-ingest loop.
        let mut cfg = good_config(&dir);
        cfg.paths.target_folder = Path::new(&cfg.paths.watch_folder)
            .join("out")
            .to_string_lossy()
            .to_string();
        assert!(cfg.validate().is_err(), "target inside watch");

        // Watch inside target.
        let mut cfg = good_config(&dir);
        let nested = dir.join("target").join("in");
        fs::create_dir_all(&nested).unwrap();
        cfg.paths.watch_folder = nested.to_string_lossy().to_string();
        assert!(cfg.validate().is_err(), "watch inside target");

        // Sibling prefixes must still be allowed: /media/in vs /media/input.
        let mut cfg = good_config(&dir);
        let sibling = dir.join("watchx");
        fs::create_dir_all(&sibling).unwrap();
        cfg.paths.watch_folder = sibling.to_string_lossy().to_string();
        cfg.paths.target_folder = dir.join("watch").to_string_lossy().to_string();
        assert!(cfg.validate().is_ok(), "sibling prefix must be allowed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_bound_strings_are_validated() {
        let dir = tmp_dir("ffstrings");

        let mut cfg = good_config(&dir);
        cfg.encoding.tune = "nonsense".into();
        assert!(cfg.validate().is_err(), "tune");

        let mut cfg = good_config(&dir);
        cfg.encoding.tune = "film".into();
        assert!(cfg.validate().is_ok(), "valid tune");

        let mut cfg = good_config(&dir);
        cfg.encoding.probesize = "lots".into();
        assert!(cfg.validate().is_err(), "probesize");

        let mut cfg = good_config(&dir);
        cfg.encoding.analyzeduration = "-1".into();
        assert!(cfg.validate().is_err(), "analyzeduration");

        let mut cfg = good_config(&dir);
        cfg.encoding.audio_bitrate = "320 kbps".into();
        assert!(cfg.validate().is_err(), "audio_bitrate");

        let mut cfg = good_config(&dir);
        cfg.profile_a.maxrate = "15M -f null -".into();
        assert!(cfg.validate().is_err(), "profile_a.maxrate");

        let mut cfg = good_config(&dir);
        cfg.profile_c.bufsize = "".into();
        assert!(cfg.validate().is_err(), "profile_c.bufsize");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingestion_and_logging_bounds() {
        let dir = tmp_dir("bounds");

        let mut cfg = good_config(&dir);
        cfg.ingestion.settle_secs = 3601;
        assert!(cfg.validate().is_err(), "settle_secs");

        let mut cfg = good_config(&dir);
        cfg.ingestion.poll_secs = 0;
        assert!(cfg.validate().is_err(), "poll_secs 0");

        let mut cfg = good_config(&dir);
        cfg.ingestion.max_concurrency = 65;
        assert!(cfg.validate().is_err(), "max_concurrency");

        let mut cfg = good_config(&dir);
        cfg.ingestion.max_attempts = 11;
        assert!(cfg.validate().is_err(), "max_attempts");

        let mut cfg = good_config(&dir);
        cfg.ingestion.retry_delay_ms = 600_001;
        assert!(cfg.validate().is_err(), "retry_delay_ms");

        let mut cfg = good_config(&dir);
        cfg.ingestion.stable_polls_min = 101;
        assert!(cfg.validate().is_err(), "stable_polls_min");

        let mut cfg = good_config(&dir);
        cfg.server.web_port = 0;
        assert!(cfg.validate().is_err(), "web_port 0");

        let mut cfg = good_config(&dir);
        cfg.logging.level = "verbose".into();
        assert!(cfg.validate().is_err(), "logging.level");

        let mut cfg = good_config(&dir);
        cfg.logging.level = "DEBUG".into();
        assert!(cfg.validate().is_ok(), "logging.level is case-insensitive");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_policy_strings_are_validated() {
        let dir = tmp_dir("audiopol");

        let mut cfg = good_config(&dir);
        let mut pol = cfg.effective_audio_policy();
        pol.codec = "flac".into();
        cfg.audio_policy = Some(pol);
        assert!(cfg.validate().is_err(), "codec");

        let mut cfg = good_config(&dir);
        let mut pol = cfg.effective_audio_policy();
        pol.bitrate = "loud".into();
        cfg.audio_policy = Some(pol);
        assert!(cfg.validate().is_err(), "bitrate");

        let mut cfg = good_config(&dir);
        let mut pol = cfg.effective_audio_policy();
        pol.channel_layout = Some("quadraphonic".into());
        cfg.audio_policy = Some(pol);
        assert!(cfg.validate().is_err(), "channel_layout");

        let mut cfg = good_config(&dir);
        let mut pol = cfg.effective_audio_policy();
        pol.channel_layout = Some("5.1(side)".into());
        cfg.audio_policy = Some(pol);
        assert!(cfg.validate().is_ok(), "valid channel_layout");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_does_not_create_the_target_directory() {
        let dir = tmp_dir("noside");
        let cfg = good_config(&dir);
        let target = PathBuf::from(&cfg.paths.target_folder);
        assert!(!target.exists());
        assert!(cfg.validate().is_ok());
        assert!(
            !target.exists(),
            "validate() must be pure; directory creation belongs to start_processing_loop"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_to_writes_atomically_and_leaves_no_temp_file() {
        let dir = tmp_dir("save");
        let cfg = good_config(&dir);
        let path = dir.join("config.toml");
        cfg.save_to(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.join("config.toml.tmp").exists());
        let reloaded: AppConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.paths.watch_folder, cfg.paths.watch_folder);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn generated_tokens_are_long_unique_and_url_safe() {
        let a = gen_api_token();
        let b = gen_api_token();
        assert_ne!(a, b, "tokens must not repeat");
        assert!(
            a.len() >= MIN_API_TOKEN_LEN,
            "token too short: {} chars",
            a.len()
        );
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be URL-safe: {}",
            a
        );
    }

    #[test]
    fn token_comparison_rejects_mismatches() {
        let t = gen_api_token();
        assert!(tokens_match(&t, &t));
        assert!(!tokens_match(&t, ""));
        assert!(!tokens_match(&t, &t[..t.len() - 1]));
        let mut wrong = t.clone();
        wrong.pop();
        wrong.push(if t.ends_with('A') { 'B' } else { 'A' });
        assert!(!tokens_match(&t, &wrong), "same length, different content");
    }

    fn token_config(dir: &Path) -> AppConfig {
        let mut cfg = AppConfig::default();
        let watch = dir.join("watch");
        fs::create_dir_all(&watch).unwrap();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = dir.join("target").to_string_lossy().to_string();
        cfg
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pt-tok-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn non_loopback_bind_requires_a_token() {
        let dir = tmp("bind");
        let mut cfg = token_config(&dir);
        cfg.server.bind_address = "0.0.0.0".into();
        let err = cfg.validate().expect_err("0.0.0.0 without a token");
        assert!(err.contains("api_token"), "unexpected message: {}", err);

        cfg.server.api_token = gen_api_token();
        assert!(
            cfg.validate().is_ok(),
            "0.0.0.0 with a token must be allowed"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn weak_or_malformed_tokens_are_rejected() {
        let dir = tmp("weak");

        let mut cfg = token_config(&dir);
        cfg.server.api_token = "short".into();
        assert!(cfg.validate().is_err(), "too short");

        let mut cfg = token_config(&dir);
        cfg.server.api_token = format!(" {} ", gen_api_token());
        assert!(cfg.validate().is_err(), "surrounding whitespace");

        let mut cfg = token_config(&dir);
        cfg.server.api_token = "a b".repeat(20);
        assert!(cfg.validate().is_err(), "contains spaces");

        let mut cfg = token_config(&dir);
        cfg.server.api_token = gen_api_token();
        assert!(cfg.validate().is_ok());

        let _ = fs::remove_dir_all(&dir);
    }
}
