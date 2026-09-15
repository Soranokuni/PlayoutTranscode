use crate::config::ToolchainPolicy;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone)]
pub struct ToolPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolchainStatus {
    pub ffmpeg_found: bool,
    pub ffprobe_found: bool,
    pub ffmpeg_version: Option<String>,
    pub ffprobe_version: Option<String>,
    pub bundled: bool,
    pub bin_dir: String,
    /// SHA-256 of the resolved binaries, so an operator can spot a swap.
    pub ffmpeg_sha256: Option<String>,
    pub ffprobe_sha256: Option<String>,
    /// The path actually resolved, which may come from `toolchain_policy`.
    pub ffmpeg_path: Option<String>,
}

/// Process-wide toolchain policy, set once from the loaded config.
///
/// `audit_toolchain` is called from eight places that have no config in scope;
/// threading the policy through all of them would be pure noise. Toolchain
/// paths are a restart-level setting anyway.
static TOOLCHAIN_POLICY: std::sync::RwLock<Option<ToolchainPolicy>> =
    std::sync::RwLock::new(None);

/// Install the configured toolchain policy. Call once, early in startup.
pub fn set_toolchain_policy(policy: ToolchainPolicy) {
    match TOOLCHAIN_POLICY.write() {
        Ok(mut slot) => *slot = Some(policy),
        Err(e) => tracing::error!("toolchain policy lock poisoned: {}", e),
    }
}

fn current_policy() -> ToolchainPolicy {
    TOOLCHAIN_POLICY
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_default()
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn bin_dir() -> PathBuf {
    exe_dir().join("bin")
}

/// Directories searched for the toolchain, in order, after any explicit path
/// from `toolchain_policy`.
fn search_dirs() -> Vec<PathBuf> {
    let exe = exe_dir();
    vec![
        exe.join("bin"),
        // The installer's layout.
        exe.join("Requirements").join("ffmpeg").join("bin"),
    ]
}

fn executable_name(base: &str) -> String {
    format!("{}{}", base, std::env::consts::EXE_SUFFIX)
}

fn run_version(tool: &Path) -> Option<String> {
    let mut cmd = Command::new(tool);
    cmd.arg("-version");
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().next().map(|s| s.to_string())
    } else {
        None
    }
}

/// Locate `name`, honouring an explicit configured path first.
///
/// The `PATH` fallback this used to have was a binary-planting primitive: any
/// writable directory earlier in `PATH` let a local user supply the
/// `ffmpeg.exe` that the service — LocalSystem in the documented deployment —
/// then executed (F-04). Only directories we control are searched now.
pub fn resolve_tool_in(
    name: &str,
    configured: Option<&str>,
    dirs: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(raw) = configured {
        let raw = raw.trim();
        if !raw.is_empty() {
            let p = PathBuf::from(raw);
            if !p.is_absolute() {
                tracing::warn!(
                    "toolchain_policy path for {} must be absolute, ignoring: {}",
                    name,
                    raw
                );
            } else if p.is_file() {
                return Some(p);
            } else {
                tracing::warn!(
                    "toolchain_policy path for {} does not exist: {}",
                    name,
                    raw
                );
            }
        }
    }
    let exe_name = executable_name(name);
    for dir in dirs {
        let candidate = dir.join(&exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// SHA-256 of a file, lower-case hex. `None` if it cannot be read.
pub fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

pub fn audit_toolchain() -> (ToolPaths, ToolchainStatus) {
    let policy = current_policy();
    let dirs = search_dirs();
    let bin = bin_dir();

    let ffmpeg = resolve_tool_in("ffmpeg", policy.ffmpeg_path.as_deref(), &dirs);
    let ffprobe = resolve_tool_in("ffprobe", policy.ffprobe_path.as_deref(), &dirs);
    let bundled = ffmpeg.as_ref().is_some_and(|p| p.starts_with(&bin))
        || ffprobe.as_ref().is_some_and(|p| p.starts_with(&bin));

    let tools = ToolPaths {
        ffmpeg: ffmpeg
            .clone()
            .unwrap_or_else(|| bin.join(executable_name("ffmpeg"))),
        ffprobe: ffprobe
            .clone()
            .unwrap_or_else(|| bin.join(executable_name("ffprobe"))),
    };

    let status = ToolchainStatus {
        ffmpeg_found: ffmpeg.is_some(),
        ffprobe_found: ffprobe.is_some(),
        ffmpeg_version: ffmpeg.as_ref().and_then(|p| run_version(p)),
        ffprobe_version: ffprobe.as_ref().and_then(|p| run_version(p)),
        bundled,
        bin_dir: bin.to_string_lossy().into_owned(),
        // Lets an operator spot a swapped binary from /api/toolchain.
        ffmpeg_sha256: ffmpeg.as_ref().and_then(|p| file_sha256(p)),
        ffprobe_sha256: ffprobe.as_ref().and_then(|p| file_sha256(p)),
        ffmpeg_path: ffmpeg.as_ref().map(|p| p.to_string_lossy().into_owned()),
    };

    (tools, status)
}

pub fn ensure_toolchain() -> Result<ToolPaths, String> {
    let (tools, status) = audit_toolchain();
    if !status.ffmpeg_found || !status.ffprobe_found {
        return Err(
            "FFmpeg/FFprobe not found. Use the 'Download FFmpeg' button in the GUI or run 'PlayoutTranscode setup' to install."
                .into(),
        );
    }
    tracing::info!(
        "FFmpeg toolchain ready: ffmpeg={:?} ffprobe={:?}",
        tools.ffmpeg,
        tools.ffprobe
    );
    Ok(tools)
}

/// Where the release archive is fetched from when the operator asks for an
/// automatic install.
const FFMPEG_DOWNLOAD_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub fn download_ffmpeg() -> Result<ToolPaths, String> {
    let policy = current_policy();
    let expected = policy.download_sha256.trim().to_lowercase();

    // An unpinned executable download that the service then runs as a
    // privileged account is not acceptable on a broadcast host (F-04). The
    // operator must state the digest they expect, or install manually.
    if expected.is_empty() {
        return Err(
            "toolchain_policy.download_sha256 is not set. Automatic FFmpeg download is \
             disabled without a pinned digest: set the expected SHA-256 of the release \
             archive in config.toml, or install FFmpeg manually into the bin directory."
                .into(),
        );
    }
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("toolchain_policy.download_sha256 must be 64 hex characters".into());
    }

    let bin = bin_dir();
    if let Err(e) = fs::create_dir_all(&bin) {
        return Err(format!("Failed to create bin directory: {}", e));
    }

    tracing::info!(
        "Downloading FFmpeg from {} (arch: {})",
        FFMPEG_DOWNLOAD_URL,
        get_arch()
    );
    let zip_path = bin.join("ffmpeg-temp.zip");

    {
        // A default reqwest client times out at 30 s, which fails on any slow
        // link for a ~90 MB archive (F-24). Stream to disk rather than holding
        // the whole archive in RAM.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(900))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;
        let mut resp = client
            .get(FFMPEG_DOWNLOAD_URL)
            .send()
            .map_err(|e| format!("FFmpeg download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("FFmpeg download returned HTTP {}", resp.status()));
        }
        let mut out = fs::File::create(&zip_path)
            .map_err(|e| format!("Failed to create FFmpeg zip: {}", e))?;
        std::io::copy(&mut resp, &mut out)
            .map_err(|e| format!("FFmpeg download read failed: {}", e))?;
    }

    let actual = file_sha256(&zip_path)
        .ok_or_else(|| "Failed to hash the downloaded archive".to_string())?;
    if actual != expected {
        let _ = fs::remove_file(&zip_path);
        return Err(format!(
            "FFmpeg archive digest mismatch: expected {}, got {}. The download was discarded.",
            expected, actual
        ));
    }
    tracing::info!("FFmpeg archive digest verified: {}", actual);

    tracing::info!("Extracting FFmpeg...");
    {
        let file =
            fs::File::open(&zip_path).map_err(|e| format!("Failed to open FFmpeg zip: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("Failed to read FFmpeg zip: {}", e))?;

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Zip entry {}: {}", i, e))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let Some(dest) = extraction_target(&bin, &name) else {
                continue;
            };
            let mut out_file = fs::File::create(&dest)
                .map_err(|e| format!("Failed to create {}: {}", dest.display(), e))?;
            if let Err(e) = std::io::copy(&mut entry, &mut out_file) {
                let _ = fs::remove_file(&zip_path);
                return Err(format!("Failed to extract {}: {}", name, e));
            }
        }
    }

    let _ = fs::remove_file(&zip_path);

    let (tools, status) = audit_toolchain();
    if !status.ffmpeg_found || !status.ffprobe_found {
        return Err("FFmpeg download succeeded but binaries not found after extraction".into());
    }

    tracing::info!(
        "FFmpeg bootstrapping complete: ffmpeg={}, ffprobe={} (sha256 {})",
        status.ffmpeg_version.as_deref().unwrap_or("unknown"),
        status.ffprobe_version.as_deref().unwrap_or("unknown"),
        status.ffmpeg_sha256.as_deref().unwrap_or("unknown"),
    );

    Ok(tools)
}

/// Decide where a zip entry is written, or `None` to skip it.
///
/// Only the final path component is ever used, so a crafted entry name like
/// `../../evil.exe` or an absolute path lands in `bin/` as `evil.exe` and can
/// never escape it. Only files that look like the toolchain are taken.
pub fn extraction_target(bin: &Path, entry_name: &str) -> Option<PathBuf> {
    let file_name = Path::new(entry_name.replace('\\', "/").trim_end_matches('/'))
        .file_name()?
        .to_owned();
    let lower = file_name.to_string_lossy().to_ascii_lowercase();
    let is_tool = lower.starts_with("ffmpeg")
        || lower.starts_with("ffprobe")
        || lower.starts_with("ffplay");
    if !is_tool {
        return None;
    }
    // Only take things out of the archive's own `bin/` directory, or entries
    // that are already bare tool names.
    let normalised = entry_name.replace('\\', "/");
    if !normalised.contains("/bin/") && normalised.contains('/') {
        return None;
    }
    Some(bin.join(file_name))
}

fn get_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub update_available: bool,
    pub current_version: Option<String>,
    pub warning: Option<String>,
}

static UPDATE_WARNING: &str = "Warning: Upgrading a verified, stable broadcast toolchain is NOT recommended for production environments unless security patches are strictly required.";

pub fn check_ffmpeg_update() -> UpdateCheckResult {
    let (_, status) = audit_toolchain();
    let current = status.ffmpeg_version.clone();
    UpdateCheckResult {
        update_available: false,
        current_version: current,
        warning: Some(UPDATE_WARNING.to_string()),
    }
}

#[cfg(test)]
mod toolchain_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pt-tool-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch_exe(dir: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(executable_name(name));
        fs::write(&p, b"not really an executable").unwrap();
        p
    }

    #[test]
    fn path_is_never_searched() {
        let dir = tmp("path");
        // A writable directory on PATH containing ffmpeg.exe was the
        // binary-planting primitive in F-04.
        let planted = dir.join("planted");
        touch_exe(&planted, "ffmpeg");

        let empty_bin = dir.join("bin");
        fs::create_dir_all(&empty_bin).unwrap();

        let resolved = resolve_tool_in("ffmpeg", None, &[empty_bin]);
        assert!(
            resolved.is_none(),
            "an ffmpeg on PATH must not be resolved, got {:?}",
            resolved
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_dir_is_searched_then_installer_layout() {
        let dir = tmp("order");
        let bin = dir.join("bin");
        let requirements = dir.join("Requirements").join("ffmpeg").join("bin");

        // Only the installer layout has it.
        let installed = touch_exe(&requirements, "ffprobe");
        fs::create_dir_all(&bin).unwrap();
        let found = resolve_tool_in("ffprobe", None, &[bin.clone(), requirements.clone()])
            .expect("should fall back to the installer layout");
        assert_eq!(found, installed);

        // bin/ wins when both exist.
        let bundled = touch_exe(&bin, "ffprobe");
        let found = resolve_tool_in("ffprobe", None, &[bin, requirements]).expect("found");
        assert_eq!(found, bundled);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_absolute_path_wins() {
        let dir = tmp("configured");
        let bin = dir.join("bin");
        touch_exe(&bin, "ffmpeg");
        let custom = touch_exe(&dir.join("custom"), "ffmpeg");

        let found = resolve_tool_in("ffmpeg", Some(&custom.to_string_lossy()), std::slice::from_ref(&bin))
            .expect("configured path");
        assert_eq!(found, custom, "toolchain_policy.ffmpeg_path must win");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_relative_or_missing_path_falls_back() {
        let dir = tmp("badconfig");
        let bin = dir.join("bin");
        let bundled = touch_exe(&bin, "ffmpeg");

        // Relative paths are refused: they would resolve against the process
        // working directory, which the service does not control.
        let found = resolve_tool_in("ffmpeg", Some("bin/ffmpeg.exe"), std::slice::from_ref(&bin));
        assert_eq!(found.as_ref(), Some(&bundled));

        let missing = dir.join("nope").join("ffmpeg.exe");
        let found = resolve_tool_in("ffmpeg", Some(&missing.to_string_lossy()), &[bin]);
        assert_eq!(found.as_ref(), Some(&bundled));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_entry_names_cannot_escape_the_bin_directory() {
        let bin = Path::new(if cfg!(windows) { r"C:\app\bin" } else { "/app/bin" });

        // Traversal and absolute entry names collapse to the file name.
        for evil in [
            "../../ffmpeg.exe",
            "ffmpeg-7.0/bin/../../../ffmpeg.exe",
            "/etc/bin/ffmpeg",
        ] {
            if let Some(dest) = extraction_target(bin, evil) {
                assert_eq!(dest.parent(), Some(bin), "{} escaped to {:?}", evil, dest);
                assert_eq!(
                    dest.file_name().unwrap().to_string_lossy(),
                    Path::new(evil).file_name().unwrap().to_string_lossy()
                );
            }
        }

        // Normal entries land in bin/.
        let dest = extraction_target(bin, "ffmpeg-7.0-essentials_build/bin/ffprobe.exe")
            .expect("tool entry must be taken");
        assert_eq!(dest, bin.join("ffprobe.exe"));

        // Non-tool entries are skipped entirely, including a planted payload.
        assert!(extraction_target(bin, "ffmpeg-7.0/bin/evil.exe").is_none());
        assert!(extraction_target(bin, "ffmpeg-7.0/LICENSE").is_none());
        assert!(extraction_target(bin, "ffmpeg-7.0/doc/ffmpeg.html").is_none());
    }

    #[test]
    fn download_refuses_without_a_pinned_digest() {
        set_toolchain_policy(ToolchainPolicy::default());
        let err = download_ffmpeg().expect_err("must refuse an unpinned download");
        assert!(
            err.contains("download_sha256"),
            "unexpected error: {}",
            err
        );

        set_toolchain_policy(ToolchainPolicy {
            download_sha256: "not-a-digest".into(),
            ..Default::default()
        });
        let err = download_ffmpeg().expect_err("must refuse a malformed digest");
        assert!(err.contains("64 hex"), "unexpected error: {}", err);

        set_toolchain_policy(ToolchainPolicy::default());
    }

    #[test]
    fn file_sha256_matches_a_known_vector() {
        let dir = tmp("sha");
        let f = dir.join("empty");
        fs::write(&f, b"").unwrap();
        assert_eq!(
            file_sha256(&f).as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );

        let f = dir.join("abc");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(
            file_sha256(&f).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );

        assert!(file_sha256(&dir.join("missing")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
