use crate::{bootstrap, config, db, encoder, fingerprint, identity, jobs, probe, profiles};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use uuid::Uuid;

pub trait Publisher {
    fn stage_path(&self, final_path: &Path, uuid: &str) -> std::path::PathBuf;
    fn publish(&self, staged: &Path, final_path: &Path) -> Result<(), String>;
    fn cleanup_staging(&self, staged: &Path);
}

pub struct LocalFilePublisher;

impl Publisher for LocalFilePublisher {
    fn stage_path(&self, final_path: &Path, uuid: &str) -> std::path::PathBuf {
        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        let filename = final_path.file_name().unwrap_or_default().to_string_lossy();
        parent.join(format!(".tmp_{}_{}", uuid, filename))
    }

    fn publish(&self, staged: &Path, final_path: &Path) -> Result<(), String> {
        if !staged.exists() {
            return Err(format!("Staging file does not exist: {}", staged.display()));
        }
        if final_path.exists() {
            return Err(format!(
                "Final output path already exists: {}",
                final_path.display()
            ));
        }
        std::fs::rename(staged, final_path).map_err(|e| {
            format!(
                "Failed to rename staging file '{}' -> '{}': {}",
                staged.display(),
                final_path.display(),
                e
            )
        })
    }

    fn cleanup_staging(&self, staged: &Path) {
        if staged.exists() {
            let _ = std::fs::remove_file(staged);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Retryable,
    Permanent,
    Cancelled,
}

pub fn classify_error(err_msg: &str, is_validation_failure: bool) -> RetryClass {
    if is_validation_failure {
        return RetryClass::Permanent;
    }
    let lower = err_msg.to_ascii_lowercase();
    if lower.contains("cancelled") || lower.contains("canceled") || lower.contains("stop") {
        return RetryClass::Cancelled;
    }
    if lower.contains("os error 32")
        || lower.contains("file locked")
        || lower.contains("sharing violation")
        || lower.contains("lock")
        || lower.contains("busy")
    {
        return RetryClass::Retryable;
    }
    if lower.contains("probe:")
        || lower.contains("profile disabled")
        || lower.contains("invalid input")
        || lower.contains("no such file or directory")
        || lower.contains("unsupported codec")
        || lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("disk full")
        || lower.contains("space")
        || lower.contains("already exists")
        || lower.contains("audio measurement failed")
        || lower.contains("unsupported_audio_channel_layout")
        || lower.contains("unsupported channel layout")
    {
        return RetryClass::Permanent;
    }
    if lower.contains("timeout")
        || lower.contains("resource temporarily unavailable")
        || lower.contains("ffmpeg exited with code")
        || lower.contains("failed to spawn ffmpeg")
        || lower.contains("output file missing or 0 bytes")
    {
        return RetryClass::Retryable;
    }
    RetryClass::Retryable
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceCleanupResult {
    pub enabled: bool,
    pub attempted: bool,
    pub deleted: bool,
    pub skipped: bool,
    pub reason: Option<String>,
    pub warning: Option<String>,
}

/// Validates and cleans up the original input file after transcode and mark_ready succeed.
/// Fail closed: if any check fails, the source is retained and a structured result is returned.
pub fn validate_and_cleanup_source(
    source_path: &Path,
    watch_folder: &Path,
    target_folder: &Path,
    final_output_path: &Path,
    initial_size: Option<u64>,
    initial_mtime: Option<u64>,
    queue: Option<&jobs::JobQueue>,
    current_job_id: Option<&str>,
    enabled: bool,
) -> SourceCleanupResult {
    if !enabled {
        return SourceCleanupResult {
            enabled: false,
            attempted: false,
            deleted: false,
            skipped: true,
            reason: Some("disabled".to_string()),
            warning: None,
        };
    }

    let source_str = source_path.to_string_lossy();
    let trimmed = source_str.trim();
    if trimmed.is_empty() {
        return SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("unsafe_path".to_string()),
            warning: Some("Source path is empty".to_string()),
        };
    }

    if source_path.components().any(|c| c == std::path::Component::ParentDir) {
        return SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("unsafe_path".to_string()),
            warning: Some("Source path contains parent directory traversal (..)".to_string()),
        };
    }

    if trimmed == "/" || trimmed == "\\" || (trimmed.len() <= 3 && trimmed.ends_with(":\\")) {
        return SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("unsafe_path".to_string()),
            warning: Some("Cannot delete root directory".to_string()),
        };
    }

    if source_path.is_dir() {
        return SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("source_is_directory".to_string()),
            warning: Some("Refusing to delete directory as source file".to_string()),
        };
    }

    // Canonicalize paths where possible
    let can_source = match source_path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("already_removed".to_string()),
                warning: None,
            };
        }
        Err(e) => {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("unsafe_path".to_string()),
                warning: Some(format!("Failed to canonicalize source path: {}", e)),
            };
        }
    };

    if let Ok(can_watch) = watch_folder.canonicalize() {
        if !can_source.starts_with(&can_watch) {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("outside_source_root".to_string()),
                warning: Some("Source path is outside configured watch directory root".to_string()),
            };
        }
        if can_source == can_watch {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_is_directory".to_string()),
                warning: Some("Refusing to delete watch folder root".to_string()),
            };
        }
    } else {
        return SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("outside_source_root".to_string()),
            warning: Some("Failed to resolve watch directory root".to_string()),
        };
    }

    // Check target / mezzanine isolation
    if let Ok(can_target) = target_folder.canonicalize() {
        if can_source.starts_with(&can_target) {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_matches_target".to_string()),
                warning: Some("Source path is inside target mezzanine directory".to_string()),
            };
        }
    }
    if let Ok(can_final) = final_output_path.canonicalize() {
        if can_source == can_final {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_matches_target".to_string()),
                warning: Some("Source path matches final output mezzanine path".to_string()),
            };
        }
    }

    // Recheck source file identity
    let current_meta = match std::fs::metadata(source_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("already_removed".to_string()),
                warning: None,
            };
        }
        Err(e) => {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("delete_failed".to_string()),
                warning: Some(format!("Failed to read source metadata before deletion: {}", e)),
            };
        }
    };

    let current_size = current_meta.len();
    let current_mtime = current_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    if let Some(init_size) = initial_size {
        if current_size != init_size {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_changed".to_string()),
                warning: Some(format!(
                    "Source file size changed during processing (was {}, now {})",
                    init_size, current_size
                )),
            };
        }
    }

    if let (Some(init_mtime), Some(curr_mtime)) = (initial_mtime, current_mtime) {
        if curr_mtime != init_mtime {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_changed".to_string()),
                warning: Some(format!(
                    "Source file modification time changed during processing (was {}, now {})",
                    init_mtime, curr_mtime
                )),
            };
        }
    }

    // Check if another active or pending job references this same input path
    if let Some(q) = queue {
        let active_jobs = q.all_recent();
        let other_job_active = active_jobs.iter().any(|j| {
            if let Some(cur_id) = current_job_id {
                if j.id == cur_id {
                    return false;
                }
            }
            if j.state == jobs::JobState::Pending || j.state == jobs::JobState::Processing {
                if let (Ok(j_path), Ok(s_path)) = (
                    std::path::Path::new(&j.input_path).canonicalize(),
                    source_path.canonicalize(),
                ) {
                    return j_path == s_path;
                }
            }
            false
        });

        if other_job_active {
            return SourceCleanupResult {
                enabled: true,
                attempted: true,
                deleted: false,
                skipped: true,
                reason: Some("source_referenced".to_string()),
                warning: Some("Source file is currently referenced by another pending or active transcode job".to_string()),
            };
        }
    }

    // Perform removal
    match std::fs::remove_file(source_path) {
        Ok(_) => SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: true,
            skipped: false,
            reason: Some("deleted".to_string()),
            warning: None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("already_removed".to_string()),
            warning: None,
        },
        Err(e) => SourceCleanupResult {
            enabled: true,
            attempted: true,
            deleted: false,
            skipped: true,
            reason: Some("delete_failed".to_string()),
            warning: Some(format!("Source cleanup deletion failed: {}", e)),
        },
    }
}

pub trait TranscodeRunner {
    fn run_transcode(
        &self,
        tools: &bootstrap::ToolPaths,
        config: &config::AppConfig,
        input_path: &Path,
        source_probe: &probe::ProbeData,
        profile_id: profiles::ProfileId,
        output_path: &Path,
        metadata_uuid: &str,
        progress_tx: std::sync::mpsc::Sender<encoder::EncodeProgress>,
        job_id: &str,
        active_pids: Option<crate::service_handle::ActivePids>,
        audio_policy: &config::AudioPolicy,
        measured_loudness: Option<&probe::MeasuredLoudness>,
    ) -> encoder::EncodeResult;
}

pub struct RealTranscodeRunner;

impl TranscodeRunner for RealTranscodeRunner {
    fn run_transcode(
        &self,
        tools: &bootstrap::ToolPaths,
        config: &config::AppConfig,
        input_path: &Path,
        source_probe: &probe::ProbeData,
        profile_id: profiles::ProfileId,
        output_path: &Path,
        metadata_uuid: &str,
        progress_tx: std::sync::mpsc::Sender<encoder::EncodeProgress>,
        job_id: &str,
        active_pids: Option<crate::service_handle::ActivePids>,
        audio_policy: &config::AudioPolicy,
        measured_loudness: Option<&probe::MeasuredLoudness>,
    ) -> encoder::EncodeResult {
        encoder::transcode_file(
            tools,
            config,
            input_path,
            source_probe,
            profile_id,
            output_path,
            metadata_uuid,
            progress_tx,
            job_id,
            active_pids,
            audio_policy,
            measured_loudness,
        )
    }
}

pub fn process_file_sync(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: crate::service_handle::ActivePids,
    existing_job: Option<jobs::JobRecord>,
) {
    process_file_sync_with_runner(
        queue,
        tools,
        target_root,
        input_path,
        config,
        pool,
        active_pids,
        existing_job,
        &RealTranscodeRunner,
    );
}

pub fn process_file_sync_with_runner(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: crate::service_handle::ActivePids,
    existing_job: Option<jobs::JobRecord>,
    runner: &impl TranscodeRunner,
) {
    process_file_sync_with_runner_and_measurer(
        queue,
        tools,
        target_root,
        input_path,
        config,
        pool,
        active_pids,
        existing_job,
        runner,
        &probe::RealLoudnessMeasurer,
    );
}

pub fn process_file_sync_with_runner_and_measurer(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: crate::service_handle::ActivePids,
    existing_job: Option<jobs::JobRecord>,
    runner: &impl TranscodeRunner,
    measurer: &impl probe::LoudnessMeasurer,
) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        process_file_inner(
            queue,
            tools,
            target_root,
            input_path,
            config,
            pool,
            active_pids,
            existing_job.clone(),
            runner,
            measurer,
        );
    }));

    if let Err(panic_payload) = result {
        let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic in process_file_sync".to_string()
        };
        tracing::error!(
            "PANIC in process_file_sync for {}: {}",
            input_path.display(),
            msg
        );
        queue.broadcast(
            "failed",
            &serde_json::json!({
                "error": format!("Internal panic: {}", msg),
                "path": input_path.to_string_lossy(),
            })
            .to_string(),
        );
    }
}

/// The floor for the disk preflight, whatever the job's own estimate says.
///
/// Even a thirty-second ident needs room for the staged file, the sidecar and
/// FFmpeg's own scratch, and a volume this close to full is about to cause
/// other problems anyway.
pub const MIN_FREE_BYTES: u64 = 500 * 1024 * 1024;

/// Headroom over the arithmetic estimate: VBR overshoot, container overhead and
/// whatever else lands on the volume while the encode runs.
const DISK_ESTIMATE_MARGIN: f64 = 1.2;

/// Slack for the sidecar, FFmpeg's temp files and filesystem rounding.
const DISK_FIXED_SLACK: u64 = 64 * 1024 * 1024;

/// Parse an FFmpeg rate string — `15M`, `320k`, `4500000` — into bits/second.
///
/// Returns `None` for anything it does not understand, which the caller treats
/// as "fall back to the floor" rather than as an error: refusing to encode
/// because a bitrate string was unfamiliar would be worse than a preflight that
/// is occasionally too optimistic.
pub fn parse_ffmpeg_rate_bps(rate: &str) -> Option<u64> {
    let t = rate.trim();
    if t.is_empty() {
        return None;
    }
    let (digits, multiplier) = match t.chars().last() {
        Some('k') | Some('K') => (&t[..t.len() - 1], 1_000u64),
        Some('m') | Some('M') => (&t[..t.len() - 1], 1_000_000u64),
        Some('g') | Some('G') => (&t[..t.len() - 1], 1_000_000_000u64),
        _ => (t, 1u64),
    };
    digits.trim().parse::<f64>().ok().and_then(|n| {
        if n < 0.0 || !n.is_finite() {
            None
        } else {
            Some((n * multiplier as f64) as u64)
        }
    })
}

/// How much free space this particular job needs.
///
/// The preflight used to be a flat 500 MB for every job (F-22), so a two-hour
/// feature at 15 Mbit/s — about 13 GB — passed a check made against 500 MB,
/// encoded for an hour and then died on `No space left on device` with the
/// staged file abandoned. Sizing it from the job means the failure happens in
/// the first second instead, and says what it actually needs.
pub fn required_bytes_for(duration_secs: f64, video_rate: &str, audio_rate: &str) -> u64 {
    let video_bps = parse_ffmpeg_rate_bps(video_rate).unwrap_or(0);
    let audio_bps = parse_ffmpeg_rate_bps(audio_rate).unwrap_or(0);
    let total_bps = video_bps.saturating_add(audio_bps);

    if duration_secs <= 0.0 || total_bps == 0 {
        // An unprobeable duration or an unparseable rate. The floor is the only
        // honest answer; guessing high would refuse work that would have run.
        return MIN_FREE_BYTES;
    }

    let bytes = (duration_secs * total_bps as f64 / 8.0) * DISK_ESTIMATE_MARGIN;
    let estimate = if bytes.is_finite() && bytes >= 0.0 {
        bytes as u64
    } else {
        0
    };
    estimate
        .saturating_add(DISK_FIXED_SLACK)
        .max(MIN_FREE_BYTES)
}

/// Where a mezzanine goes when the registry will not accept it (T2-8).
///
/// Not a temp directory and not the bin: an operator has to be able to find it
/// and decide. The name is preserved so it is obvious what the file was.
pub const QUARANTINE_DIR: &str = "quarantine";

/// Retry a fallible blocking operation with a fixed backoff.
///
/// `op` receives the 1-based attempt number. Returns the first success, or the
/// last error once `attempts` have been spent.
fn retry_blocking<T, E: std::fmt::Display>(
    attempts: u32,
    backoff: std::time::Duration,
    what: &str,
    mut op: impl FnMut(u32) -> Result<T, E>,
) -> Result<T, E> {
    let attempts = attempts.max(1);
    let mut last: Option<E> = None;
    for attempt in 1..=attempts {
        match op(attempt) {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < attempts {
                    tracing::warn!(
                        "{} failed on attempt {}/{}: {} -- retrying in {}ms",
                        what,
                        attempt,
                        attempts,
                        e,
                        backoff.as_millis()
                    );
                    std::thread::sleep(backoff);
                }
                last = Some(e);
            }
        }
    }
    // `attempts >= 1`, so the loop ran and `last` is populated on this path.
    Err(last.expect("retry_blocking ran at least one attempt"))
}

/// Move a published mezzanine out of the library into `<target>/quarantine/`.
///
/// Called when the file encoded fine but the registry would not record it. It
/// must not stay in `videos/`: nothing references it, the next ingest of the
/// same source would collide with it, and a listing built from the filesystem
/// rather than the registry would show an asset PlayOut cannot resolve.
///
/// Returns where the file ended up. A name collision in `quarantine/` is
/// resolved by suffixing, because a repeated failure must not overwrite the
/// evidence from the first one.
fn quarantine_published(
    target_root: &Path,
    published: &Path,
) -> Result<std::path::PathBuf, String> {
    let dir = target_root.join(QUARANTINE_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create the quarantine directory: {}", e))?;

    let file_name = published
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());

    let mut dest: std::path::PathBuf = dir.join(&file_name);
    if dest.exists() {
        let stem = published
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());
        let ext = published
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        for n in 1..1000 {
            let candidate = dir.join(format!("{}.{}{}", stem, n, ext));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
        }
    }

    // Same volume, so a rename is atomic and cheap. Fall back to copy+delete
    // only if it is not (a target folder spanning volumes is unsupported, but
    // losing the file over it would be worse than a slow move).
    match std::fs::rename(published, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            std::fs::copy(published, &dest)
                .map_err(|e| format!("cannot move the file to quarantine: {}", e))?;
            let _ = std::fs::remove_file(published);
            Ok(dest)
        }
    }
}

/// Is a sampled-fingerprint match a *real* duplicate?
///
/// The sampled fingerprint is a prefilter, never a verdict — see
/// [`crate::fingerprint`]. Only two full SHA-256 hashes that agree confirm a
/// duplicate; every other combination means "cannot confirm", and the file is
/// ingested.
///
/// Failing towards a redundant encode is the cheap mistake. Failing the other
/// way silently drops a programme, and nothing downstream can tell that it
/// happened (F-15).
fn is_confirmed_duplicate(
    stored_hash: Option<&str>,
    our_hash: Option<&str>,
    existing_uuid: &str,
) -> bool {
    match (stored_hash, our_hash) {
        (Some(stored), Some(ours)) => stored == ours,
        // The existing row predates `source_sha256` (T2-6), so there is nothing
        // to compare against. Falling back to the sampled match would reinstate
        // F-15 for exactly the rows most likely to have been hit by it. The
        // cost of re-ingesting is one redundant encode per legacy asset, once.
        (None, _) => {
            tracing::info!(
                "Dedup: asset {} matched on the sampled fingerprint but predates \
                 source_sha256; cannot confirm, re-transcoding",
                existing_uuid
            );
            false
        }
        // Our own hash could not be computed — an unreadable or vanishing
        // source. Same reasoning.
        (_, None) => false,
    }
}

fn process_file_inner(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: crate::service_handle::ActivePids,
    existing_job: Option<jobs::JobRecord>,
    runner: &impl TranscodeRunner,
    measurer: &impl probe::LoudnessMeasurer,
) {
    // Every early return below happens before the main job record is created.
    //
    // T2-4 made them close out an *adopted* record, or it stayed Pending
    // forever (F-13). But on a fresh ingest -- the watcher offering a file for
    // the first time -- there was no record to close, so a file rejected at the
    // watch-folder boundary, or skipped as a duplicate, simply vanished: no job,
    // no event, nothing in /api/jobs, and an operator watching a folder saw
    // their file disappear with no explanation (F-26). Now every early return
    // leaves a visible terminal record, adopted or created.
    let terminate_early = |phase: jobs::JobPhase,
                           category: Option<&str>,
                           message: &str,
                           asset_uuid: Option<&str>| {
        let msg = message.to_string();
        let cat = category.map(|c| c.to_string());
        let stage = phase.as_str().to_string();
        let apply = move |job: &mut jobs::JobRecord| {
            job.error = Some(msg);
            job.error_category = cat;
            job.finished_at = Some(chrono::Utc::now().to_rfc3339());
        };

        let id = match existing_job.as_ref() {
            Some(prior) => {
                let _ = queue.transition(&prior.id, phase, Some(stage), apply);
                prior.id.clone()
            }
            None => {
                let mut job = jobs::JobRecord::new(&input_path.to_string_lossy(), "pending");
                if let Some(u) = asset_uuid {
                    // A skip points at the asset that already holds this
                    // content, so the UI can link straight to it.
                    job.uuid = Some(u.to_string());
                }
                let id = job.id.clone();
                let _ = job.transition_to(phase, Some(stage));
                apply(&mut job);
                queue.push(job);
                id
            }
        };

        // `skipped` is a new event type. Clients that do not know it must treat
        // an unknown SSE event as a no-op -- PlayOut and the web UI both do.
        let event = if phase == jobs::JobPhase::Skipped {
            "skipped"
        } else {
            "failed"
        };
        queue.broadcast(
            event,
            &serde_json::json!({
                "id": id,
                "error": message,
                "error_category": category,
                "uuid": asset_uuid,
            })
            .to_string(),
        );
    };

    let close_job = |category: &str, message: &str| {
        terminate_early(jobs::JobPhase::Failed, Some(category), message, None);
    };
    let skip_job = |asset_uuid: &str, message: &str| {
        terminate_early(jobs::JobPhase::Skipped, None, message, Some(asset_uuid));
    };

    let watch_root = std::path::Path::new(&config.paths.watch_folder);
    // Both sides must end up spelled the same way or the containment check is
    // meaningless.
    //
    // The previous version canonicalized each side and fell back to the raw
    // path on failure. That is not enough: a file deleted between the watcher
    // offering it and this worker picking it up cannot be canonicalized, so its
    // side kept whatever spelling the watcher produced while the watch root was
    // resolved to its true form. Wherever those differ the comparison fails and
    // a missing file is reported as a path-traversal attempt.
    //
    // They differ more often than it looks. Windows hands out 8.3 short names
    // (`C:\Users\RUNNER~1\...` for `C:\Users\runneradmin\...`) through the
    // environment, and junctions and mapped drives resolve elsewhere again. CI
    // on a GitHub Windows runner is exactly that case, which is how this was
    // caught.
    //
    // `canonicalize_existing_prefix` resolves the deepest ancestor that exists
    // -- normally the watch folder itself -- and re-joins the rest, so both
    // sides are canonical even when the file is gone.
    let canonical_input = crate::paths::canonicalize_existing_prefix(input_path);
    let canonical_watch = crate::paths::canonicalize_existing_prefix(watch_root);
    if !canonical_input.starts_with(&canonical_watch) {
        tracing::warn!("Rejected path traversal attempt: {}", input_path.display());
        close_job("path_outside_watch_folder", "Input is outside the watch folder");
        return;
    }

    let initial_source_meta = std::fs::metadata(input_path).ok();
    let initial_source_size = initial_source_meta.as_ref().map(|m| m.len());
    let initial_source_mtime = initial_source_meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    let handle = tokio::runtime::Handle::current();

    let fingerprint = match fingerprint::compute_sampled_fingerprint(input_path) {
        Ok(fp) => fp,
        Err(e) => {
            tracing::error!("Fingerprint failed for {}: {}", input_path.display(), e);
            close_job("fingerprint_failure", "Could not read the source file");
            return;
        }
    };

    // The full hash, computed lazily: only when the cheap sampled hash has
    // already matched something. For a library of distinct media that is
    // approximately never, so the common path still reads 192 KiB, not 40 GB.
    let mut source_sha256: Option<String> = None;
    let mut full_hash_of_source = || -> Option<String> {
        if source_sha256.is_none() {
            match fingerprint::compute_full_sha256(input_path) {
                Ok(h) => source_sha256 = Some(h),
                Err(e) => {
                    tracing::warn!(
                        "Full hash failed for {}: {} -- treating as not-a-duplicate",
                        input_path.display(),
                        e
                    );
                }
            }
        }
        source_sha256.clone()
    };

    if let Ok(Some(existing)) = handle.block_on(db::find_by_fingerprint(pool, fingerprint)) {
        let usable = existing.status == "ready"
            && existing.mezzanine_ok
            && !existing.current_path.is_empty()
            && std::path::Path::new(&existing.current_path).exists();

        // A sampled-hash match is a *candidate*, never a verdict. Two distinct
        // programmes cut from the same master share size, leader and tail, and
        // the old code dropped the second one on that evidence alone (F-15).
        let confirmed = usable
            && is_confirmed_duplicate(
                existing.source_sha256.as_deref(),
                full_hash_of_source().as_deref(),
                &existing.uuid,
            );

        if confirmed {
            tracing::info!(
                "Dedup: asset {} is byte-identical (sha256 confirmed) and already ready at {}, skipping transcode",
                existing.uuid,
                existing.current_path,
            );
            // A confirmed duplicate is not a failure -- nothing went wrong and
            // no work was needed. T2-6 gave the phase machine a terminal
            // `Skipped` for it, which maps to the v1 `Completed` state so the
            // wire contract is unchanged (T2-4 had to report this as `Failed`
            // for want of anywhere else to put it).
            skip_job(
                &existing.uuid,
                "Skipped: an identical asset is already ingested",
            );
            return;
        }

        if usable {
            tracing::info!(
                "Dedup: fingerprint {} matched asset {} but the full hashes differ -- \
                 these are different files, ingesting both",
                fingerprint,
                existing.uuid,
            );
        } else {
            tracing::info!(
                "Dedup: fingerprint {} matched but existing asset not usable (status={}, mezzanine_ok={}, path_exists={}), re-transcoding",
                fingerprint,
                existing.status,
                existing.mezzanine_ok,
                std::path::Path::new(&existing.current_path).exists(),
            );
            // Only clears out the leftovers of a failed ingest. Subclips and
            // live `ready` rows are protected -- deleting every row sharing the
            // fingerprint destroyed operator-cut subclips (F-26).
            match handle.block_on(db::purge_unusable_rows_by_fingerprint(
                pool,
                fingerprint,
                |p| std::path::Path::new(p).exists(),
            )) {
                Ok(outcome) => {
                    if outcome.demoted > 0 || outcome.protected > 0 {
                        tracing::info!(
                            "Re-ingest cleanup for fingerprint {}: {} deleted, {} demoted to error, {} protected (subclips / live assets)",
                            fingerprint,
                            outcome.deleted,
                            outcome.demoted,
                            outcome.protected,
                        );
                    }
                }
                Err(e) => tracing::error!("Re-ingest cleanup failed: {}", e),
            }
        }
    }

    // Computed here if the dedup path never needed it, so every new row
    // carries one and the next ingest has something to confirm against.
    let source_sha256 = full_hash_of_source();

    let metadata_uuid = Uuid::new_v4().to_string();
    let video_dir = target_root.join("videos");
    let _ = std::fs::create_dir_all(&video_dir);
    let raw_stem = input_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let safe_stem = identity::sanitize_filename(&raw_stem);

    let final_output_path = build_unique_output_path(&video_dir, &safe_stem, &metadata_uuid);
    let publisher = LocalFilePublisher;
    let staged_output_path = publisher.stage_path(&final_output_path, &metadata_uuid);
    publisher.cleanup_staging(&staged_output_path);

    if let Err(e) = handle.block_on(db::insert_processing(
        pool,
        &metadata_uuid,
        fingerprint,
        source_sha256.as_deref(),
        &input_path.to_string_lossy(),
        &raw_stem,
    )) {
        // This used to log and carry on, which is F-18 at its worst: the encode
        // ran to completion, wrote a mezzanine into the library, and then
        // `mark_ready` updated a row that had never been inserted -- zero rows
        // affected, `Ok(())`. The result was a published file no registry knew
        // about, and therefore an hour of CPU spent on an asset PlayOut could
        // never see. Fail here instead, before any of that work happens.
        tracing::error!(
            "DB insert processing failed for {}: {} -- refusing to transcode a file \
             the registry has no row for",
            input_path.display(),
            e
        );
        publisher.cleanup_staging(&staged_output_path);
        close_job(
            "db_insert_failed",
            "Could not create the registry entry for this file",
        );
        return;
    }

    // Reuse the adopted record where there is one. Creating a fresh JobRecord
    // for a retry is what left the old one behind as a permanent ghost (F-13);
    // the id, created_at and attempt count all carry over.
    let job = match existing_job.as_ref() {
        Some(prior) => {
            let uuid = metadata_uuid.clone();
            let _ = queue.transition(
                &prior.id,
                jobs::JobPhase::Probing,
                Some("Probing".to_string()),
                |j| {
                    j.uuid = Some(uuid);
                    j.fingerprint = Some(fingerprint);
                    j.error = None;
                    j.error_category = None;
                    j.finished_at = None;
                },
            );
            queue.get(&prior.id).unwrap_or_else(|| prior.clone())
        }
        None => {
            let mut job = jobs::JobRecord::new(&input_path.to_string_lossy(), "pending");
            job.uuid = Some(metadata_uuid.clone());
            job.fingerprint = Some(fingerprint);
            let _ = job.transition_to(jobs::JobPhase::Probing, Some("Probing".to_string()));
            queue.push(job.clone());
            job
        }
    };
    queue.broadcast(
        "job_update",
        &serde_json::json!({"id": job.id, "stage": "Probing", "phase": "probing"}).to_string(),
    );

    let probe_data = match probe::probe_media(tools, input_path) {
        Ok(p) => p,
        Err(e) => {
            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Failed,
                Some("Failed".into()),
                |j| {
                    j.error = Some(format!("Probe: {}", e));
                    j.error_category = Some("probe_failure".into());
                },
            );
            tracing::error!("Probe failed for {}: {}", input_path.display(), e);
            queue.broadcast(
                "failed",
                &serde_json::json!({"id": job.id, "error": format!("Probe: {}", e)}).to_string(),
            );
            let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
            publisher.cleanup_staging(&staged_output_path);
            return;
        }
    };

    let profile_id = probe_data.profile_id();
    let profile_name = profile_id.to_string();
    let profile = profiles::EncodingProfile::by_id(profile_id);
    if !profile.config_for(config).enabled {
        let _ = queue.transition(
            &job.id,
            jobs::JobPhase::Failed,
            Some("Failed".into()),
            |j| {
                j.error = Some("Profile disabled".into());
                j.error_category = Some("profile_disabled".into());
            },
        );
        queue.broadcast(
            "failed",
            &serde_json::json!({"id": job.id, "error": "Profile disabled"}).to_string(),
        );
        let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
        publisher.cleanup_staging(&staged_output_path);
        return;
    }

    let audio_policy = config.effective_audio_policy();
    let measured_loudness = if !probe_data.has_valid_audio() {
        None
    } else {
        match measurer.measure_loudness(
            tools,
            input_path,
            probe_data.audio_channels,
            probe_data.duration_secs,
            &audio_policy,
        ) {
            Ok(m) => m,
            Err(e) => {
                let _ = queue.transition(
                    &job.id,
                    jobs::JobPhase::Failed,
                    Some("Failed".into()),
                    |j| {
                        j.error = Some(format!("Audio measurement failed: {}", e));
                        j.error_category = Some("audio_measurement_failure".into());
                    },
                );
                tracing::error!(
                    "Audio measurement failed for {}: {}",
                    input_path.display(),
                    e
                );
                queue.broadcast("failed", &serde_json::json!({"id": job.id, "error": format!("Audio measurement failed: {}", e)}).to_string());
                let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
                publisher.cleanup_staging(&staged_output_path);
                return;
            }
        }
    };

    let retry_policy = config.effective_retry_policy();
    let max_attempts = (retry_policy.max_attempts as usize).max(1);
    let retry_delay_ms = retry_policy.retry_delay_ms;

    let req_hash = format!(
        "{:016x}",
        (fingerprint as u64) ^ (probe_data.frame_count as u64).rotate_left(13)
    );
    let _ = queue.transition(
        &job.id,
        jobs::JobPhase::Planned,
        Some(format!("Planned ({})", profile_name)),
        |j| {
            j.profile = profile_name.clone();
            j.source_frame_count = probe_data.frame_count;
            j.duration_secs = probe_data.duration_secs;
            j.duration_ms = (probe_data.duration_secs * 1000.0).round() as i64;
            j.max_attempts = max_attempts as u32;
            j.request_hash = Some(req_hash);
        },
    );

    // Sized from this job, not a flat 500 MB (T2-9). `profile` is the encoding
    // profile chosen from the source probe a few lines above, so its maxrate is
    // the rate this encode will actually be capped at.
    let required_bytes = required_bytes_for(
        probe_data.duration_secs,
        &profile.config_for(config).maxrate,
        &config.encoding.audio_bitrate,
    );
    if let Err(e) = check_disk_space(Path::new(&config.paths.target_folder), required_bytes) {
        tracing::error!("Disk preflight failed for {}: {}", input_path.display(), e);
        let _ = queue.transition(
            &job.id,
            jobs::JobPhase::Failed,
            Some("Failed".into()),
            |j| {
                j.error = Some(e.clone());
                j.error_category = Some("io_disk_full".into());
            },
        );
        queue.broadcast(
            "failed",
            &serde_json::json!({"id": job.id, "error": e}).to_string(),
        );
        let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
        queue.prune_old(500);
        return;
    }

    let mut attempt = 1;
    let mut last_error;

    while attempt <= max_attempts {
        publisher.cleanup_staging(&staged_output_path);

        let stage_label = if max_attempts > 1 {
            format!(
                "Encoding {} (attempt {}/{})",
                profile_name, attempt, max_attempts
            )
        } else {
            format!("Encoding {}", profile_name)
        };

        let _ = queue.transition(
            &job.id,
            jobs::JobPhase::Encoding,
            Some(stage_label.clone()),
            |j| {
                j.attempt = attempt as u32;
                j.current_stage = stage_label.clone();
            },
        );

        let (ptx, prx) = std::sync::mpsc::channel::<encoder::EncodeProgress>();
        let jid = job.id.clone();
        let qc = queue.clone();
        std::thread::spawn(move || {
            let mut last_broadcast = std::time::Instant::now();
            const THROTTLE_MS: u64 = 250;
            while let Ok(p) = prx.recv() {
                let pct = p.percent;
                // In-memory only: FFmpeg emits a progress line every few
                // frames and each one used to spawn its own upsert -- from
                // this std thread, that meant a new OS thread and a new Tokio
                // runtime per line (F-13). The row is written below, at the
                // same 250 ms throttle as the SSE broadcast.
                qc.update_local(&jid, |j| {
                    j.progress = pct;
                    j.current_frame = p.frame;
                    j.encode_fps = p.fps;
                    j.encode_bitrate = p.bitrate.clone();
                    j.encode_speed = p.speed.clone();
                    j.current_time_ms = p.current_time_ms;
                    j.duration_ms = p.duration_ms;
                    j.current_stage = if pct >= 100.0 {
                        "Finalizing".into()
                    } else {
                        format!("Encoding {:.0}%", pct)
                    };
                });
                let now = std::time::Instant::now();
                if now.duration_since(last_broadcast).as_millis() as u64 >= THROTTLE_MS
                    || pct >= 100.0
                {
                    last_broadcast = now;
                    qc.persist_now(&jid);
                    let determinate = p.duration_ms > 0 || p.total_frames > 0;
                    let _ = qc.broadcast(
                        "progress",
                        &serde_json::json!({
                            "id": jid,
                            "percent": pct,
                            "current_time_ms": p.current_time_ms,
                            "duration_ms": p.duration_ms,
                            "determinate": determinate,
                            "fps": p.fps,
                            "bitrate": p.bitrate,
                            "speed": p.speed,
                            "stage": if pct >= 100.0 { "Finalizing" } else { "Encoding" },
                        })
                        .to_string(),
                    );
                }
            }
        });

        let worker_id = format!("worker-{}", &metadata_uuid[..8.min(metadata_uuid.len())]);
        let (hb_stop_tx, hb_stop_rx) = std::sync::mpsc::channel::<()>();
        let hb_jid = job.id.clone();
        let hb_wid = worker_id.clone();
        let hb_queue = queue.clone();
        let hb_pids = active_pids.clone();

        let hb_thread = std::thread::spawn(move || {
            while hb_stop_rx
                .recv_timeout(std::time::Duration::from_millis(2000))
                .is_err()
            {
                if let Ok(cancel_req) = hb_queue.heartbeat(&hb_jid, &hb_wid, 10) {
                    if cancel_req {
                        tracing::warn!(
                            "Heartbeat detected cancel request for job {} — terminating FFmpeg",
                            hb_jid
                        );
                        // Kill only this job's FFmpeg. This used to iterate
                        // the whole shared pid list, so cancelling one clip
                        // killed every concurrent encode (F-12).
                        if !crate::service_handle::kill_ffmpeg_for_job(
                            &hb_pids,
                            &hb_jid,
                            crate::service_handle::kill_process_tree,
                        ) {
                            tracing::warn!(
                                "Cancel requested for job {} but no FFmpeg pid was registered",
                                hb_jid
                            );
                        }
                        break;
                    }
                }
            }
        });

        let result = runner.run_transcode(
            tools,
            config,
            input_path,
            &probe_data,
            profile_id,
            &staged_output_path,
            &metadata_uuid,
            ptx,
            &job.id,
            Some(active_pids.clone()),
            &audio_policy,
            measured_loudness.as_ref(),
        );

        let _ = hb_stop_tx.send(());
        let _ = hb_thread.join();

        let is_cancelled = queue
            .get(&job.id)
            .map(|j| {
                j.cancel_requested
                    || j.phase == jobs::JobPhase::CancelRequested
                    || j.phase == jobs::JobPhase::Cancelled
            })
            .unwrap_or(false);

        if is_cancelled {
            tracing::info!("Job {} was cancelled by user request", job.id);
            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Cancelled,
                Some("Cancelled".into()),
                |j| {
                    j.error = Some("Cancelled by user".into());
                    j.error_category = Some("cancelled".into());
                },
            );
            publisher.cleanup_staging(&staged_output_path);
            let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
            return;
        }

        if result.success {
            wait_for_file_flush(&result.output_path, 5000);
        }

        let _ = queue.transition(
            &job.id,
            jobs::JobPhase::Validating,
            Some("Validating".into()),
            |_| {},
        );

        let mut validation_ok = false;
        let mut validation_error = String::new();
        let mut final_probe = None;

        let stderr_tail: Vec<String> = if result.success {
            Vec::new()
        } else {
            result.stderr_tail.clone()
        };
        if !stderr_tail.is_empty() {
            queue.update(&job.id, |j| {
                j.stderr_log = Some(stderr_tail.clone());
            });
        }

        if result.success {
            let file_ok = if result.output_path.exists() {
                if let Ok(metadata) = std::fs::metadata(&result.output_path) {
                    metadata.len() > 0
                } else {
                    false
                }
            } else {
                false
            };

            if !file_ok {
                validation_error = "Output file missing or 0 bytes".to_string();
            } else {
                if !try_acquire_output_lock(&result.output_path, 5, 400) {
                    tracing::warn!(
                        "Could not acquire exclusive lock on {:?} — proceeding to ffprobe validation",
                        result.output_path
                    );
                }
                match probe_with_retry(tools, &result.output_path, 3, 500) {
                    Ok(p) => match classify_probe_match(p, &probe_data) {
                        Ok(p) => {
                            validation_ok = true;
                            final_probe = Some(p);
                        }
                        Err(e) => validation_error = e,
                    },
                    Err(e) => validation_error = e,
                }
            }
        } else {
            validation_error = result
                .error
                .clone()
                .unwrap_or_else(|| "FFmpeg encoding failed".to_string());
        }

        if validation_ok {
            let output_probe = final_probe.unwrap();
            let total_frames = output_probe.frame_count;
            let fps = output_probe.fps();
            let duration_ms = (output_probe.duration_secs * 1000.0).round() as i64;
            let gop_frames = compute_gop_from_fps(fps);

            // One keyframe scan, not two. `-skip_frame nokey` still demuxes
            // every packet, so this reads the whole mezzanine; the safe start
            // is simply the first offset that scan already returned.
            let keyframe_offsets = extract_keyframe_offsets_ms(
                tools.ffprobe.to_str().unwrap_or(""),
                &result.output_path,
            );
            let keyframe_safe_start_ms = keyframe_offsets.first().copied().unwrap_or(0);
            let closed_gop_ok = verify_closed_gop(&keyframe_offsets, gop_frames, fps);
            let faststart_ok = verify_faststart(&result.output_path);

            let qc_report = run_qc_evaluation(
                &output_probe,
                &probe_data,
                closed_gop_ok,
                faststart_ok,
                measured_loudness.as_ref(),
                &audio_policy,
                &config.effective_validation_policy(),
            );

            let mezzanine_ok = qc_report.passed;
            let warnings_list: Vec<String> = qc_report
                .findings
                .iter()
                .filter(|f| {
                    f.severity == identity::Severity::Warning
                        || f.severity == identity::Severity::Error
                })
                .map(|f| f.code.clone())
                .collect();

            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Publishing,
                Some("Publishing".into()),
                |j| {
                    j.output_path = Some(final_output_path.to_string_lossy().into_owned());
                },
            );

            // Everything after a successful `publish` that can still fail has
            // the same remedy: get the orphaned mezzanine out of the library,
            // drop its sidecar, mark the row `error` and fail the job visibly
            // (T2-8). Leaving it in `videos/` is the silent loss F-18 is about.
            let fail_after_publish = |category: &str, message: &str| {
                match quarantine_published(target_root, &final_output_path) {
                    Ok(dest) => tracing::error!(
                        "Quarantined {} -> {} ({})",
                        final_output_path.display(),
                        dest.display(),
                        category
                    ),
                    Err(e) => tracing::error!(
                        "Could not quarantine {}: {} -- it is still in the library and \
                         nothing references it",
                        final_output_path.display(),
                        e
                    ),
                }
                let sidecar = identity::sidecar_path_for(&final_output_path);
                if sidecar.exists() {
                    if let Err(e) = std::fs::remove_file(&sidecar) {
                        tracing::warn!("Could not remove {}: {}", sidecar.display(), e);
                    }
                }
                let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
                let msg = message.to_string();
                let cat = category.to_string();
                let _ = queue.transition(
                    &job.id,
                    jobs::JobPhase::Failed,
                    Some("Failed".into()),
                    |j| {
                        j.error = Some(msg);
                        j.error_category = Some(cat);
                    },
                );
                queue.broadcast(
                    "failed",
                    &serde_json::json!({
                        "id": job.id,
                        "uuid": metadata_uuid,
                        "error": message,
                        "error_category": category,
                    })
                    .to_string(),
                );
                queue.prune_old(500);
            };

            let sha256 = compute_file_sha256(&staged_output_path).ok();
            let file_size_bytes = std::fs::metadata(&staged_output_path).ok().map(|m| m.len());

            // Checked again here (T2-9). The publish itself is a same-volume
            // rename and needs nothing, but the sidecar write does, and a
            // concurrent job may have filled the volume during this encode.
            // Failing now leaves a staged file to clean up; failing after a
            // half-written sidecar leaves a `ready` asset PlayOut cannot use.
            if let Err(e) = check_disk_space(target_root, DISK_FIXED_SLACK) {
                tracing::error!(
                    "Disk space ran out during the encode of {}: {}",
                    input_path.display(),
                    e
                );
                let _ = queue.transition(
                    &job.id,
                    jobs::JobPhase::Failed,
                    Some("Failed".into()),
                    |j| {
                        j.error = Some(e.clone());
                        j.error_category = Some("io_disk_full".into());
                    },
                );
                queue.broadcast(
                    "failed",
                    &serde_json::json!({"id": job.id, "error": e, "error_category": "io_disk_full"})
                        .to_string(),
                );
                let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
                publisher.cleanup_staging(&staged_output_path);
                queue.prune_old(500);
                return;
            }

            if let Err(e) = publisher.publish(&staged_output_path, &final_output_path) {
                tracing::error!("Atomic publish failed for {}: {}", input_path.display(), e);
                let _ = queue.transition(
                    &job.id,
                    jobs::JobPhase::Failed,
                    Some("Failed".into()),
                    |j| {
                        j.error = Some(format!("Atomic publish failed: {}", e));
                        j.error_category = Some("publish_failure".into());
                    },
                );
                queue.broadcast("failed", &serde_json::json!({"id": job.id, "error": format!("Atomic publish failed: {}", e)}).to_string());
                let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
                publisher.cleanup_staging(&staged_output_path);
                queue.prune_old(500);
                return;
            }

            let sidecar_loudness = measured_loudness.as_ref().map(|ml| identity::LoudnessInfo {
                integrated_lufs: ml.input_i,
                true_peak_dbtp: ml.input_tp,
                lra: ml.input_lra,
                threshold: ml.input_thresh,
                target_lufs: ml.target_i,
                target_true_peak_dbtp: ml.target_tp,
                normalization_mode: match audio_policy.mode {
                    config::AudioMode::EbuR128 => "ebu_r128".to_string(),
                    config::AudioMode::AtscA85 => "atsc_a85".to_string(),
                    _ => "legacy".to_string(),
                },
                linear_applied: ml.is_linear,
            });

            let validation_report = identity::ValidationReport {
                mezzanine_ok,
                duration_ms,
                fps,
                fps_num: output_probe.fps_num,
                fps_den: output_probe.fps_den,
                audio_sample_rate: output_probe.audio_sample_rate,
                audio_channels: output_probe.audio_channels,
                closed_gop: closed_gop_ok,
                faststart: faststart_ok,
                warnings: warnings_list.clone(),
                findings: Some(qc_report.findings.clone()),
                qc_report: Some(qc_report),
                sha256: sha256.clone(),
                file_size_bytes,
            };

            if let Err(e) = identity::write_sidecar_next_to_video_with_validation(
                &final_output_path,
                &metadata_uuid,
                &probe_data,
                &output_probe,
                &profile_name,
                "h264",
                &config.encoding.audio_codec,
                duration_ms,
                mezzanine_ok,
                fps,
                output_probe.fps_num,
                output_probe.fps_den,
                total_frames,
                gop_frames,
                keyframe_safe_start_ms,
                &warnings_list,
                sidecar_loudness,
                Some(validation_report),
                sha256,
                file_size_bytes,
            ) {
                // Used to log and continue to `mark_ready`, which published a
                // `ready` asset with no sidecar -- and the sidecar is the
                // contract PlayOut hydrates from, so the asset was broken in a
                // way only PlayOut would discover (F-18). Quarantine it.
                tracing::error!(
                    "Failed to write metadata sidecar for '{}': {}",
                    final_output_path.display(),
                    e
                );
                fail_after_publish(
                    "sidecar_write_failed",
                    "The mezzanine encoded but its sidecar could not be written",
                );
                return;
            }

            let keyframe_offsets_json =
                serde_json::to_string(&keyframe_offsets).unwrap_or_else(|_| "[]".to_string());

            // Retried, because this is the last write of a job that may have
            // cost an hour of CPU and a transient `database is locked` must not
            // throw it away.
            let ready = retry_blocking(
                3,
                std::time::Duration::from_millis(500),
                "mark_ready",
                |_| {
                    handle.block_on(db::mark_ready(
                        pool,
                        &metadata_uuid,
                        &final_output_path.to_string_lossy(),
                        duration_ms,
                        mezzanine_ok,
                        fps,
                        output_probe.fps_num,
                        output_probe.fps_den,
                        total_frames,
                        gop_frames,
                        keyframe_safe_start_ms,
                        &warnings_list,
                        &keyframe_offsets_json,
                    ))
                },
            );

            if let Err(e) = ready {
                // The file is encoded, validated and published, and the
                // registry will not record it. `let _ =` here left exactly that
                // on disk: a mezzanine in `videos/` that nothing references,
                // which the next ingest of the same source would collide with.
                tracing::error!(
                    uuid = %metadata_uuid,
                    path = %final_output_path.display(),
                    "mark_ready failed after 3 attempts: {} -- quarantining the mezzanine",
                    e
                );
                fail_after_publish(
                    "db_mark_ready_failed",
                    "The mezzanine encoded but the registry could not record it",
                );
                return;
            }

            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Completed,
                Some("Completed".into()),
                |j| {
                    j.uuid = Some(metadata_uuid.clone());
                    j.output_path = Some(final_output_path.to_string_lossy().into_owned());
                    j.progress = 100.0;
                },
            );

            let storage_policy = config.effective_storage_policy();
            let cleanup_result = validate_and_cleanup_source(
                input_path,
                Path::new(&config.paths.watch_folder),
                Path::new(&config.paths.target_folder),
                &final_output_path,
                initial_source_size,
                initial_source_mtime,
                Some(queue),
                Some(&job.id),
                storage_policy.clean_source_after_success,
            );

            if let Some(ref warn) = cleanup_result.warning {
                tracing::warn!("Source cleanup note for {}: {}", input_path.display(), warn);
            }

            queue.broadcast(
                "completed",
                &serde_json::json!({
                    "id": job.id,
                    "uuid": metadata_uuid,
                    "source_cleanup": cleanup_result,
                })
                .to_string(),
            );
            tracing::info!(
                "Completed and verified: {} -> {} (uuid={}, source_cleanup={:?})",
                input_path.display(),
                final_output_path.display(),
                metadata_uuid,
                cleanup_result
            );
            queue.prune_old(500);
            return;
        }

        last_error = validation_error.clone();
        let is_val_fail = result.success && !validation_ok;
        let retry_class = classify_error(&validation_error, is_val_fail);

        publisher.cleanup_staging(&staged_output_path);

        if retry_class == RetryClass::Retryable && attempt < max_attempts {
            tracing::warn!(
                "Transcode attempt {}/{} failed for {} ({}). Retrying in {}ms...",
                attempt,
                max_attempts,
                input_path.display(),
                validation_error,
                retry_delay_ms
            );
            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Recoverable,
                Some(format!("Retrying attempt {}/{}", attempt + 1, max_attempts)),
                |j| {
                    j.error = Some(validation_error.clone());
                    j.error_category = Some("retryable_error".into());
                },
            );
            queue.broadcast(
                "progress",
                &serde_json::json!({
                    "id": job.id,
                    "stage": format!("Retrying attempt {}/{}", attempt + 1, max_attempts),
                })
                .to_string(),
            );

            if retry_delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
            }
            attempt += 1;
        } else {
            tracing::error!(
                "Final transcode failure for {} (attempt {}/{}, class={:?}): {}",
                input_path.display(),
                attempt,
                max_attempts,
                retry_class,
                last_error
            );
            let err_cat = if is_val_fail {
                "validation_failure"
            } else {
                "transcode_failure"
            };
            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Failed,
                Some("Failed".into()),
                |j| {
                    j.error = Some(last_error.clone());
                    j.error_category = Some(err_cat.into());
                },
            );
            queue.broadcast(
                "failed",
                &serde_json::json!({"id": job.id, "error": last_error}).to_string(),
            );
            let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
            if final_output_path.exists() {
                let _ = std::fs::remove_file(&final_output_path);
            }
            break;
        }
    }

    queue.prune_old(500);
}

fn build_unique_output_path(video_dir: &Path, safe_stem: &str, uuid: &str) -> std::path::PathBuf {
    let base_name = if safe_stem.is_empty() {
        uuid.to_string()
    } else {
        format!("{}_{}", safe_stem, uuid)
    };
    let filename = format!("{}.mp4", base_name);
    let path = video_dir.join(&filename);

    if !path.exists() {
        return path;
    }

    for _ in 0..3 {
        let new_uuid = Uuid::new_v4().to_string();
        let new_name = if safe_stem.is_empty() {
            format!("{}.mp4", new_uuid)
        } else {
            format!("{}_{}.mp4", safe_stem, new_uuid)
        };
        let candidate = video_dir.join(&new_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    let ts = chrono::Utc::now().timestamp_millis();
    let fallback = format!("{}_{}_{}.mp4", safe_stem, uuid, ts);
    video_dir.join(fallback)
}

fn compute_gop_from_fps(fps: f64) -> i64 {
    let gop = (fps * 2.0).round() as i64;
    if gop > 0 {
        gop
    } else {
        50
    }
}

fn verify_closed_gop(keyframe_offsets: &[i64], gop_frames: i64, fps: f64) -> bool {
    if keyframe_offsets.len() < 2 {
        return true;
    }
    if fps <= 0.0 || gop_frames <= 0 {
        return true;
    }
    let frame_ms = 1000.0 / fps;
    let gop_ms = frame_ms * gop_frames as f64;
    let tolerance = frame_ms * 0.5;

    for window in keyframe_offsets.windows(2) {
        let diff = (window[1] - window[0]) as f64;
        if (diff - gop_ms).abs() > tolerance && diff < gop_ms - tolerance {
            return false;
        }
    }
    true
}

fn verify_faststart(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };

    let Ok(file_size) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    if file_size < 16 {
        return false;
    }

    let mut buf = [0u8; 16];
    if file.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    if file.read_exact(&mut buf).is_err() {
        return false;
    }

    let moov_in_first_64k = {
        let mut scan_buf = vec![0u8; 65536.min(file_size as usize)];
        if file.seek(SeekFrom::Start(0)).is_err() {
            return false;
        }
        if file.read(&mut scan_buf).is_err() {
            return false;
        }
        scan_buf.windows(4).any(|w| w == b"moov")
    };

    moov_in_first_64k
}

/// Try to acquire an exclusive write lock on the freshly-written mezzanine. Returns true on
/// success. On Windows, antivirus and the search indexer often hold a brief read-only handle to
/// the new file; we retry a handful of times with short backoff so those can release it before
/// we give up. Failure to acquire is **advisory only** — the caller should still attempt ffprobe
/// validation, which is the real test.
fn try_acquire_output_lock(path: &Path, attempts: u32, delay_ms: u64) -> bool {
    for attempt in 0..attempts {
        #[cfg(target_os = "windows")]
        let res = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .share_mode(0)
                .open(path)
        };
        #[cfg(not(target_os = "windows"))]
        let res = std::fs::OpenOptions::new().write(true).open(path);

        match res {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.raw_os_error() == Some(32) /* ERROR_SHARING_VIOLATION */ =>
            {
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
            Err(_) => {
                // Any other error (not found, etc.) — no point retrying.
                return false;
            }
        }
    }
    false
}

/// Attempt ffprobe with a small retry loop. Returns the first successful result, or the last
/// error. Many transient output-file issues (av touching, moov atom still flushed) clear within
/// a second or two.
fn probe_with_retry(
    tools: &bootstrap::ToolPaths,
    path: &Path,
    attempts: u32,
    delay_ms: u64,
) -> Result<probe::ProbeData, String> {
    let mut last_err = String::new();
    for attempt in 0..attempts {
        match probe::probe_media(tools, path) {
            Ok(p) => return Ok(p),
            Err(e) => {
                last_err = e;
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
    }
    // Best-effort diagnostic: if ffprobe produced an empty error string, surface a more useful
    // message pointing the operator to where the real diagnostic would be.
    if last_err.trim() == "ffprobe failed:" || last_err.trim().is_empty() {
        Err(format!("ffprobe returned an error without message (output may be premature or unreadable); see logs"))
    } else {
        Err(last_err)
    }
}

/// Evaluate the encoded mezzanine against the operator's validation policy
/// (T3-3, F-29).
///
/// `validation_policy.*` used to be pure decoration: `GET /api/config` served
/// the knobs, the UI rendered them, `PUT /api/config` stored them, and nothing
/// ever read them. Every check here was unconditionally blocking. An operator
/// who turned off `enforce_faststart` because their downstream did not care
/// still had every such asset marked `mezzanine_ok = false`.
///
/// Each `enforce_*` now chooses **severity**, not whether the check runs: the
/// finding is always recorded, so the sidecar and the DB viewer still say what
/// was observed. Turning one off downgrades it from blocking to a warning; it
/// never hides it.
pub fn run_qc_evaluation(
    output_probe: &probe::ProbeData,
    source_probe: &probe::ProbeData,
    closed_gop_ok: bool,
    faststart_ok: bool,
    measured_loudness: Option<&probe::MeasuredLoudness>,
    _audio_policy: &config::AudioPolicy,
    policy: &config::ValidationPolicy,
) -> identity::QcReport {
    let mut findings = Vec::new();
    let mut blocking_errors = 0;
    let mut warnings_count = 0;

    // Records a finding at error severity when the operator enforces this rule,
    // and at warning severity when they do not.
    let push = |enforced: bool,
                    code: &str,
                    message: &str,
                    observed: Option<String>,
                    expected: Option<String>,
                    findings: &mut Vec<identity::ValidationFinding>,
                    blocking: &mut usize,
                    warnings: &mut usize| {
        if enforced {
            findings.push(identity::ValidationFinding::error(
                code, message, observed, expected,
            ));
            *blocking += 1;
        } else {
            findings.push(identity::ValidationFinding::warning(
                code, message, observed, expected,
            ));
            *warnings += 1;
        }
    };

    // 1. Duration check
    let duration_ms = (output_probe.duration_secs * 1000.0).round() as i64;
    if duration_ms <= 0 {
        findings.push(identity::ValidationFinding::error(
            "zero_duration",
            "Output duration must be greater than zero",
            Some(format!("{} ms", duration_ms)),
            Some("> 0 ms".to_string()),
        ));
        blocking_errors += 1;
    }

    // 2. FPS check
    let fps = output_probe.fps();
    let expected_fps = profiles::TARGET_FPS_NUM as f64 / profiles::TARGET_FPS_DEN as f64;
    if (fps - expected_fps).abs() > 0.01 {
        findings.push(identity::ValidationFinding::error(
            "fps_mismatch",
            "Output FPS does not match target broadcast standard",
            Some(format!(
                "{:.3} fps ({}/{})",
                fps, output_probe.fps_num, output_probe.fps_den
            )),
            Some(format!("{:.3} fps", expected_fps)),
        ));
        blocking_errors += 1;
    }

    let source_fps = source_probe.fps();
    if (source_fps - expected_fps).abs() > 0.01 {
        findings.push(identity::ValidationFinding::warning(
            "fps_converted",
            "Input frame rate was converted to match output profile standard",
            Some(format!("{:.3} fps", source_fps)),
            Some(format!("{:.3} fps", expected_fps)),
        ));
        warnings_count += 1;
    }

    // 3. Audio sample rate check
    if output_probe.audio_sample_rate != 48000 {
        push(
            policy.enforce_48k_audio,
            "audio_sample_rate_not_48k",
            "Output audio sample rate must be exactly 48000 Hz",
            Some(format!("{} Hz", output_probe.audio_sample_rate)),
            Some("48000 Hz".to_string()),
            &mut findings,
            &mut blocking_errors,
            &mut warnings_count,
        );
    }

    // 4. Closed GOP check
    if !closed_gop_ok {
        push(
            policy.enforce_closed_gop,
            "closed_gop_violation",
            "Keyframe structure does not satisfy closed GOP cadence requirements",
            Some("irregular GOP detected".to_string()),
            Some("strict closed GOP with 2s interval".to_string()),
            &mut findings,
            &mut blocking_errors,
            &mut warnings_count,
        );
    }

    // 5. Faststart check
    if !faststart_ok {
        push(
            policy.enforce_faststart,
            "missing_faststart",
            "MP4 moov atom is not at the beginning of the file (faststart missing)",
            Some("moov atom not in first 64KB".to_string()),
            Some("+faststart enabled".to_string()),
            &mut findings,
            &mut blocking_errors,
            &mut warnings_count,
        );
    }

    // 6. Audio loudness checks
    if let Some(ml) = measured_loudness {
        if ml.is_silent {
            findings.push(identity::ValidationFinding::warning(
                "silent_audio_loudness_skipped",
                "Input audio is silent or near-silence; loudness normalization skipped",
                Some(format!("{:.1} LUFS", ml.input_i)),
                None,
            ));
            warnings_count += 1;
        } else if ml.is_short {
            findings.push(identity::ValidationFinding::warning(
                "short_clip_loudnorm_dynamic",
                "Clip duration under 3 seconds; dynamic loudnorm mode applied",
                Some(format!("{:.2} s", source_probe.duration_secs)),
                Some(">= 3.0 s for linear mode".to_string()),
            ));
            warnings_count += 1;
        }
    }

    // 7. Duration drift against the source.
    //
    // There was no such check at all before T3-3, despite
    // `max_duration_delta_ms` being an advertised, validated, UI-rendered knob.
    // A mezzanine silently a second short of its source is exactly the failure
    // an as-run log catches at transmission and nobody catches before it.
    let source_ms = (source_probe.duration_secs * 1000.0).round() as i64;
    if source_ms > 0 && duration_ms > 0 {
        let delta = (duration_ms - source_ms).abs();
        if delta > policy.max_duration_delta_ms {
            push(
                true,
                "duration_delta_exceeded",
                "Output duration differs from the source by more than the configured tolerance",
                Some(format!("{} ms drift", delta)),
                Some(format!("<= {} ms", policy.max_duration_delta_ms)),
                &mut findings,
                &mut blocking_errors,
                &mut warnings_count,
            );
        }
    }

    // `strict_ready_blocking` promotes warnings to blocking. For a station that
    // will not air anything with an open question against it, "passed with
    // warnings" is not a state they want in the library.
    let passed = if policy.strict_ready_blocking {
        blocking_errors == 0 && warnings_count == 0
    } else {
        blocking_errors == 0
    };

    identity::QcReport {
        passed,
        blocking_errors,
        warnings_count,
        findings,
    }
}

pub fn check_disk_space(target_dir: &Path, min_bytes_required: u64) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = target_dir.as_os_str().encode_wide().collect();
        wide.push(0);

        let mut free_bytes_available: u64 = 0;
        let mut total_number_of_bytes: u64 = 0;
        let mut total_number_of_free_bytes: u64 = 0;

        unsafe {
            let res = windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_bytes_available,
                &mut total_number_of_bytes,
                &mut total_number_of_free_bytes,
            );
            if res != 0 && free_bytes_available < min_bytes_required {
                return Err(format!(
                    "Insufficient disk space on target volume: {} MB available, {} MB required",
                    free_bytes_available / (1024 * 1024),
                    min_bytes_required / (1024 * 1024)
                ));
            }
        }
    }
    let _ = (target_dir, min_bytes_required);
    Ok(())
}

pub fn compute_file_sha256(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn cleanup_orphan_staging_files(target_dir: &Path, max_age_secs: u64) -> usize {
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(target_dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let filename = path.file_name().unwrap_or_default().to_string_lossy();
            if crate::watcher::is_temp_file_name(&path)
                || filename.starts_with(".tmp_")
                || filename.ends_with(".tmp_json")
            {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(age) = now.duration_since(modified) {
                            if age.as_secs() >= max_age_secs {
                                if std::fs::remove_file(&path).is_ok() {
                                    tracing::info!(
                                        "Cleaned up orphaned staging file: {}",
                                        path.display()
                                    );
                                    removed += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    removed
}

/// Validate that the probed mezzanine matches the source expectation: video + audio streams
/// present, and duration within tolerance of the source (a couple of frames).
fn classify_probe_match(
    p: probe::ProbeData,
    source: &probe::ProbeData,
) -> Result<probe::ProbeData, String> {
    if p.width == 0 || p.height == 0 {
        return Err("No valid video stream in output".into());
    }
    if p.audio_codec == "none" {
        // Audio missing is non-fatal for some source media (clean switches). Warn instead.
        tracing::warn!(
            "Output has no audio stream (source had codec={})",
            source.audio_codec
        );
    }
    let output_duration = p.duration_secs;
    let source_duration = source.duration_secs;
    let fps = p.fps();
    let frame_duration_ms = if fps > 0.0 {
        (1000.0 / fps).round() as f64
    } else {
        40.0
    };
    let tolerance_ms = (frame_duration_ms * 2.0).max(1200.0);
    let diff_ms = ((output_duration - source_duration).abs() * 1000.0).round() as f64;
    if diff_ms > tolerance_ms {
        return Err(format!(
            "Duration mismatch: source={:.3}s output={:.3}s (diff={}ms, tolerance={}ms)",
            source_duration, output_duration, diff_ms, tolerance_ms
        ));
    }
    Ok(p)
}

fn wait_for_file_flush(path: &Path, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    let mut last_size = None;
    let mut stable_count = 0;
    loop {
        let size = std::fs::metadata(path)
            .map(|m| m.len())
            .ok()
            .filter(|&s| s > 0);

        match (size, last_size) {
            (Some(s), Some(prev)) if s == prev => {
                stable_count += 1;
                if stable_count >= 3 {
                    return true;
                }
            }
            (Some(s), _) => {
                stable_count = 1;
                last_size = Some(s);
            }
            (None, _) => {
                stable_count = 0;
                last_size = None;
            }
        }

        if start.elapsed().as_millis() as u64 > timeout_ms {
            tracing::warn!("Timeout waiting for file flush on {:?}", path);
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn extract_keyframe_offsets_ms(ffprobe: &str, path: &Path) -> Vec<i64> {
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;
    #[cfg(target_os = "windows")]
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    #[cfg(target_os = "windows")]
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

    let mut cmd = std::process::Command::new(ffprobe);
    cmd.args(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-skip_frame",
        "nokey",
        "-show_entries",
        "frame=pts_time",
        "-of",
        "csv=p=0",
    ]);
    cmd.arg(path);

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            tracing::error!("Failed to execute ffprobe for keyframe scanning: {}", e);
            return Vec::new();
        }
    };

    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        tracing::error!("ffprobe keyframe scanning failed: {}", err_msg);
        return Vec::new();
    }

    parse_keyframe_pts_ms(&String::from_utf8_lossy(&output.stdout))
}

/// Turn ffprobe's `frame=pts_time` CSV into milliseconds.
///
/// Split out so the offsets list and the safe-start value can never be
/// produced by two subtly different parsers: the safe start is just the first
/// element of this list.
fn parse_keyframe_pts_ms(stdout: &str) -> Vec<i64> {
    stdout
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && *line != "N/A")
        .filter_map(|line| line.parse::<f64>().ok())
        .map(|t| (t * 1000.0).round() as i64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::LoudnessMeasurer;
    use std::path::PathBuf;

    /// SB-02: the safe start is now the first element of the offsets list
    /// instead of a second full scan of the mezzanine. The two must agree on
    /// every shape of ffprobe output, including the empty and `N/A` lines the
    /// old pair of parsers each skipped separately.
    #[test]
    fn the_safe_start_equals_the_first_keyframe_offset() {
        // What the old probe::get_keyframe_safe_start_ms did: first line that
        // is neither empty nor N/A and parses as a float, rounded to ms.
        fn legacy_safe_start(stdout: &str) -> i64 {
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed == "N/A" {
                    continue;
                }
                if let Ok(t_sec) = trimmed.parse::<f64>() {
                    return (t_sec * 1000.0).round() as i64;
                }
            }
            0
        }

        for stdout in [
            "0.000000\n2.000000\n4.000000\n",
            "N/A\n\n0.040000\n2.040000\n",
            "  1.500000  \n3.000000\n",
            "0.083333\n",
            "",
            "N/A\nN/A\n",
            "garbage\n1.000000\n",
        ] {
            let offsets = parse_keyframe_pts_ms(stdout);
            assert_eq!(
                offsets.first().copied().unwrap_or(0),
                legacy_safe_start(stdout),
                "diverged on {:?}",
                stdout
            );
        }

        // And the rounding is the one written to the DB and the sidecar.
        assert_eq!(parse_keyframe_pts_ms("0.083333\n"), vec![83]);
        assert_eq!(parse_keyframe_pts_ms("2.0405\n"), vec![2041]);
    }

    #[test]
    fn test_verify_closed_gop_uniform() {
        let offsets = vec![0, 2000, 4000, 6000];
        assert!(verify_closed_gop(&offsets, 50, 25.0));
    }

    #[test]
    fn test_verify_closed_gop_violation() {
        let offsets = vec![0, 1000, 2000, 4000];
        assert!(!verify_closed_gop(&offsets, 50, 25.0));
    }

    #[test]
    fn test_compute_gop_from_fps() {
        assert_eq!(compute_gop_from_fps(25.0), 50);
        assert_eq!(compute_gop_from_fps(29.97), 60);
    }

    #[test]
    fn test_local_file_publisher_stage_path() {
        let publ = LocalFilePublisher;
        let final_path = Path::new("C:/target/videos/clip1_uuid1.mp4");
        let staged = publ.stage_path(final_path, "uuid1");
        assert_eq!(
            staged,
            Path::new("C:/target/videos/.tmp_uuid1_clip1_uuid1.mp4")
        );
    }

    #[test]
    fn test_local_file_publisher_atomic_rename_success() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir().join("pt_v2_2a_test_rename");
        let _ = std::fs::create_dir_all(&temp_dir);

        let publ = LocalFilePublisher;
        let final_path = temp_dir.join("final_clip.mp4");
        let staged_path = publ.stage_path(&final_path, "test1234");

        let mut file = std::fs::File::create(&staged_path).unwrap();
        writeln!(file, "dummy video content").unwrap();
        drop(file);

        assert!(staged_path.exists());
        assert!(!final_path.exists());

        let res = publ.publish(&staged_path, &final_path);
        assert!(res.is_ok());
        assert!(!staged_path.exists());
        assert!(final_path.exists());

        let _ = std::fs::remove_file(&final_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_local_file_publisher_fails_if_final_exists() {
        let temp_dir = std::env::temp_dir().join("pt_v2_2a_test_conflict");
        let _ = std::fs::create_dir_all(&temp_dir);

        let publ = LocalFilePublisher;
        let final_path = temp_dir.join("existing_clip.mp4");
        let staged_path = publ.stage_path(&final_path, "test5678");

        std::fs::File::create(&staged_path).unwrap();
        std::fs::File::create(&final_path).unwrap();

        let res = publ.publish(&staged_path, &final_path);
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .contains("Final output path already exists"));

        let _ = std::fs::remove_file(&staged_path);
        let _ = std::fs::remove_file(&final_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_classify_error_categories() {
        assert_eq!(
            classify_error("Permission denied (os error 32) / file locked", false),
            RetryClass::Retryable
        );
        assert_eq!(
            classify_error("ffmpeg exited with code 1", false),
            RetryClass::Retryable
        );
        assert_eq!(
            classify_error("fps_mismatch: got 30.0 expected 25.0", true),
            RetryClass::Permanent
        );
        assert_eq!(
            classify_error("Probe: invalid media header", false),
            RetryClass::Permanent
        );
        assert_eq!(
            classify_error("Job cancelled by user", false),
            RetryClass::Cancelled
        );
        assert_eq!(
            classify_error("No such file or directory", false),
            RetryClass::Permanent
        );
    }

    struct MockTranscodeRunner {
        responses: std::sync::Mutex<Vec<encoder::EncodeResult>>,
        attempts_seen: std::sync::atomic::AtomicUsize,
    }

    impl TranscodeRunner for MockTranscodeRunner {
        fn run_transcode(
            &self,
            _tools: &bootstrap::ToolPaths,
            _config: &config::AppConfig,
            _input_path: &Path,
            _source_probe: &probe::ProbeData,
            _profile_id: profiles::ProfileId,
            output_path: &Path,
            _metadata_uuid: &str,
            _progress_tx: std::sync::mpsc::Sender<encoder::EncodeProgress>,
            _job_id: &str,
            _active_pids: Option<crate::service_handle::ActivePids>,
            _audio_policy: &config::AudioPolicy,
            _measured_loudness: Option<&probe::MeasuredLoudness>,
        ) -> encoder::EncodeResult {
            self.attempts_seen
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut list = self.responses.lock().unwrap();
            if !list.is_empty() {
                let res = list.remove(0);
                if res.success {
                    let _ = std::fs::File::create(output_path);
                }
                res
            } else {
                encoder::EncodeResult {
                    output_path: output_path.to_path_buf(),
                    success: false,
                    error: Some("Mock empty response".into()),
                    stderr_tail: Vec::new(),
                    exit_pid: None,
                }
            }
        }
    }

    struct MockLoudnessMeasurer {
        measurements: std::sync::Mutex<Vec<Result<Option<probe::MeasuredLoudness>, String>>>,
        calls_seen: std::sync::atomic::AtomicUsize,
    }

    impl probe::LoudnessMeasurer for MockLoudnessMeasurer {
        fn measure_loudness(
            &self,
            _tools: &bootstrap::ToolPaths,
            _input_path: &Path,
            _channels: i64,
            _duration_secs: f64,
            _policy: &config::AudioPolicy,
        ) -> Result<Option<probe::MeasuredLoudness>, String> {
            self.calls_seen
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut list = self.measurements.lock().unwrap();
            if !list.is_empty() {
                list.remove(0)
            } else {
                Ok(None)
            }
        }
    }

    #[test]
    fn test_max_attempts_boundary_semantics() {
        let mut cfg = config::AppConfig::default();
        cfg.retry_policy_v2 = Some(config::RetryPolicyV2 {
            max_attempts: 1,
            retry_delay_ms: 10,
            auto_retry_on_start: false,
        });

        let policy = cfg.effective_retry_policy();
        assert_eq!(policy.max_attempts, 1);
        let max_att = (policy.max_attempts as usize).max(1);
        assert_eq!(max_att, 1);
    }

    /// A queue, pool and watch/target tree for driving `process_file_sync`.
    async fn adoption_fixture(
        tag: &str,
    ) -> (
        jobs::JobQueue,
        SqlitePool,
        config::AppConfig,
        std::path::PathBuf,
    ) {
        let root = std::env::temp_dir().join(format!(
            "pt-adopt-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::new_v4()
        ));
        let watch = root.join("watch");
        let target = root.join("target");
        std::fs::create_dir_all(&watch).expect("watch dir");
        std::fs::create_dir_all(&target).expect("target dir");

        let pool = crate::db::init_pool(&root.join("jobs.db"))
            .await
            .expect("init pool");
        let (event_tx, _rx) = tokio::sync::broadcast::channel::<String>(16);
        let queue = jobs::JobQueue::new(event_tx, None);

        let mut cfg = config::AppConfig::default();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = target.to_string_lossy().to_string();

        (queue, pool, cfg, root)
    }

    /// A job in the state a manual retry leaves behind: re-queued and pending.
    fn requeued_job(input: &std::path::Path) -> jobs::JobRecord {
        let mut job = jobs::JobRecord::new(&input.to_string_lossy(), "pending");
        job.transition_to(jobs::JobPhase::Probing, None).unwrap();
        job.transition_to(jobs::JobPhase::Failed, Some("Failed".into()))
            .unwrap();
        job.transition_to(jobs::JobPhase::Queued, Some("Re-queued (manual retry)".into()))
            .unwrap();
        job.attempt = 2;
        job
    }

    #[tokio::test]
    async fn a_retry_whose_source_vanished_closes_its_own_job() {
        // The ghost F-13 describes: the retry cannot run, and before T2-4
        // nothing ever moved the re-queued record off Pending, so it sat in
        // /api/jobs forever as outstanding work.
        let (queue, pool, cfg, root) = adoption_fixture("gone").await;
        let missing = std::path::Path::new(&cfg.paths.watch_folder).join("vanished.mov");

        let job = requeued_job(&missing);
        let id = job.id.clone();
        queue.push(job.clone());

        process_file_sync(
            &queue,
            &bootstrap::ToolPaths {
                ffmpeg: std::path::PathBuf::new(),
                ffprobe: std::path::PathBuf::new(),
            },
            std::path::Path::new(&cfg.paths.target_folder),
            &missing,
            &cfg,
            &pool,
            crate::service_handle::ActivePids::default(),
            Some(job),
        );

        let all = queue.all();
        assert_eq!(all.len(), 1, "a second, ghost job record was created");
        let after = queue.get(&id).expect("the adopted job is still there");
        assert_eq!(after.phase, jobs::JobPhase::Failed);
        assert_eq!(after.state, jobs::JobState::Failed);
        assert_eq!(after.error_category.as_deref(), Some("fingerprint_failure"));
        assert_eq!(after.attempt, 2, "the attempt count was reset");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn an_input_outside_the_watch_folder_closes_an_adopted_job() {
        let (queue, pool, cfg, root) = adoption_fixture("outside").await;
        // Real file, but not under the watch folder: the traversal guard trips.
        let outside = root.join("elsewhere.mov");
        std::fs::write(&outside, b"not media").expect("write fixture");

        let job = requeued_job(&outside);
        let id = job.id.clone();
        queue.push(job.clone());

        process_file_sync(
            &queue,
            &bootstrap::ToolPaths {
                ffmpeg: std::path::PathBuf::new(),
                ffprobe: std::path::PathBuf::new(),
            },
            std::path::Path::new(&cfg.paths.target_folder),
            &outside,
            &cfg,
            &pool,
            crate::service_handle::ActivePids::default(),
            Some(job),
        );

        let after = queue.get(&id).expect("job still present");
        assert_eq!(after.phase, jobs::JobPhase::Failed);
        assert_eq!(
            after.error_category.as_deref(),
            Some("path_outside_watch_folder")
        );
        assert_eq!(queue.all().len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_fresh_ingest_that_fails_early_still_leaves_a_visible_job() {
        // T2-4 asserted the opposite -- that a fresh ingest creating no record
        // was correct. T2-6 changed that deliberately: a file the watcher
        // offered and the service then rejected used to vanish with no job, no
        // event and nothing in /api/jobs, so an operator saw their file
        // disappear with no explanation (F-26).
        let (queue, pool, cfg, root) = adoption_fixture("fresh").await;
        let missing = std::path::Path::new(&cfg.paths.watch_folder).join("nope.mov");

        process_file_sync(
            &queue,
            &bootstrap::ToolPaths {
                ffmpeg: std::path::PathBuf::new(),
                ffprobe: std::path::PathBuf::new(),
            },
            std::path::Path::new(&cfg.paths.target_folder),
            &missing,
            &cfg,
            &pool,
            crate::service_handle::ActivePids::default(),
            None,
        );

        let all = queue.all();
        assert_eq!(all.len(), 1, "the rejection must be visible as a job");
        assert_eq!(all[0].phase, jobs::JobPhase::Failed);
        assert_eq!(all[0].input_path, missing.to_string_lossy());
        assert!(
            all[0].error_category.is_some(),
            "a visible failure must say why: {:?}",
            all[0]
        );
        assert!(
            all[0].finished_at.is_some(),
            "a terminal job must carry a finish time"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- T3-3: the validation policy knobs actually do something (F-29) ----

    fn probe_at(duration_secs: f64, sample_rate: i64) -> probe::ProbeData {
        probe::ProbeData {
            duration_secs,
            frame_count: (duration_secs * 25.0) as i64,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: sample_rate,
            audio_channels: 2,
            fps_num: crate::profiles::TARGET_FPS_NUM,
            fps_den: crate::profiles::TARGET_FPS_DEN,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "input.mp4".into(),
        }
    }

    fn qc(
        closed_gop_ok: bool,
        faststart_ok: bool,
        sample_rate: i64,
        policy: &config::ValidationPolicy,
    ) -> identity::QcReport {
        let out = probe_at(10.0, sample_rate);
        let src = probe_at(10.0, 48000);
        run_qc_evaluation(
            &out,
            &src,
            closed_gop_ok,
            faststart_ok,
            None,
            &config::AudioPolicy::default(),
            policy,
        )
    }

    #[test]
    fn with_the_default_policy_every_check_blocks() {
        let p = config::ValidationPolicy::default();
        assert!(!qc(false, true, 48000, &p).passed, "closed GOP");
        assert!(!qc(true, false, 48000, &p).passed, "faststart");
        assert!(!qc(true, true, 44100, &p).passed, "sample rate");
        assert!(qc(true, true, 48000, &p).passed, "a clean encode passes");
    }

    #[test]
    fn turning_off_enforcement_downgrades_the_finding_rather_than_hiding_it() {
        // The distinction that matters: an operator whose downstream does not
        // care about faststart should not have every asset marked unusable --
        // but the observation must still reach the sidecar and the DB viewer,
        // or nobody can answer "was this file actually faststart?" later.
        let p = config::ValidationPolicy {
            enforce_faststart: false,
            ..Default::default()
        };

        let report = qc(true, false, 48000, &p);
        assert!(report.passed, "must no longer block");
        assert_eq!(report.blocking_errors, 0);
        assert!(report.warnings_count >= 1, "but it must still be recorded");
        assert!(
            report.findings.iter().any(|f| f.code == "missing_faststart"),
            "the finding must survive the downgrade: {:?}",
            report.findings
        );
    }

    #[test]
    fn each_enforcement_flag_governs_only_its_own_check() {
        let p = config::ValidationPolicy {
            enforce_closed_gop: false,
            ..Default::default()
        };
        // GOP is waived, faststart is not.
        assert!(qc(false, true, 48000, &p).passed);
        assert!(!qc(false, false, 48000, &p).passed);

        let p = config::ValidationPolicy {
            enforce_48k_audio: false,
            ..Default::default()
        };
        assert!(qc(true, true, 44100, &p).passed);
        assert!(!qc(false, true, 44100, &p).passed);
    }

    #[test]
    fn duration_drift_beyond_the_tolerance_blocks() {
        // There was no duration-delta check at all before T3-3, despite
        // `max_duration_delta_ms` being advertised, validated and rendered in
        // the UI. A mezzanine a second short of its source is the failure an
        // as-run log catches at transmission and nothing catches before it.
        let policy = config::ValidationPolicy::default(); // 80 ms
        let src = probe_at(10.0, 48000);

        let within = run_qc_evaluation(
            &probe_at(10.05, 48000),
            &src,
            true,
            true,
            None,
            &config::AudioPolicy::default(),
            &policy,
        );
        assert!(within.passed, "50 ms of drift is inside the 80 ms tolerance");

        let beyond = run_qc_evaluation(
            &probe_at(11.0, 48000),
            &src,
            true,
            true,
            None,
            &config::AudioPolicy::default(),
            &policy,
        );
        assert!(!beyond.passed, "a full second of drift must block");
        assert!(beyond
            .findings
            .iter()
            .any(|f| f.code == "duration_delta_exceeded"));
    }

    #[test]
    fn a_wider_tolerance_admits_drift_a_narrow_one_rejects() {
        let src = probe_at(10.0, 48000);
        let out = probe_at(10.5, 48000); // 500 ms

        let narrow = config::ValidationPolicy {
            max_duration_delta_ms: 80,
            ..Default::default()
        };
        let wide = config::ValidationPolicy {
            max_duration_delta_ms: 1000,
            ..Default::default()
        };

        let audio = config::AudioPolicy::default();
        assert!(!run_qc_evaluation(&out, &src, true, true, None, &audio, &narrow).passed);
        assert!(run_qc_evaluation(&out, &src, true, true, None, &audio, &wide).passed);
    }

    #[test]
    fn strict_ready_blocking_promotes_warnings_to_failures() {
        // For a station that will not air anything with an open question
        // against it, "passed with warnings" is not a state they want.
        // enforce_faststart off produces a warning, not an error.
        let lenient = config::ValidationPolicy {
            enforce_faststart: false,
            ..Default::default()
        };
        assert!(qc(true, false, 48000, &lenient).passed);

        let strict_policy = config::ValidationPolicy {
            strict_ready_blocking: true,
            ..lenient
        };
        let strict = qc(true, false, 48000, &strict_policy);
        assert!(!strict.passed, "a warning now blocks");
        assert_eq!(strict.blocking_errors, 0, "it is still a warning, not an error");
        assert!(strict.warnings_count >= 1);
    }

    // ---- T2-9: the preflight has to know how big this job is ----

    #[test]
    fn ffmpeg_rate_strings_parse_to_bits_per_second() {
        assert_eq!(parse_ffmpeg_rate_bps("15M"), Some(15_000_000));
        assert_eq!(parse_ffmpeg_rate_bps("15m"), Some(15_000_000));
        assert_eq!(parse_ffmpeg_rate_bps("320k"), Some(320_000));
        assert_eq!(parse_ffmpeg_rate_bps("320K"), Some(320_000));
        assert_eq!(parse_ffmpeg_rate_bps("1G"), Some(1_000_000_000));
        assert_eq!(parse_ffmpeg_rate_bps("4500000"), Some(4_500_000));
        assert_eq!(parse_ffmpeg_rate_bps(" 8M "), Some(8_000_000));
        assert_eq!(parse_ffmpeg_rate_bps("1.5M"), Some(1_500_000));

        // Unparseable, which must be `None` rather than 0 -- the caller
        // distinguishes them.
        assert_eq!(parse_ffmpeg_rate_bps(""), None);
        assert_eq!(parse_ffmpeg_rate_bps("fast"), None);
        assert_eq!(parse_ffmpeg_rate_bps("-5M"), None);
    }

    #[test]
    fn a_two_hour_feature_needs_far_more_than_the_old_flat_500mb() {
        // The case from F-22: 2 h at 15 Mbit/s video + 320 kbit/s audio.
        // 7200 s x 15.32 Mbit/s / 8 = ~13.8 GB, x1.2 margin = ~16.5 GB.
        let required = required_bytes_for(7200.0, "15M", "320k");
        let gb = required as f64 / 1_000_000_000.0;
        assert!(
            (16.0..17.5).contains(&gb),
            "expected about 16.5 GB, got {:.2} GB",
            gb
        );
        assert!(
            required > 30 * MIN_FREE_BYTES,
            "the old flat floor was off by more than an order of magnitude"
        );
    }

    #[test]
    fn an_hour_at_profile_a_rates_sizes_correctly() {
        // 3600 s x (15 Mbit/s + 320 kbit/s) / 8 = 6.894 GB, x1.2 = 8.273 GB,
        // + 64 MiB = 8.34 GB.
        //
        // REMEDIATION-PLAN.md quotes "~7.1 GB" for this case. That figure is a
        // slip -- no combination of the margin, the slack and GB-vs-GiB
        // produces it -- so the arithmetic above is what this asserts. Sizing
        // it *lower* than reality is the one direction that reintroduces F-22.
        let bytes = required_bytes_for(3600.0, "15M", "320k");
        let gb = bytes as f64 / 1_000_000_000.0;
        assert!((8.2..8.5).contains(&gb), "expected ~8.34 GB, got {:.2} GB", gb);
        assert_eq!(bytes, (3600.0 * 15_320_000.0 / 8.0 * 1.2) as u64 + DISK_FIXED_SLACK);
    }

    #[test]
    fn a_short_clip_never_falls_below_the_floor() {
        // 10 s at 15 Mbit/s is ~22 MB, well under the floor -- but the staged
        // file, the sidecar and FFmpeg's scratch still need room.
        assert_eq!(required_bytes_for(10.0, "15M", "320k"), MIN_FREE_BYTES);
    }

    #[test]
    fn an_unknown_duration_or_rate_falls_back_to_the_floor() {
        // Refusing to encode because a bitrate string was unfamiliar would be
        // worse than a preflight that is occasionally optimistic.
        assert_eq!(required_bytes_for(0.0, "15M", "320k"), MIN_FREE_BYTES);
        assert_eq!(required_bytes_for(-1.0, "15M", "320k"), MIN_FREE_BYTES);
        assert_eq!(required_bytes_for(3600.0, "", ""), MIN_FREE_BYTES);
        assert_eq!(required_bytes_for(3600.0, "veryfast", "nope"), MIN_FREE_BYTES);
    }

    #[test]
    fn an_absurd_duration_does_not_overflow() {
        // A corrupt probe reporting a nonsense duration must produce a large
        // number, not a wrapped-around small one that passes the check.
        let huge = required_bytes_for(f64::MAX, "15M", "320k");
        assert!(huge >= MIN_FREE_BYTES);
        let nan = required_bytes_for(f64::NAN, "15M", "320k");
        assert!(nan >= MIN_FREE_BYTES);
    }

    // ---- T2-8: a publish that the registry refuses must not leave the file ----

    #[test]
    fn retry_blocking_returns_the_first_success() {
        let calls = std::cell::Cell::new(0u32);
        let out: Result<&str, String> = retry_blocking(
            3,
            std::time::Duration::from_millis(0),
            "test",
            |attempt| {
                calls.set(calls.get() + 1);
                if attempt < 2 {
                    Err("locked".to_string())
                } else {
                    Ok("ok")
                }
            },
        );
        assert_eq!(out.unwrap(), "ok");
        assert_eq!(calls.get(), 2, "must stop as soon as it succeeds");
    }

    #[test]
    fn retry_blocking_surfaces_the_last_error_after_exhausting_attempts() {
        let calls = std::cell::Cell::new(0u32);
        let out: Result<(), String> = retry_blocking(
            3,
            std::time::Duration::from_millis(0),
            "test",
            |attempt| {
                calls.set(calls.get() + 1);
                Err(format!("failure {}", attempt))
            },
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(
            out.unwrap_err(),
            "failure 3",
            "the caller needs the most recent error, not the first"
        );
    }

    #[test]
    fn retry_blocking_always_runs_at_least_once() {
        let calls = std::cell::Cell::new(0u32);
        let _: Result<(), String> = retry_blocking(0, std::time::Duration::from_millis(0), "t", |_| {
            calls.set(calls.get() + 1);
            Err("e".into())
        });
        assert_eq!(calls.get(), 1, "zero attempts must not mean zero work");
    }

    fn quarantine_fixture(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pt-quar-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("videos")).unwrap();
        root
    }

    #[test]
    fn an_orphaned_mezzanine_is_moved_out_of_the_library() {
        let root = quarantine_fixture("move");
        let published = root.join("videos").join("programme.mp4");
        std::fs::write(&published, b"mezzanine").unwrap();

        let dest = quarantine_published(&root, &published).expect("quarantine");

        assert!(!published.exists(), "it must not stay where PlayOut looks");
        assert!(dest.exists());
        assert_eq!(dest.parent().unwrap(), root.join(QUARANTINE_DIR));
        assert_eq!(
            dest.file_name().unwrap(),
            "programme.mp4",
            "the name is preserved so an operator can tell what it was"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"mezzanine");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_repeated_failure_does_not_overwrite_the_first_ones_evidence() {
        let root = quarantine_fixture("collide");
        let videos = root.join("videos");

        std::fs::write(videos.join("clip.mp4"), b"first").unwrap();
        let a = quarantine_published(&root, &videos.join("clip.mp4")).unwrap();

        std::fs::write(videos.join("clip.mp4"), b"second").unwrap();
        let b = quarantine_published(&root, &videos.join("clip.mp4")).unwrap();

        assert_ne!(a, b, "the second must not land on the first");
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quarantining_creates_the_directory_on_first_use() {
        let root = quarantine_fixture("mkdir");
        assert!(!root.join(QUARANTINE_DIR).exists());

        let published = root.join("videos").join("x.mp4");
        std::fs::write(&published, b"x").unwrap();
        quarantine_published(&root, &published).unwrap();

        assert!(root.join(QUARANTINE_DIR).is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dedup_confirms_only_on_two_agreeing_full_hashes() {
        // The one case that is a duplicate.
        assert!(is_confirmed_duplicate(Some("abc"), Some("abc"), "u"));

        // Same sampled fingerprint, different bytes: two different programmes
        // cut from one master. This is F-15, and it must ingest both.
        assert!(!is_confirmed_duplicate(Some("abc"), Some("def"), "u"));

        // A row ingested before source_sha256 existed. Unconfirmable, so it
        // re-transcodes rather than guessing from the sampled hash alone.
        assert!(!is_confirmed_duplicate(None, Some("abc"), "u"));

        // Our own hash failed -- unreadable source. Also unconfirmable.
        assert!(!is_confirmed_duplicate(Some("abc"), None, "u"));
        assert!(!is_confirmed_duplicate(None, None, "u"));
    }

    #[test]
    fn a_skipped_phase_is_terminal_and_reads_as_completed_on_the_wire() {
        // The whole point of adding the phase: a duplicate is not a failure,
        // but v1 clients only see `state`, so it has to land on Completed.
        assert!(jobs::JobPhase::Skipped.is_terminal());
        assert_eq!(
            jobs::JobPhase::Skipped.as_v1_state(),
            jobs::JobState::Completed
        );
        assert_eq!(jobs::JobPhase::Skipped.as_str(), "skipped");
        // Re-triable: the operator may purge the asset that caused the skip.
        assert!(jobs::JobPhase::Skipped.can_transition_to(jobs::JobPhase::Queued));
        assert!(jobs::JobPhase::Queued.can_transition_to(jobs::JobPhase::Skipped));
    }

    #[test]
    fn test_mock_runner_retryable_until_exhausted() {
        use std::path::PathBuf;

        let runner = MockTranscodeRunner {
            responses: std::sync::Mutex::new(vec![
                encoder::EncodeResult {
                    output_path: PathBuf::from("staged1.mp4"),
                    success: false,
                    error: Some("file locked by another process".into()),
                    stderr_tail: Vec::new(),
                    exit_pid: None,
                },
                encoder::EncodeResult {
                    output_path: PathBuf::from("staged1.mp4"),
                    success: false,
                    error: Some("file locked by another process".into()),
                    stderr_tail: Vec::new(),
                    exit_pid: None,
                },
            ]),
            attempts_seen: std::sync::atomic::AtomicUsize::new(0),
        };

        let err_class1 = classify_error("file locked by another process", false);
        assert_eq!(err_class1, RetryClass::Retryable);

        let max_attempts = 2;
        let mut attempt = 1;
        let mut attempts_executed = 0;

        let dummy_probe = probe::ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "input.mp4".into(),
        };

        let dummy_policy = config::AudioPolicy::default();

        while attempt <= max_attempts {
            attempts_executed += 1;
            let res = runner.run_transcode(
                &bootstrap::ToolPaths {
                    ffmpeg: PathBuf::new(),
                    ffprobe: PathBuf::new(),
                },
                &config::AppConfig::default(),
                Path::new("input.mp4"),
                &dummy_probe,
                profiles::ProfileId::ProfileA,
                Path::new("staged.mp4"),
                "uuid",
                std::sync::mpsc::channel().0,
                "job-1",
                None,
                &dummy_policy,
                None,
            );
            let cls = classify_error(res.error.as_deref().unwrap_or(""), false);
            if cls == RetryClass::Retryable && attempt < max_attempts {
                attempt += 1;
            } else {
                break;
            }
        }

        assert_eq!(attempts_executed, 2);
        assert_eq!(
            runner
                .attempts_seen
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    #[test]
    fn test_sidecar_loudness_field_omitted_in_legacy_mode() {
        let dummy_probe = probe::ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "input.mp4".into(),
        };

        let sidecar = identity::SidecarPayload::new(
            "test-uuid",
            "C:/target/videos/clip.mp4",
            &dummy_probe,
            &dummy_probe,
            "profile_a",
            "h264",
            "aac",
            10000,
            true,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &[],
            None,
        );

        let json = serde_json::to_string(&sidecar).unwrap();
        assert!(
            !json.contains("loudness"),
            "Loudness field must be omitted when None in Legacy mode"
        );
    }

    #[test]
    fn test_sidecar_loudness_field_additive_and_optional() {
        let dummy_probe = probe::ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "input.mp4".into(),
        };

        let loudness = identity::LoudnessInfo {
            integrated_lufs: -24.5,
            true_peak_dbtp: -1.5,
            lra: 6.5,
            threshold: -34.5,
            target_lufs: -23.0,
            target_true_peak_dbtp: -1.0,
            normalization_mode: "ebu_r128".to_string(),
            linear_applied: true,
        };

        let sidecar = identity::SidecarPayload::new(
            "test-uuid",
            "C:/target/videos/clip.mp4",
            &dummy_probe,
            &dummy_probe,
            "profile_a",
            "h264",
            "aac",
            10000,
            true,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &[],
            Some(loudness),
        );

        let json = serde_json::to_string(&sidecar).unwrap();
        assert!(
            json.contains("\"loudness\":{"),
            "Sidecar must include loudness object when present"
        );
        assert!(json.contains("\"integrated_lufs\":-24.5"));
        assert!(json.contains("\"normalization_mode\":\"ebu_r128\""));
        assert!(json.contains("\"linear_applied\":true"));
    }

    #[test]
    fn test_legacy_mode_skips_measurement() {
        let _measurer = MockLoudnessMeasurer {
            measurements: std::sync::Mutex::new(Vec::new()),
            calls_seen: std::sync::atomic::AtomicUsize::new(0),
        };
        let policy = config::AudioPolicy {
            mode: config::AudioMode::LegacyV1Encode,
            ..Default::default()
        };
        let real_measurer = probe::RealLoudnessMeasurer;
        let res = real_measurer
            .measure_loudness(
                &bootstrap::ToolPaths {
                    ffmpeg: PathBuf::new(),
                    ffprobe: PathBuf::new(),
                },
                Path::new("in.mp4"),
                2,
                10.0,
                &policy,
            )
            .unwrap();
        assert!(
            res.is_none(),
            "LegacyV1Encode must skip measurement completely"
        );
    }

    #[test]
    fn test_video_only_input_skips_measurement() {
        let policy = config::AudioPolicy {
            mode: config::AudioMode::EbuR128,
            ..Default::default()
        };
        let real_measurer = probe::RealLoudnessMeasurer;
        let res = real_measurer
            .measure_loudness(
                &bootstrap::ToolPaths {
                    ffmpeg: PathBuf::new(),
                    ffprobe: PathBuf::new(),
                },
                Path::new("in.mp4"),
                0, // 0 audio channels
                10.0,
                &policy,
            )
            .unwrap();
        assert!(
            res.is_none(),
            "Video-only input (0 channels) must skip measurement completely"
        );
    }

    #[test]
    fn test_classified_permanent_on_audio_measurement_failure() {
        let cls = classify_error("Audio measurement failed: invalid json", false);
        assert_eq!(
            cls,
            RetryClass::Permanent,
            "Measurement failure must be classified as Permanent"
        );

        let cls_layout = classify_error("unsupported_audio_channel_layout", false);
        assert_eq!(
            cls_layout,
            RetryClass::Permanent,
            "Unsupported channel layout must be classified as Permanent"
        );
    }

    #[test]
    fn test_compute_file_sha256() {
        let temp_dir = std::env::temp_dir().join(format!("pt_sha256_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("test.txt");
        std::fs::write(&test_file, b"hello playout transcode").unwrap();

        let hash = compute_file_sha256(&test_file).unwrap();
        // SHA-256 of "hello playout transcode"
        assert_eq!(
            hash,
            "4860d772576a6cdb7a774627c607d95e1cd1ff62cd69ef045093eaaf67ae76cb"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cleanup_orphan_staging_files() {
        let temp_dir = std::env::temp_dir().join(format!("pt_orphan_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let orphan1 = temp_dir.join(".tmp_12345_video.mp4");
        let orphan2 = temp_dir.join("clip.tmp_json");
        let normal_file = temp_dir.join("clip.mp4");

        std::fs::write(&orphan1, b"temp content 1").unwrap();
        std::fs::write(&orphan2, b"temp content 2").unwrap();
        std::fs::write(&normal_file, b"normal video content").unwrap();

        // Staging cleanup with max_age_secs = 0 (delete everything matching temp prefix/extension immediately)
        let removed = cleanup_orphan_staging_files(&temp_dir, 0);
        assert_eq!(removed, 2);
        assert!(!orphan1.exists());
        assert!(!orphan2.exists());
        assert!(normal_file.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_build_unique_output_path_collision() {
        let temp_dir = std::env::temp_dir().join(format!("pt_collision_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let uuid1 = "fixed-uuid-1234";
        let initial_path = temp_dir.join("clip_fixed-uuid-1234.mp4");
        std::fs::write(&initial_path, b"already exists").unwrap();

        let unique = build_unique_output_path(&temp_dir, "clip", uuid1);
        assert_ne!(unique, initial_path);
        assert!(!unique.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sidecar_with_validation_and_sha256() {
        let dummy_probe = probe::ProbeData {
            input_path: "C:/src/clip.mp4".into(),
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            fps_num: 25,
            fps_den: 1,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
        };

        let val_report = identity::ValidationReport {
            mezzanine_ok: true,
            duration_ms: 10000,
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            audio_sample_rate: 48000,
            audio_channels: 2,
            closed_gop: true,
            faststart: true,
            warnings: vec![],
            findings: None,
            qc_report: None,
            sha256: Some("abcdef1234567890".into()),
            file_size_bytes: Some(1048576),
        };

        let sidecar = identity::SidecarPayload::new(
            "test-uuid",
            "C:/target/videos/clip.mp4",
            &dummy_probe,
            &dummy_probe,
            "profile_a",
            "h264",
            "aac",
            10000,
            true,
            25.0,
            25,
            1,
            250,
            50,
            0,
            &[],
            None,
        )
        .with_validation(val_report, Some("abcdef1234567890".into()), Some(1048576));

        let json = serde_json::to_string(&sidecar).unwrap();
        assert!(json.contains("\"validation_report\":{"));
        assert!(json.contains("\"sha256\":\"abcdef1234567890\""));
        assert!(json.contains("\"file_size_bytes\":1048576"));
    }

    #[test]
    fn test_qc_evaluation_compliant_pass() {
        let dummy_probe = probe::ProbeData {
            input_path: "C:/src/clip.mp4".into(),
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            fps_num: 25,
            fps_den: 1,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
        };
        let policy = config::AudioPolicy::default();

        let qc = run_qc_evaluation(
            &dummy_probe,
            &dummy_probe,
            true,
            true,
            None,
            &policy,
            &config::ValidationPolicy::default(),
        );
        assert!(qc.passed);
        assert_eq!(qc.blocking_errors, 0);
    }

    #[test]
    fn test_qc_evaluation_blocking_failures() {
        let dummy_source = probe::ProbeData {
            input_path: "C:/src/clip.mp4".into(),
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            fps_num: 25,
            fps_den: 1,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 44100, // Non-48k audio
            audio_channels: 2,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
        };
        let mut dummy_output = dummy_source.clone();
        dummy_output.duration_secs = 0.0; // Zero duration error

        let policy = config::AudioPolicy::default();

        // 1. Zero duration + 44.1k audio + GOP violation + missing faststart -> 4 blocking errors
        let qc = run_qc_evaluation(
            &dummy_output,
            &dummy_source,
            false,
            false,
            None,
            &policy,
            &config::ValidationPolicy::default(),
        );
        assert!(!qc.passed);
        assert!(qc.blocking_errors >= 4);
        assert!(qc.findings.iter().any(|f| f.code == "zero_duration"));
        assert!(qc
            .findings
            .iter()
            .any(|f| f.code == "audio_sample_rate_not_48k"));
        assert!(qc.findings.iter().any(|f| f.code == "closed_gop_violation"));
        assert!(qc.findings.iter().any(|f| f.code == "missing_faststart"));
    }

    #[test]
    // `check_disk_space` is a deliberate no-op off Windows -- it has no
    // GetDiskFreeSpaceEx equivalent wired up, and the product is a Windows
    // service -- so the "insufficient space" half of this can only be asserted
    // there. Running it on Linux asserted that a no-op returns an error.
    #[cfg(windows)]
    fn test_check_disk_space_current_dir() {
        let cwd = std::env::current_dir().unwrap();
        // Request 1 byte (should succeed on any working volume)
        let res = check_disk_space(&cwd, 1);
        assert!(res.is_ok());

        // Request 1000 TB (should fail due to insufficient space)
        let huge_space = 1000 * 1024 * 1024 * 1024 * 1024;
        let res_huge = check_disk_space(&cwd, huge_space);
        assert!(res_huge.is_err());
    }

    #[test]
    fn test_source_cleanup_disabled() {
        let watch = std::env::temp_dir().join("test_cleanup_dis_watch");
        let target = std::env::temp_dir().join("test_cleanup_dis_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let source_file = watch.join("clip.mp4");
        let _ = std::fs::write(&source_file, b"content");
        let final_out = target.join("out.mp4");

        let res = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(7),
            None,
            None,
            None,
            false,
        );
        assert!(!res.enabled);
        assert!(!res.attempted);
        assert!(!res.deleted);
        assert!(res.skipped);
        assert_eq!(res.reason.as_deref(), Some("disabled"));
        assert!(source_file.exists(), "Source file must not be deleted when cleanup is disabled");

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_outside_watch_rejected() {
        let watch = std::env::temp_dir().join("test_cleanup_out_watch");
        let other = std::env::temp_dir().join("test_cleanup_out_other");
        let target = std::env::temp_dir().join("test_cleanup_out_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&other);
        let _ = std::fs::create_dir_all(&target);
        let source_file = other.join("outside.mp4");
        let _ = std::fs::write(&source_file, b"outside content");
        let final_out = target.join("out.mp4");

        let res = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(15),
            None,
            None,
            None,
            true,
        );
        assert!(res.enabled);
        assert!(res.attempted);
        assert!(!res.deleted);
        assert_eq!(res.reason.as_deref(), Some("outside_source_root"));
        assert!(source_file.exists(), "Source file outside watch root must be retained");

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_traversal_and_directory_rejected() {
        let watch = std::env::temp_dir().join("test_cleanup_trav_watch");
        let target = std::env::temp_dir().join("test_cleanup_trav_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let final_out = target.join("out.mp4");

        // 1. Directory rejected
        let res_dir = validate_and_cleanup_source(
            &watch,
            &watch,
            &target,
            &final_out,
            None,
            None,
            None,
            None,
            true,
        );
        assert!(!res_dir.deleted);
        assert_eq!(res_dir.reason.as_deref(), Some("source_is_directory"));

        // 2. Traversal path rejected
        let trav_path = watch.join("sub").join("..").join("clip.mp4");
        let res_trav = validate_and_cleanup_source(
            &trav_path,
            &watch,
            &target,
            &final_out,
            None,
            None,
            None,
            None,
            true,
        );
        assert!(!res_trav.deleted);
        assert_eq!(res_trav.reason.as_deref(), Some("unsafe_path"));

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_target_and_final_collision_rejected() {
        let watch = std::env::temp_dir().join("test_cleanup_col_watch");
        let target = std::env::temp_dir().join("test_cleanup_col_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let target_file = target.join("target_mezzanine.mp4");
        let _ = std::fs::write(&target_file, b"mezzanine");

        // Source path inside target folder
        let res = validate_and_cleanup_source(
            &target_file,
            &target, // even if watch were mistakenly target
            &target,
            &target_file,
            Some(9),
            None,
            None,
            None,
            true,
        );
        assert!(!res.deleted);
        assert_eq!(res.reason.as_deref(), Some("source_matches_target"));
        assert!(target_file.exists());

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_content_changed_during_processing_rejected() {
        let watch = std::env::temp_dir().join("test_cleanup_chg_watch");
        let target = std::env::temp_dir().join("test_cleanup_chg_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let source_file = watch.join("clip.mp4");
        let _ = std::fs::write(&source_file, b"original content");
        let final_out = target.join("out.mp4");

        // Simulate file size change: initial was 100 bytes, current is 16 bytes
        let res = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(100), // initial size was 100
            None,
            None,
            None,
            true,
        );
        assert!(!res.deleted);
        assert_eq!(res.reason.as_deref(), Some("source_changed"));
        assert!(source_file.exists(), "Source must be retained if size changed");

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_referenced_by_other_job_rejected() {
        let watch = std::env::temp_dir().join("test_cleanup_ref_watch");
        let target = std::env::temp_dir().join("test_cleanup_ref_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let source_file = watch.join("shared_clip.mp4");
        let _ = std::fs::write(&source_file, b"shared media data");
        let final_out = target.join("out.mp4");

        let (tx, _) = tokio::sync::broadcast::channel(16);
        let queue = jobs::JobQueue::new(tx, None);
        // Add current job
        let job1 = jobs::JobRecord::new(source_file.to_str().unwrap(), "A");
        queue.push(job1.clone());
        // Add second pending job for the exact same input file
        let job2 = jobs::JobRecord::new(source_file.to_str().unwrap(), "B");
        queue.push(job2);

        let res = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(17),
            None,
            Some(&queue),
            Some(&job1.id),
            true,
        );
        assert!(!res.deleted);
        assert_eq!(res.reason.as_deref(), Some("source_referenced"));
        assert!(source_file.exists(), "Source must be retained if another job references it");

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_source_cleanup_success_verified_deletion() {
        let watch = std::env::temp_dir().join("test_cleanup_ok_watch");
        let target = std::env::temp_dir().join("test_cleanup_ok_target");
        let _ = std::fs::create_dir_all(&watch);
        let _ = std::fs::create_dir_all(&target);
        let source_file = watch.join("clean_clip.mp4");
        let _ = std::fs::write(&source_file, b"verified content data");
        let final_out = target.join("out.mp4");
        let _ = std::fs::write(&final_out, b"mezzanine video data");

        let meta = std::fs::metadata(&source_file).unwrap();
        let size = meta.len();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let res = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(size),
            Some(mtime),
            None,
            None,
            true,
        );
        assert!(res.enabled);
        assert!(res.attempted);
        assert!(res.deleted);
        assert!(!res.skipped);
        assert_eq!(res.reason.as_deref(), Some("deleted"));
        assert!(!source_file.exists(), "Source file must be safely deleted");
        assert!(final_out.exists(), "Target file must be completely untouched");

        // Second call (idempotent: already removed)
        let res2 = validate_and_cleanup_source(
            &source_file,
            &watch,
            &target,
            &final_out,
            Some(size),
            Some(mtime),
            None,
            None,
            true,
        );
        assert!(res2.enabled);
        assert!(!res2.deleted);
        assert!(res2.skipped);
        assert_eq!(res2.reason.as_deref(), Some("already_removed"));

        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn test_classify_probe_match_duration_tolerance() {
        let source = probe::ProbeData {
            duration_secs: 10.0,
            frame_count: 250,
            width: 1920,
            height: 1080,
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            fps_num: 25,
            fps_den: 1,
            field_order: "progressive".into(),
            display_aspect_ratio: "16:9".into(),
            input_path: "source.ts".into(),
        };

        // 1000ms difference (e.g. PTS lead-in skew) within 1200ms tolerance -> Success
        let output_1000ms_skew = probe::ProbeData {
            duration_secs: 11.0,
            ..source.clone()
        };
        assert!(classify_probe_match(output_1000ms_skew, &source).is_ok());

        // 1500ms difference exceeds 1200ms tolerance -> Error
        let output_1500ms_skew = probe::ProbeData {
            duration_secs: 11.5,
            ..source.clone()
        };
        assert!(classify_probe_match(output_1500ms_skew, &source).is_err());
    }
}

