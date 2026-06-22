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
}

fn bin_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_dir.join("bin")
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

fn resolve_tool(name: &str, bin: &Path, check_path: bool) -> Option<PathBuf> {
    let exe_name = executable_name(name);
    let candidate = bin.join(&exe_name);
    if candidate.exists() {
        return Some(candidate);
    }
    if check_path {
        for dir in std::env::var("PATH").unwrap_or_default().split(';') {
            let p = PathBuf::from(dir).join(&exe_name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

pub fn audit_toolchain() -> (ToolPaths, ToolchainStatus) {
    let bin = bin_dir();
    let ffmpeg = resolve_tool("ffmpeg", &bin, true);
    let ffprobe = resolve_tool("ffprobe", &bin, true);
    let bundled = ffmpeg.as_ref().is_some_and(|p| p.starts_with(&bin))
        || ffprobe.as_ref().is_some_and(|p| p.starts_with(&bin));

    let tools = ToolPaths {
        ffmpeg: ffmpeg.clone().unwrap_or_else(|| bin.join(executable_name("ffmpeg"))),
        ffprobe: ffprobe.clone().unwrap_or_else(|| bin.join(executable_name("ffprobe"))),
    };

    let status = ToolchainStatus {
        ffmpeg_found: ffmpeg.is_some(),
        ffprobe_found: ffprobe.is_some(),
        ffmpeg_version: ffmpeg.as_ref().and_then(|p| run_version(p)),
        ffprobe_version: ffprobe.as_ref().and_then(|p| run_version(p)),
        bundled,
        bin_dir: bin.to_string_lossy().into_owned(),
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

pub fn download_ffmpeg() -> Result<ToolPaths, String> {
    let bin = bin_dir();
    if let Err(e) = fs::create_dir_all(&bin) {
        return Err(format!("Failed to create bin directory: {}", e));
    }

    let arch = get_arch();
    let url = format!(
        "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
    );

    tracing::info!("Downloading FFmpeg from {} (arch: {})", url, arch);
    let zip_path = bin.join("ffmpeg-temp.zip");

    {
        let resp = reqwest::blocking::get(&url)
            .map_err(|e| format!("FFmpeg download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("FFmpeg download returned HTTP {}", resp.status()));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| format!("FFmpeg download read failed: {}", e))?;
        fs::write(&zip_path, &bytes)
            .map_err(|e| format!("Failed to write FFmpeg zip: {}", e))?;
    }

    tracing::info!("Extracting FFmpeg...");
    {
        let file = fs::File::open(&zip_path)
            .map_err(|e| format!("Failed to open FFmpeg zip: {}", e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Failed to read FFmpeg zip: {}", e))?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)
                .map_err(|e| format!("Zip entry {}: {}", i, e))?;
            let name = entry.name().to_string();

            if let Some(rel) = name.strip_prefix(|c: char| c != '/') {
                let rel = rel.to_string();
                if let Some(inner) = rel.split_once('/').and_then(|(_, rest)| {
                    if rest.contains("bin/") || rest.contains("ffprobe") || rest.contains("ffmpeg") {
                        Some(rest)
                    } else {
                        None
                    }
                }) {
                    let dest = bin.join(
                        Path::new(inner)
                            .file_name()
                            .unwrap_or_default(),
                    );
                    if let Some(parent) = dest.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                        if !entry.is_dir() {
                            let mut out_file = fs::File::create(&dest)
                                .unwrap_or_else(|_| std::fs::File::create(dest.with_extension("tmp")).unwrap());
                            if let Err(e) = std::io::copy(&mut entry, &mut out_file) {
                                tracing::warn!("Failed to extract {}: {}", inner, e);
                            }
                        }
                }
            }
        }
    }

    let _ = fs::remove_file(&zip_path);

    let (tools, status) = audit_toolchain();
    if !status.ffmpeg_found || !status.ffprobe_found {
        return Err("FFmpeg download succeeded but binaries not found after extraction".into());
    }

    tracing::info!(
        "FFmpeg bootstrapping complete: ffmpeg={}, ffprobe={}",
        status.ffmpeg_version.as_deref().unwrap_or("unknown"),
        status.ffprobe_version.as_deref().unwrap_or("unknown")
    );

    Ok(tools)
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


