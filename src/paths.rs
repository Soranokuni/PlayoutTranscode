//! Resolution of the service's writable data directory (T2-2).
//!
//! Before this module every mutable file the service owns — `config.toml`, the
//! SQLite registry, the downloaded FFmpeg toolchain — lived next to the
//! executable. That is fine for the portable/dev layout but wrong for the
//! documented production deployment: the installer puts the exe under
//! `C:\Program Files\...`, where `NT AUTHORITY\LocalService` (the account
//! T2-1 installs the service under) has read+execute and nothing more. The
//! service would start and then fail on the first write.
//!
//! Resolution order, highest priority first:
//!
//! 1. the `--data-dir` CLI flag,
//! 2. the `PLAYOUT_TRANSCODE_DATA` environment variable,
//! 3. `%ProgramData%\PlayoutTranscode` when the executable lives under a
//!    `Program Files` tree,
//! 4. the executable's own directory — today's portable behaviour, which is
//!    what dev builds and `target\debug\deps\` test binaries keep getting.
//!
//! The resolved directory is set once at startup by the binary and read
//! everywhere else through [`data_dir`]. Nothing re-resolves it per call: a
//! service that changed where it writes half-way through a run would be far
//! worse than one that writes to the wrong place consistently.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

static DATA_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The environment variable consulted when `--data-dir` is absent.
pub const DATA_DIR_ENV: &str = "PLAYOUT_TRANSCODE_DATA";

/// Pure resolution, with every input injected so it can be tested on any host.
///
/// `program_data` is the value of `%ProgramData%` (`None` when unset, which is
/// always the case off Windows). `exe_dir` is the directory holding the running
/// executable.
pub fn resolve_data_dir(
    cli: Option<&str>,
    env: Option<&str>,
    exe_dir: &Path,
    program_data: Option<&str>,
) -> PathBuf {
    if let Some(p) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(p) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    if is_under_program_files(exe_dir) {
        if let Some(pd) = program_data.map(str::trim).filter(|s| !s.is_empty()) {
            return Path::new(pd).join("PlayoutTranscode");
        }
    }
    exe_dir.to_path_buf()
}

/// True when any component of `dir` is a `Program Files` directory.
///
/// Matches `Program Files`, `Program Files (x86)` and the localised-install
/// `ProgramFiles` spelling, case-insensitively, because that is the whole set
/// of places the installer can land and the only signal available without
/// asking the OS for a known-folder path.
fn is_under_program_files(dir: &Path) -> bool {
    // Split on both separators rather than using `Path::components`: a
    // Windows-shaped path is also evaluated by the Linux CI build, where `\`
    // is an ordinary character and the whole string would be one component.
    dir.to_string_lossy().split(['/', '\\']).any(|seg| {
        let name = seg.trim().to_ascii_lowercase();
        name == "program files" || name == "program files (x86)" || name == "programfiles"
    })
}

/// Resolve from the real process environment. Called once, by the binary.
pub fn resolve_data_dir_from_env(cli: Option<&str>) -> PathBuf {
    let exe_dir = exe_dir();
    let env = std::env::var(DATA_DIR_ENV).ok();
    let program_data = std::env::var("ProgramData").ok();
    resolve_data_dir(cli, env.as_deref(), &exe_dir, program_data.as_deref())
}

/// The directory holding the running executable, or `.` if it cannot be found.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Install the resolved data directory and create it.
///
/// Returns an error if the directory cannot be created: continuing would mean
/// silently falling back to a path the operator did not choose, which is how
/// "the service ran but wrote nothing anyone could find" happens.
pub fn set_data_dir(dir: PathBuf) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create data directory: {}", e))?;
    // Resolve symlinks/8.3 names so comparisons elsewhere (media-root
    // validation) see the same spelling the filesystem uses.
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    if let Ok(mut slot) = DATA_DIR.write() {
        *slot = Some(dir.clone());
    }
    Ok(dir)
}

/// The resolved data directory, falling back to the executable's directory when
/// nothing has been set — which is the case in unit tests and in any tool that
/// links the library without going through `main`.
pub fn data_dir() -> PathBuf {
    DATA_DIR
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(exe_dir)
}

/// `<data_dir>/config.toml`.
pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// `<data_dir>/media_assets.db`.
pub fn database_path() -> PathBuf {
    data_dir().join("media_assets.db")
}

/// `<data_dir>/bin` — where a downloaded toolchain is installed.
pub fn toolchain_bin_dir() -> PathBuf {
    data_dir().join("bin")
}

/// `<data_dir>/logs` — the sink T2-3 writes rotated logs into.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf() -> PathBuf {
        PathBuf::from(r"C:\Program Files\PlayoutTranscode")
    }

    #[test]
    fn cli_flag_wins_over_everything() {
        let got = resolve_data_dir(
            Some(r"D:\data"),
            Some(r"E:\env"),
            &pf(),
            Some(r"C:\ProgramData"),
        );
        assert_eq!(got, PathBuf::from(r"D:\data"));
    }

    #[test]
    fn env_wins_when_no_cli_flag() {
        let got = resolve_data_dir(None, Some(r"E:\env"), &pf(), Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"E:\env"));
    }

    #[test]
    fn program_files_install_uses_program_data() {
        let got = resolve_data_dir(None, None, &pf(), Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"C:\ProgramData\PlayoutTranscode"));
    }

    #[test]
    fn program_files_x86_is_recognised() {
        let exe = PathBuf::from(r"C:\Program Files (x86)\PlayoutTranscode");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"C:\ProgramData\PlayoutTranscode"));
    }

    #[test]
    fn portable_install_stays_next_to_the_exe() {
        let exe = PathBuf::from(r"D:\PlayoutTranscode\target\debug");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, exe);
    }

    #[test]
    fn program_files_without_program_data_falls_back_to_the_exe_dir() {
        let got = resolve_data_dir(None, None, &pf(), None);
        assert_eq!(got, pf());
    }

    #[test]
    fn blank_cli_and_env_values_are_ignored() {
        let exe = PathBuf::from(r"D:\portable");
        let got = resolve_data_dir(Some("   "), Some(""), &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, exe);
    }

    #[test]
    fn program_files_match_is_case_insensitive() {
        let exe = PathBuf::from(r"C:\PROGRAM FILES\PlayoutTranscode");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"C:\ProgramData\PlayoutTranscode"));
    }

    #[test]
    fn a_program_files_lookalike_is_not_matched() {
        // `Program Files Backup` is an ordinary directory, not an install root.
        let exe = PathBuf::from(r"D:\Program Files Backup\PlayoutTranscode");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, exe);
    }
}
