//! Lifecycle rules for every FFmpeg/ffprobe child the service spawns (PL-01).
//!
//! Three gaps this closes:
//!
//! - **Orphans.** A child outlived the service. `taskkill` on stop only reaches
//!   pids someone registered, and when the service process itself died -- a
//!   crash, an SCM stop that ran past its timeout -- nothing killed anything:
//!   ffmpeg kept writing a `.tmp_` file into the library for as long as the
//!   encode took. On Windows every child is now placed in one job object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the kernel kills them all when
//!   the last handle to the job -- ours -- goes away, however we exit.
//! - **Unbounded waits.** The probe, the keyframe scan and the loudness pass
//!   used `Command::output()`, which waits forever. A source on an SMB share
//!   that stops answering held a concurrency slot until the service restarted.
//!   [`output_with_timeout`] bounds them.
//! - **Uncancellable passes.** Only the encode could be cancelled; a cancel
//!   or a service stop during a two-minute loudness pass waited it out.
//!   [`output_with_timeout`] also polls the calling thread's
//!   [`interrupt_scope`], which the processor sets for the duration of a job.

use std::cell::RefCell;
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// "Should this job stop now?" -- a cancel, or the service stopping.
pub type InterruptCheck = Arc<dyn Fn() -> bool + Send + Sync>;

thread_local! {
    static INTERRUPT: RefCell<Option<InterruptCheck>> = const { RefCell::new(None) };
}

/// Restores the previous interrupt check when dropped.
pub struct InterruptScope {
    previous: Option<InterruptCheck>,
}

impl Drop for InterruptScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        INTERRUPT.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Make `check` the interrupt for every [`output_with_timeout`] call on this
/// thread until the returned guard is dropped.
///
/// Thread-local rather than a parameter because the helpers that spawn
/// children sit behind traits (`LoudnessMeasurer`, `TranscodeRunner`) whose
/// fakes are used across the test suite; a job runs start to finish on one
/// blocking thread, so the scope is exactly the job.
pub fn interrupt_scope(check: InterruptCheck) -> InterruptScope {
    let previous = INTERRUPT.with(|slot| slot.borrow_mut().replace(check));
    InterruptScope { previous }
}

/// Has this thread's job been cancelled or its run stopped? For loops that
/// do their own work rather than wait on a child.
pub fn interrupted() -> bool {
    INTERRUPT.with(|slot| slot.borrow().as_ref().map(|f| f()).unwrap_or(false))
}

/// Why [`output_with_timeout`] gave up on a child.
pub const TIMED_OUT: &str = "timed out";
pub const INTERRUPTED: &str = "interrupted";

/// `Command::output()`, but bounded by `timeout` and by the thread's
/// interrupt, and with the child in the kill-on-close job.
///
/// stdout and stderr are drained on their own threads while waiting, so a
/// chatty child cannot fill a pipe and deadlock against us. On timeout or
/// interrupt the child is killed *and reaped* -- a bare `kill()` leaves a
/// zombie on Linux and a leaked handle on Windows -- and the error kind says
/// which (`TimedOut` / `Interrupted`).
pub fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    adopt(&child);

    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let started = Instant::now();
    let deadline = started + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let why = if interrupted() {
            Some((std::io::ErrorKind::Interrupted, INTERRUPTED))
        } else if Instant::now() >= deadline {
            Some((std::io::ErrorKind::TimedOut, TIMED_OUT))
        } else {
            None
        };
        if let Some((kind, what)) = why {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                kind,
                format!("child process {} after {:.1}s", what, started.elapsed().as_secs_f64()),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    Ok(Output {
        status,
        stdout: stdout.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
        stderr: stderr.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Put `child` in the service's kill-on-close job. A no-op off Windows, and
/// best-effort on it: failing to adopt a child only means it can outlive a
/// crash, exactly as every child could before.
pub fn adopt(child: &Child) {
    #[cfg(windows)]
    windows_job::assign(child);
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// The job handle, as an integer because raw handles are not `Sync`. It
    /// is never closed: the kernel closes it when the process exits, and that
    /// close is what kills the children.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();

    fn job() -> Option<isize> {
        *JOB.get_or_init(|| unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                tracing::warn!("CreateJobObjectW failed; children may outlive the service");
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                tracing::warn!("SetInformationJobObject failed; children may outlive the service");
                return None;
            }
            Some(handle as isize)
        })
    }

    pub fn assign(child: &std::process::Child) {
        let Some(job) = job() else { return };
        let process = child.as_raw_handle();
        let ok = unsafe { AssignProcessToJobObject(job as _, process as _) };
        if ok == 0 {
            tracing::debug!(
                "AssignProcessToJobObject failed for pid {}: {}",
                child.id(),
                std::io::Error::last_os_error()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeper(secs: u32) -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", &format!("Start-Sleep -Seconds {}", secs)]);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sleep");
            c.arg(secs.to_string());
            c
        }
    }

    #[test]
    fn a_child_that_overruns_is_killed_and_reported_as_timed_out() {
        let started = Instant::now();
        let err = output_with_timeout(&mut sleeper(30), Duration::from_millis(500)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    #[test]
    fn an_interrupt_stops_a_child_early() {
        let _scope = interrupt_scope(Arc::new(|| true));
        let started = Instant::now();
        let err = output_with_timeout(&mut sleeper(30), Duration::from_secs(60)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_scope_is_restored_when_dropped() {
        {
            let _scope = interrupt_scope(Arc::new(|| true));
            assert!(interrupted());
        }
        assert!(!interrupted());
    }

    #[test]
    fn output_is_collected_as_command_output_would() {
        #[cfg(windows)]
        let mut c = {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo hello"]);
            c
        };
        #[cfg(not(windows))]
        let mut c = {
            let mut c = Command::new("echo");
            c.arg("hello");
            c
        };
        let out = output_with_timeout(&mut c, Duration::from_secs(20)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
    }
}
