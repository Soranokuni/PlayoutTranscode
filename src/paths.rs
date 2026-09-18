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

/// Canonicalize as much of `path` as exists on disk, keeping the rest verbatim.
///
/// `Path::canonicalize` is all-or-nothing: it fails if the final component is
/// missing, and the caller is then left comparing a raw path against a
/// canonicalized one. On Windows those two can differ even for the same file,
/// because the environment hands out 8.3 short names (`RUNNER~1`) while
/// canonicalization returns the long form (`runneradmin`), and because junctions
/// and mapped drives resolve to something else entirely.
///
/// That is not hypothetical: it is why a file which vanished between the watcher
/// offering it and the worker picking it up was reported as a path-traversal
/// attempt rather than a missing source. The containment check compared an
/// un-canonicalized input against a canonicalized watch root, and on any host
/// where the two spellings differ, *every* input looked like it was outside the
/// watch folder.
///
/// Walking up to the nearest ancestor that does exist — normally the watch
/// folder itself — and re-joining the remainder gives both sides the same
/// spelling, so the comparison means what it says.
pub fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return strip_verbatim_prefix(&resolved);
    }

    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path;

    loop {
        let Some(parent) = cursor.parent() else {
            // No ancestor resolved (a relative path, or a root that is gone).
            // Returning the input unchanged is the honest answer.
            return path.to_path_buf();
        };
        if let Some(name) = cursor.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(resolved) = parent.canonicalize() {
            let mut out = strip_verbatim_prefix(&resolved);
            for name in suffix.iter().rev() {
                out.push(name);
            }
            return out;
        }
        cursor = parent;
    }
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
    let dir = std::fs::canonicalize(&dir)
        .map(|c| strip_verbatim_prefix(&c))
        .unwrap_or(dir);
    if let Ok(mut slot) = DATA_DIR.write() {
        *slot = Some(dir.clone());
    }
    Ok(dir)
}

/// Drop the `\\?\` extended-length prefix Windows' `canonicalize` adds.
///
/// Not cosmetic: sqlx builds the SQLite connection string from this path, and
/// its URL parser reads `\\?\C:\...\media_assets.db?mode=rwc` as having a
/// query string starting at the `?` in the prefix, so the pool fails to open.
/// Plenty of other tools mishandle verbatim paths too, so the service keeps
/// ordinary paths everywhere and only canonicalizes to settle the spelling.
/// Comparing a canonicalized path against a non-canonicalized one -- which
/// happens whenever one side is a file that no longer exists -- also fails
/// silently, because only one of them carries the prefix. `processor` relies
/// on this for its watch-folder containment check.
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{}", rest));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
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

    // Backslash is not a path separator off Windows, so `Path::components`
    // sees `C:\Program Files\...` as a single opaque component and the
    // detection cannot fire. The function is Windows-only in meaning --
    // there is no ProgramData elsewhere -- so the test is too.
    #[test]
    #[cfg(windows)]
    fn program_files_install_uses_program_data() {
        let got = resolve_data_dir(None, None, &pf(), Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"C:\ProgramData\PlayoutTranscode"));
    }

    // Backslash is not a path separator off Windows, so `Path::components`
    // sees `C:\Program Files\...` as a single opaque component and the
    // detection cannot fire. The function is Windows-only in meaning --
    // there is no ProgramData elsewhere -- so the test is too.
    #[test]
    #[cfg(windows)]
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
    #[cfg(windows)]
    fn program_files_match_is_case_insensitive() {
        let exe = PathBuf::from(r"C:\PROGRAM FILES\PlayoutTranscode");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, PathBuf::from(r"C:\ProgramData\PlayoutTranscode"));
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pt-canon-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_existing_file_canonicalizes_normally() {
        let dir = scratch_dir("exists");
        let file = dir.join("clip.mxf");
        std::fs::write(&file, b"x").unwrap();

        let got = canonicalize_existing_prefix(&file);
        assert!(got.ends_with("clip.mxf"), "{}", got.display());
        assert_eq!(got, strip_verbatim_prefix(&file.canonicalize().unwrap()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_resolves_through_its_existing_parent() {
        // The case that broke ingest: the watcher offered a file and it was
        // gone by the time the worker looked. `canonicalize` fails outright, so
        // the old code compared an unresolved path against a resolved watch
        // root -- and on a host where the two spell the same directory
        // differently, that made every vanished file look like a traversal
        // attempt.
        let dir = scratch_dir("missing");
        let missing = dir.join("gone.mxf");
        assert!(!missing.exists());

        let got = canonicalize_existing_prefix(&missing);
        let parent = canonicalize_existing_prefix(&dir);

        assert!(
            got.starts_with(&parent),
            "a missing file must still resolve inside its parent: {} vs {}",
            got.display(),
            parent.display()
        );
        assert!(got.ends_with("gone.mxf"), "{}", got.display());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_missing_components_still_resolve() {
        let dir = scratch_dir("deep");
        let deep = dir.join("a").join("b").join("c.mxf");

        let got = canonicalize_existing_prefix(&deep);
        assert!(got.starts_with(canonicalize_existing_prefix(&dir)));
        assert!(got.ends_with("c.mxf"), "{}", got.display());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The containment check the processor performs, in miniature.
    ///
    /// This is the assertion that actually matters: a file inside the watch
    /// folder must compare as inside it whether or not it still exists, and a
    /// file outside must not.
    #[test]
    fn containment_holds_for_present_and_absent_files_alike() {
        let watch = scratch_dir("watch");
        let outside = scratch_dir("outside");

        let present = watch.join("present.mxf");
        std::fs::write(&present, b"x").unwrap();
        let absent = watch.join("absent.mxf");
        let elsewhere = outside.join("elsewhere.mxf");
        std::fs::write(&elsewhere, b"x").unwrap();

        let root = canonicalize_existing_prefix(&watch);

        assert!(canonicalize_existing_prefix(&present).starts_with(&root));
        assert!(
            canonicalize_existing_prefix(&absent).starts_with(&root),
            "a vanished file is still a file in the watch folder"
        );
        assert!(
            !canonicalize_existing_prefix(&elsewhere).starts_with(&root),
            "and the guard must still reject a genuine outsider"
        );

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Windows hands out 8.3 short names through the environment, and
    /// `canonicalize` returns the long form. Two spellings of one directory is
    /// precisely what made the old comparison fail on a GitHub runner, where
    /// `%TEMP%` is `C:\Users\RUNNER~1\...`.
    #[test]
    #[cfg(windows)]
    fn a_short_name_and_its_long_form_resolve_to_the_same_path() {
        let dir = scratch_dir("shortname");
        let long = canonicalize_existing_prefix(&dir);

        // Ask Windows for the short form of this directory. If the volume has
        // 8.3 names disabled there is nothing to test, so skip rather than
        // assert something untrue.
        let short = short_path_name(&dir);
        let Some(short) = short else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        if short == dir {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let missing_via_short = PathBuf::from(&short).join("gone.mxf");
        let resolved = canonicalize_existing_prefix(&missing_via_short);

        assert!(
            resolved.starts_with(&long),
            "a path reached by its short name must resolve under the long form: \
             {} should start with {}",
            resolved.display(),
            long.display()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two spellings of one directory, built with a junction.
    ///
    /// This is the regression test for the bug CI caught. Under the old code
    /// the watch root resolved to the junction's *target* while a missing input
    /// under the junction kept the junction's own path, so the containment
    /// check compared two unrelated-looking strings and rejected a file that
    /// was plainly inside the watch folder.
    ///
    /// A junction is the reproducible form of what a GitHub Windows runner
    /// produces with 8.3 short names, and unlike a symlink it needs no
    /// privileges.
    #[test]
    #[cfg(windows)]
    fn a_file_reached_through_a_junction_is_still_inside_the_watch_folder() {
        let real = scratch_dir("junction-real");
        let link = real.with_file_name(format!(
            "{}-link",
            real.file_name().unwrap().to_string_lossy()
        ));

        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output();

        // Junctions are unavailable on some filesystems. Skip rather than
        // assert something untrue.
        let Ok(out) = made else {
            let _ = std::fs::remove_dir_all(&real);
            return;
        };
        if !out.status.success() || !link.exists() {
            let _ = std::fs::remove_dir_all(&real);
            return;
        }

        // The watch folder is configured by its junction path, and the file
        // the watcher offered has since vanished.
        let watch_root = canonicalize_existing_prefix(&link);
        let vanished = link.join("gone.mxf");
        let resolved = canonicalize_existing_prefix(&vanished);

        assert!(
            resolved.starts_with(&watch_root),
            "a vanished file under the configured watch folder must compare as \
             inside it: {} should start with {}",
            resolved.display(),
            watch_root.display()
        );

        // And the guard still rejects a genuine outsider.
        let outside = scratch_dir("junction-outside").join("elsewhere.mxf");
        assert!(!canonicalize_existing_prefix(&outside).starts_with(&watch_root));

        let _ = std::fs::remove_dir_all(&link);
        let _ = std::fs::remove_dir_all(&real);
    }

    #[cfg(windows)]
    fn short_path_name(path: &Path) -> Option<PathBuf> {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut buf = vec![0u16; 1024];
        let len = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                wide.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        };
        if len == 0 || len as usize >= buf.len() {
            return None;
        }
        buf.truncate(len as usize);
        Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
    }

    #[test]
    fn the_verbatim_prefix_is_stripped() {
        // sqlx's SQLite URL parser treats the `?` in `\\?\` as a query string
        // and refuses to open the database, so this must never survive.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\ProgramData\PlayoutTranscode")),
            PathBuf::from(r"C:\ProgramData\PlayoutTranscode")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\nas01\media\ingest")),
            PathBuf::from(r"\\nas01\media\ingest")
        );
        // An ordinary path is returned untouched.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"D:\PlayoutTranscode")),
            PathBuf::from(r"D:\PlayoutTranscode")
        );
    }

    #[test]
    fn a_program_files_lookalike_is_not_matched() {
        // `Program Files Backup` is an ordinary directory, not an install root.
        let exe = PathBuf::from(r"D:\Program Files Backup\PlayoutTranscode");
        let got = resolve_data_dir(None, None, &exe, Some(r"C:\ProgramData"));
        assert_eq!(got, exe);
    }
}
