//! Persistent, rotated logging (T2-3, closes F-16).
//!
//! `tracing_subscriber` installs one global subscriber per process, so this
//! lives in its own integration test binary: any other test that logged first
//! would make `try_init` a no-op and these assertions meaningless.

use playout_transcode::logging::{self, FileLogging};
use playout_transcode::service_handle::ServiceHandle;
use std::path::{Path, PathBuf};

fn read_rotated(dir: &Path, base: &str) -> String {
    // The appender writes `<base>.<YYYY-MM-DD>`; there is exactly one per run.
    let mut out = String::new();
    for entry in std::fs::read_dir(dir).expect("read log dir").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(base) {
            out.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    out
}

#[test]
fn service_logging_writes_json_files_and_feeds_the_ui_panel() {
    let dir: PathBuf = std::env::temp_dir().join(format!("pt-logging-{}-main", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // A log file that should have been pruned, and one that should survive.
    std::fs::create_dir_all(&dir).expect("create log dir");
    let today = chrono::Local::now().date_naive();
    let stale = dir.join(format!(
        "transcode.log.{}",
        (today - chrono::Duration::days(60)).format("%Y-%m-%d")
    ));
    std::fs::write(&stale, b"old").expect("write stale log");

    let handle = ServiceHandle::new();
    let guards = logging::init_service_logging(
        "info",
        FileLogging {
            dir: dir.clone(),
            file: "transcode.log".into(),
            retain_days: 14,
        },
        handle.clone(),
    )
    .expect("logging installed with file sinks");

    assert!(
        !stale.exists(),
        "a 60-day-old rotated log survived startup pruning"
    );

    // Emitted on the service's own target: the filter is
    // `playout_transcode=<level>,audit=<level>`, so an event from this test
    // crate's own target would be dropped exactly as a dependency's is.
    tracing::info!(target: "playout_transcode", "Completed and verified");
    tracing::error!(target: "playout_transcode", "encoder exited with status 1");
    tracing::warn!(
        target: "audit",
        op = "DELETE",
        path = "/api/assets/x/purge",
        status = 200u64,
        "destructive operation"
    );

    // Dropping the guards flushes the non-blocking writer threads.
    drop(guards);

    let main_log = read_rotated(&dir, "transcode.log");
    assert!(
        main_log.contains("Completed and verified"),
        "the main log is missing the info line:\n{main_log}"
    );
    assert!(
        main_log.contains("encoder exited with status 1"),
        "the main log is missing the error line:\n{main_log}"
    );
    // JSON, not the pretty console format -- this is what a support engineer
    // greps and what any log shipper parses.
    let first = main_log.lines().next().expect("at least one line");
    let parsed: serde_json::Value =
        serde_json::from_str(first).unwrap_or_else(|e| panic!("line is not JSON ({e}): {first}"));
    assert!(
        parsed.get("level").is_some() && parsed.get("fields").is_some(),
        "unexpected JSON shape: {parsed}"
    );

    let audit_log = read_rotated(&dir, "audit.log");
    assert!(
        audit_log.contains("destructive operation") && audit_log.contains("/api/assets/x/purge"),
        "the audit log is missing the destructive-operation record:\n{audit_log}"
    );
    assert!(
        !audit_log.contains("Completed and verified"),
        "non-audit events leaked into the audit log:\n{audit_log}"
    );

    let ui = handle.get_logs().join("\n");
    assert!(
        ui.contains("encoder exited with status 1"),
        "the UI panel did not receive the ERROR event:\n{ui}"
    );
    assert!(
        ui.contains("destructive operation") && ui.contains("/api/assets/x/purge"),
        "the UI panel did not receive the audit event with its fields:\n{ui}"
    );
    assert!(
        !ui.contains("Completed and verified"),
        "INFO events should not flood the UI panel:\n{ui}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
