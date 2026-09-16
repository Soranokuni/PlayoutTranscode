//! Tracing setup: console, rotated JSON files, a separate audit log and a
//! bridge into the web UI's log panel (T2-3, closes F-16).
//!
//! Until this landed the only sink was stdout. A headless install -- which is
//! every production install -- discarded it, so after an incident there was
//! nothing to read: no record of which job failed, which FFmpeg invocation
//! produced it, or which destructive API call preceded it. The audit trail
//! T1-5 writes on the `audit` target went to the same place: nowhere.
//!
//! Four sinks, installed together:
//!
//! | Sink | Format | Contents |
//! |---|---|---|
//! | stdout | pretty | everything at the configured level (interactive runs) |
//! | `<data_dir>/logs/<log_file>.<date>` | JSON, daily | everything at the configured level |
//! | `<data_dir>/logs/audit.log.<date>` | JSON, daily | `target = "audit"` only |
//! | the web UI log panel | plain text | `WARN`/`ERROR` and `target = "audit"` |
//!
//! Files older than `logging.retain_days` are pruned at startup. Rotation is
//! by date, so a service that runs for months does not accumulate one
//! unbounded file, and pruning at startup (rather than on a timer) keeps the
//! whole thing free of background tasks that could themselves fail silently.

use crate::service_handle::ServiceHandle;
use std::path::{Path, PathBuf};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer};

/// The `tracing` target T1-5 writes destructive-operation records on.
pub const AUDIT_TARGET: &str = "audit";

/// Keeps the non-blocking writer threads alive.
///
/// `tracing_appender`'s non-blocking writers flush on a background thread and
/// stop the moment their guard is dropped, so dropping this loses whatever was
/// buffered. `main` holds it for the life of the process.
pub struct LoggingGuards {
    _guards: Vec<tracing_appender::non_blocking::WorkerGuard>,
}

/// Where and how the file sinks are written.
pub struct FileLogging {
    /// Directory for the rotated files. Created if missing.
    pub dir: PathBuf,
    /// Base name of the main log, e.g. `transcode.log`. `tracing_appender`
    /// appends `.YYYY-MM-DD`.
    pub file: String,
    /// Rotated files older than this are deleted at startup. 0 disables
    /// pruning.
    pub retain_days: u16,
}

/// Console-only logging, for subcommands and tests that have no data directory.
pub fn init_logging(level: &str) {
    install(level, None, None);
}

/// The full setup: console, rotated JSON files, audit log and UI bridge.
///
/// Returns the writer guards, which the caller must keep alive. A second call
/// is a no-op -- `try_init` rather than `init` -- so a subcommand that already
/// set up console logging does not make the service path panic.
pub fn init_service_logging(
    level: &str,
    files: FileLogging,
    ui: ServiceHandle,
) -> Option<LoggingGuards> {
    install(level, Some(files), Some(ui))
}

fn filter_for(level: &str) -> EnvFilter {
    let configured =
        EnvFilter::try_new(format!("playout_transcode={level},{AUDIT_TARGET}={level}"))
            .unwrap_or_else(|_| {
                EnvFilter::new(format!("playout_transcode=info,{AUDIT_TARGET}=info"))
            });
    // `RUST_LOG` wins, so a support session can raise the level without editing
    // config.toml and restarting twice.
    EnvFilter::try_from_default_env().unwrap_or(configured)
}

fn install(
    level: &str,
    files: Option<FileLogging>,
    ui: Option<ServiceHandle>,
) -> Option<LoggingGuards> {
    let console = fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_level(true);

    let mut guards = Vec::new();
    let (main_file, audit_file) = match &files {
        Some(f) => {
            if let Err(e) = std::fs::create_dir_all(&f.dir) {
                // No `tracing` yet, so this is the only way to say it.
                eprintln!("Failed to create log directory {}: {}", f.dir.display(), e);
                (None, None)
            } else {
                if f.retain_days > 0 {
                    prune_old_logs(&f.dir, f.retain_days);
                }
                let (main_w, g1) = tracing_appender::non_blocking(
                    tracing_appender::rolling::daily(&f.dir, &f.file),
                );
                let (audit_w, g2) = tracing_appender::non_blocking(
                    tracing_appender::rolling::daily(&f.dir, "audit.log"),
                );
                guards.push(g1);
                guards.push(g2);
                (
                    Some(fmt::layer().json().with_writer(main_w)),
                    Some(
                        fmt::layer()
                            .json()
                            .with_writer(audit_w)
                            // The audit log must stay a clean record of
                            // destructive operations; everything else already
                            // has the main file.
                            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                                meta.target() == AUDIT_TARGET
                            })),
                    ),
                )
            }
        }
        None => (None, None),
    };

    // `try_init` rather than `init`: the Windows service path may reach this
    // after an earlier subcommand has already installed a subscriber, and a
    // second `init` panics. A duplicate call is a no-op, not a crash.
    let installed = tracing_subscriber::registry()
        .with(filter_for(level))
        .with(console)
        .with(main_file)
        .with(audit_file)
        .with(ui.map(UiLogLayer::new))
        .try_init()
        .is_ok();

    if !installed || guards.is_empty() {
        return None;
    }
    Some(LoggingGuards { _guards: guards })
}

/// Delete rotated files older than `retain_days`.
///
/// Matches on the `.YYYY-MM-DD` suffix `tracing_appender` writes rather than on
/// mtime, so a file that was merely touched is not kept forever, and anything
/// without that suffix (an operator's saved copy, say) is left alone.
fn prune_old_logs(dir: &Path, retain_days: u16) {
    let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(retain_days as i64);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(date) = log_date_suffix(&name) else {
            continue;
        };
        if date < cutoff {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                eprintln!("Failed to prune old log {}: {}", name, e);
            }
        }
    }
}

/// The `YYYY-MM-DD` at the end of a rotated file name, if there is one.
fn log_date_suffix(name: &str) -> Option<chrono::NaiveDate> {
    let tail = name.rsplit('.').next()?;
    chrono::NaiveDate::parse_from_str(tail, "%Y-%m-%d").ok()
}

/// Forwards selected events into the web UI's log panel.
///
/// Before this, the panel showed only the handful of hand-written
/// `ServiceHandle::add_log` calls, so an operator watching the UI could not see
/// that anything had gone wrong anywhere else in the service.
struct UiLogLayer {
    handle: ServiceHandle,
}

impl UiLogLayer {
    fn new(handle: ServiceHandle) -> Self {
        Self { handle }
    }
}

impl<S: tracing::Subscriber> Layer<S> for UiLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let is_audit = meta.target() == AUDIT_TARGET;
        let level = *meta.level();
        if !is_audit && level > tracing::Level::WARN {
            return;
        }

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let Some(message) = visitor.message else {
            return;
        };
        // The audit records are structured -- the message alone is the constant
        // "destructive operation" -- so the fields have to come along or the
        // panel shows eight identical lines.
        let message = if visitor.fields.is_empty() {
            message
        } else {
            format!("{} ({})", message, visitor.fields.join(", "))
        };

        let label = if is_audit {
            "audit"
        } else if level == tracing::Level::ERROR {
            "error"
        } else {
            "warn"
        };
        self.handle.add_log(label, &message);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            if self.message.is_none() {
                self.message = Some(value);
            }
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{:?}", value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pt-logs-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp log dir");
        d
    }

    #[test]
    fn the_rotation_date_suffix_is_recognised() {
        assert_eq!(
            log_date_suffix("transcode.log.2026-09-15"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap())
        );
        assert_eq!(
            log_date_suffix("audit.log.2026-01-02"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
        );
        // Files without the suffix are not ours to delete.
        assert_eq!(log_date_suffix("transcode.log"), None);
        assert_eq!(log_date_suffix("operator-copy.txt"), None);
        assert_eq!(log_date_suffix("transcode.log.2026-13-99"), None);
    }

    #[test]
    fn pruning_deletes_only_rotated_files_past_the_window() {
        let dir = unique_dir("prune");
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(30);
        let recent = today - chrono::Duration::days(2);

        let stale = dir.join(format!("transcode.log.{}", old.format("%Y-%m-%d")));
        let fresh = dir.join(format!("transcode.log.{}", recent.format("%Y-%m-%d")));
        let stale_audit = dir.join(format!("audit.log.{}", old.format("%Y-%m-%d")));
        let unrelated = dir.join("operator-copy.txt");
        for f in [&stale, &fresh, &stale_audit, &unrelated] {
            std::fs::write(f, b"x").expect("write fixture");
        }

        prune_old_logs(&dir, 14);

        assert!(!stale.exists(), "a 30-day-old log survived a 14-day window");
        assert!(
            !stale_audit.exists(),
            "the audit log is rotated on the same schedule"
        );
        assert!(fresh.exists(), "a 2-day-old log was deleted");
        assert!(unrelated.exists(), "a file that is not ours was deleted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_a_missing_directory_is_not_an_error() {
        prune_old_logs(Path::new("no-such-directory-pt-test"), 14);
    }
}
