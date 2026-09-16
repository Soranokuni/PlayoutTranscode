//! PlayoutTranscode — broadcast media ingest, analysis and mezzanine
//! transcoding engine.
//!
//! The crate is built as both a library and the `PlayoutTranscode` binary. The
//! library target exists so integration tests under `tests/` can drive the real
//! Axum router and the real handlers, instead of the stub routers the wire
//! contract tests used to build by hand (F-31).

pub mod app;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod encoder;
pub mod fingerprint;
pub mod identity;
pub mod jobs;
pub mod logging;
pub mod paths;
pub mod probe;
pub mod processor;
pub mod profiles;
pub mod server;
pub mod service_handle;
pub mod watcher;

/// Windows Service Control Manager integration. Absent on other platforms; the
/// `service-run` subcommand reports that it is Windows-only there.
#[cfg(windows)]
pub mod win_service;
