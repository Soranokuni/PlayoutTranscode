//! Single-instance guard for the service's data directory (T2-5, F-17).
//!
//! Two PlayoutTranscode processes pointed at the same data directory are not a
//! configuration mistake anyone notices quickly: both open the same SQLite
//! registry, both run a watcher over the same folder, and both dispatch the
//! same file — the losing encode fails on a locked output, or worse, the two
//! publish over each other. The obvious way to reach it is an operator starting
//! the portable build while the Windows service is running, and the symptom is
//! a stream of unexplained ingest failures.
//!
//! The guard is a file, `playout-transcode.lock`, created exclusively in the
//! data directory and holding the owning process id. It lives in the **data**
//! directory, not next to the executable: since T2-2 those differ, and the data
//! directory is the thing two processes actually contend over. Two installs
//! with separate data directories are a supported layout and are not blocked.
//!
//! It is advisory. A lock file whose process is gone — a crash, a hard kill —
//! is stale and is taken over, with a log line. That is the common case after
//! an unclean stop, and refusing to start until an operator deletes a file
//! would be worse than the problem.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The lock file's name inside the data directory.
pub const LOCK_FILE_NAME: &str = "playout-transcode.lock";

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another live process owns it.
    Held { pid: u32, path: PathBuf },
    /// The lock file could not be created or read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held { pid, path } => write!(
                f,
                "another PlayoutTranscode instance (pid {}) is already using this data \
                 directory; its lock is {}. Stop that instance, or start this one with a \
                 different --data-dir.",
                pid,
                path.display()
            ),
            LockError::Io { path, source } => write!(
                f,
                "cannot manage the instance lock {}: {}",
                path.display(),
                source
            ),
        }
    }
}

impl std::error::Error for LockError {}

/// A held single-instance lock. Releases on drop.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    pid: u32,
}

impl InstanceLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Only remove it if it is still ours. A stale takeover elsewhere could
        // have rewritten it, and deleting someone else's lock reintroduces
        // exactly the concurrency this module prevents.
        if read_pid(&self.path) == Some(self.pid) {
            if let Err(e) = std::fs::remove_file(&self.path) {
                tracing::warn!(
                    "Could not remove the instance lock {}: {}",
                    self.path.display(),
                    e
                );
            }
        }
    }
}

/// Take the lock in `dir` for the current process.
pub fn acquire(dir: &Path) -> Result<InstanceLock, LockError> {
    acquire_with(dir, std::process::id(), process_is_alive)
}

/// Injectable form: `pid` is the owner to record and `alive` decides whether an
/// existing owner still exists. Both are parameters so the stale-takeover and
/// refusal paths are testable without spawning processes.
pub fn acquire_with(
    dir: &Path,
    pid: u32,
    alive: impl Fn(u32) -> bool,
) -> Result<InstanceLock, LockError> {
    let path = dir.join(LOCK_FILE_NAME);

    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(LockError::Io { path, source: e });
    }

    // Two attempts at most: one to create, and — if an existing lock turns out
    // to be stale and we remove it — one more. A third would mean another
    // process is racing us for the same stale lock, and that process winning is
    // the correct outcome.
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                if let Err(e) = write!(f, "{}", pid) {
                    let _ = std::fs::remove_file(&path);
                    return Err(LockError::Io { path, source: e });
                }
                let _ = f.flush();
                return Ok(InstanceLock { path, pid });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt == 1 {
                    return Err(LockError::Io { path, source: e });
                }
                let owner = read_pid(&path);
                match owner {
                    Some(owner) if owner != pid && alive(owner) => {
                        return Err(LockError::Held { pid: owner, path })
                    }
                    _ => {
                        // Stale (unreadable, empty, our own pid, or a dead
                        // owner): take it over and say so, because a stale lock
                        // is evidence of an unclean stop.
                        tracing::warn!(
                            "Taking over a stale instance lock at {} (previous owner {}); the \
                             last run did not shut down cleanly",
                            path.display(),
                            owner
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "unknown".into())
                        );
                        if let Err(e) = std::fs::remove_file(&path) {
                            return Err(LockError::Io { path, source: e });
                        }
                    }
                }
            }
            Err(e) => return Err(LockError::Io { path, source: e }),
        }
    }

    unreachable!("the loop above either returns or removes the stale lock")
}

fn read_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    std::fs::File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse::<u32>().ok()
}

/// Does a process with this id exist?
///
/// A false positive (the id was recycled onto an unrelated process) refuses a
/// start that would have been fine; a false negative allows a second instance.
/// Neither is free, but the lock is advisory and the takeover path logs loudly,
/// so a wrong answer is always visible in the log.
#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Either it is gone, or we are not allowed to look. Under
            // LocalService the latter is possible for another account's
            // process, but the lock we care about was written by a sibling
            // PlayoutTranscode, which runs as the same account.
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    // No libc dependency in this crate; `kill -0` is the same probe the FFmpeg
    // teardown path already shells out for.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pt-lock-{}-{}-{}",
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
    fn a_first_instance_takes_the_lock_and_writes_its_pid() {
        let dir = tmpdir("first");
        let lock = acquire_with(&dir, 4242, |_| true).expect("first acquire");
        assert_eq!(read_pid(lock.path()), Some(4242));
        drop(lock);
        // Released on drop, so the next process can start.
        assert!(!dir.join(LOCK_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_live_instance_is_refused() {
        let dir = tmpdir("second");
        let _held = acquire_with(&dir, 4242, |_| true).expect("first acquire");
        let err = acquire_with(&dir, 5353, |pid| pid == 4242).expect_err("must refuse");
        match err {
            LockError::Held { pid, .. } => assert_eq!(pid, 4242),
            other => panic!("expected Held, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_lock_from_a_dead_process_is_taken_over() {
        let dir = tmpdir("stale");
        // Simulate a crash: the lock file survives, its owner does not.
        std::fs::write(dir.join(LOCK_FILE_NAME), "9999").unwrap();
        let lock = acquire_with(&dir, 5353, |_| false).expect("stale lock must be taken over");
        assert_eq!(read_pid(lock.path()), Some(5353));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_lock_file_is_treated_as_stale() {
        let dir = tmpdir("corrupt");
        std::fs::write(dir.join(LOCK_FILE_NAME), "not a pid").unwrap();
        let lock = acquire_with(&dir, 5353, |_| true).expect("corrupt lock must not wedge startup");
        assert_eq!(read_pid(lock.path()), Some(5353));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_a_lock_that_someone_else_now_owns_leaves_it_alone() {
        let dir = tmpdir("stolen");
        let lock = acquire_with(&dir, 4242, |_| true).expect("first acquire");
        // Another instance took over after deciding ours was stale.
        std::fs::write(dir.join(LOCK_FILE_NAME), "7777").unwrap();
        drop(lock);
        assert_eq!(read_pid(&dir.join(LOCK_FILE_NAME)), Some(7777));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
