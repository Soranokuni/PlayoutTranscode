//! Windows Service Control Manager integration (T2-1, closes F-11).
//!
//! The installer has always registered the service with
//! `binPath = "<exe> run --config <path>"`, but `run` is an ordinary console
//! program: it never calls `StartServiceCtrlDispatcher`, so the SCM waits for a
//! status report that never comes and fails the start with error 1053 after
//! 30 seconds. The documented production deployment mode did not work at all.
//! The workaround — running it interactively — dies at logoff and takes ingest
//! with it.
//!
//! This module adds the missing half. `PlayoutTranscode service-run` is used
//! **only** as the SCM `binPath`; running it from a console fails immediately
//! with `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`, which is the correct and
//! intended outcome. `run` is untouched.
//!
//! Lifecycle:
//!
//! ```text
//! service_main -> StartPending (hint 30 s)
//!              -> spawn the Tokio runtime, boot run_service
//!              -> Running (accepts Stop | Shutdown | PreShutdown)
//!   Stop       -> StopPending (hint 30 s), trigger the ShutdownToken
//!              -> run_service drains HTTP, stops the watcher, kills FFmpeg,
//!                 closes the pool
//!              -> Stopped (exit code 0, or the service-specific code on error)
//! ```

use crate::app::{run_service, ShutdownToken};
use crate::paths;

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

/// Must match the name the installer passes to `sc.exe create`.
pub const SERVICE_NAME: &str = "PlayoutTranscode";

/// A Windows service hosting exactly one process.
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// How long the SCM should wait before deciding a pending transition has hung.
/// Startup opens and migrates the SQLite registry and audits the toolchain, so
/// a few seconds is realistic and 30 gives crash-recovery room.
const PENDING_HINT: Duration = Duration::from_secs(30);

/// Parameters captured by `main` before the dispatcher takes over.
///
/// `service_main` is called by the SCM through a plain `extern "system"` fn
/// pointer, so there is nowhere to thread them through except a static.
struct ServiceArgs {
    config_path: Option<String>,
}

static ARGS: OnceLock<ServiceArgs> = OnceLock::new();

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Entry point for `PlayoutTranscode service-run`.
///
/// Blocks until the SCM stops the service. An error here means the process was
/// not started by the SCM.
pub fn run(config_path: Option<String>) -> Result<(), String> {
    let _ = ARGS.set(ServiceArgs { config_path });
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| {
        format!(
            "Not started by the Service Control Manager ({e}). `service-run` is the \
             service entry point and is not meant to be run from a console; use \
             `PlayoutTranscode run` instead."
        )
    })
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = service_body() {
        // The subscriber may not be installed yet, so also take the OS path:
        // a failure before `run_service` gets going is otherwise invisible.
        tracing::error!("Service failed: {}", e);
        eprintln!("Service failed: {}", e);
    }
}

fn service_body() -> Result<(), String> {
    let shutdown = ShutdownToken::new();

    let handler_shutdown = shutdown.clone();
    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| {
        match control {
            // Required by the SCM to probe that the handler is alive.
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                handler_shutdown.trigger();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    })
    .map_err(|e| format!("Failed to register the service control handler: {e}"))?;

    let report = |state: ServiceState,
                  controls: ServiceControlAccept,
                  exit: ServiceExitCode,
                  hint: Duration,
                  checkpoint: u32| {
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: controls,
            exit_code: exit,
            checkpoint,
            wait_hint: hint,
            process_id: None,
        })
    };

    report(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        PENDING_HINT,
        1,
    )
    .map_err(|e| format!("Failed to report StartPending: {e}"))?;

    let config_path = ARGS.get().and_then(|a| a.config_path.clone());

    // `main` resolved and created the data directory before dispatching; log it
    // so an operator debugging a permission failure can see which directory the
    // service account actually needs Modify on.
    tracing::info!(
        "Service starting, data directory {}",
        paths::data_dir().display()
    );

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create the Tokio runtime: {e}"))?;

    // Running is reported before the boot completes on purpose: the SCM kills a
    // start that exceeds the wait hint, and startup does real work (registry
    // migration, crash recovery, toolchain audit). The health endpoint, not the
    // SCM state, is what tells an operator the service is actually serving.
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ServiceExitCode::Win32(0),
        Duration::default(),
        0,
    )
    .map_err(|e| format!("Failed to report Running: {e}"))?;

    let result = runtime.block_on(run_service(config_path, shutdown.clone()));

    report(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        PENDING_HINT,
        2,
    )
    .map_err(|e| format!("Failed to report StopPending: {e}"))?;

    // Give spawned blocking work (FFmpeg kills, the final DB flush) a bounded
    // window to finish rather than dropping the runtime from under it.
    runtime.shutdown_timeout(Duration::from_secs(20));

    let exit_code = match &result {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(e) => {
            tracing::error!("Service exited with an error: {}", e);
            // A service-specific code, so `sc query` distinguishes "we stopped"
            // from "Windows could not start us".
            ServiceExitCode::ServiceSpecific(1)
        }
    };

    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
        Duration::default(),
        0,
    )
    .map_err(|e| format!("Failed to report Stopped: {e}"))?;

    result.map_err(|e| e.to_string())
}
