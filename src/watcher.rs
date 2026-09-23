use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc;
use walkdir::WalkDir;

static SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "mxf", "mkv", "avi", "webm", "ts", "m2ts", "mpg", "mpeg", "m4v", "vob", "mts",
    "m2t", "wmv", "asf", "flv",
];

static TEMP_EXTENSIONS: &[&str] = &[
    "tmp",
    "temp",
    "part",
    "partial",
    "crdownload",
    "download",
    "filepart",
    "upload",
    "incomplete",
    "json",
];

/// Bound on the filesystem-event channel (T3-7).
///
/// Sized for a bulk copy: a few thousand files landing at once fills it, and
/// the overflow is reconciled by the poll walk rather than buffered forever.
pub const NOTIFY_CHANNEL_CAPACITY: usize = 4096;

/// Minimum interval between full directory walks when `notify` is healthy.
///
/// The walk is O(files in the watch folder) and ran every `poll_secs` -- 10 s
/// by default -- whether or not anything had changed. On a watch folder holding
/// thousands of files that is a continuous disk scan for no benefit, since
/// `notify` already reports changes within milliseconds. It stays a
/// *reconciliation* pass: events can be dropped, and a file dropped from the
/// channel has to be found eventually.
pub const RECONCILE_MIN_SECS: u64 = 60;

/// Events the OS reported that the channel could not hold. Purely diagnostic:
/// correctness comes from the reconciliation walk, not from this counter.
pub static NOTIFY_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long to wait before the next full walk.
///
/// With a healthy `notify` the walk only has to reconcile, so it backs off to
/// `RECONCILE_MIN_SECS`. Without one, polling is the only way a file is ever
/// noticed and `poll_secs` is honoured exactly.
pub fn effective_poll_interval_secs(poll_secs: u64, notify_healthy: bool) -> u64 {
    let configured = poll_secs.max(1);
    if notify_healthy {
        configured.max(RECONCILE_MIN_SECS)
    } else {
        configured
    }
}

#[derive(Debug, Clone)]
pub struct WatchCandidate {
    pub path: PathBuf,
    pub size: u64,
    pub modified_epoch_secs: u64,
    pub stable_polls: u32,
}

pub fn is_temp_file_name(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return true,
    };

    // Hidden files, temporary staging files, or editor backup files
    if file_name.starts_with('.') || file_name.starts_with(".tmp_") || file_name.starts_with('~') {
        return true;
    }

    // Sidecar metadata files (e.g. video.mp4.uuid.json or *.json)
    if file_name.ends_with(".uuid.json") || file_name.ends_with(".json") {
        return true;
    }

    // In-flight or partial download extensions
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_ascii_lowercase();
        if TEMP_EXTENSIONS.contains(&lower.as_str()) {
            return true;
        }
    }

    false
}

pub fn collect_candidates(root: &Path) -> Vec<WatchCandidate> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.path().to_path_buf();
            if is_temp_file_name(&path) {
                return None;
            }
            // Extension first: rejecting a .txt must not cost a syscall.
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            // `entry.metadata()` is served from what FindNextFileW already
            // returned for this directory; `fs::metadata` would issue a fresh
            // open per file, which on an SMB watch folder is a round trip.
            // walkdir does not follow links here, and `file_type().is_file()`
            // above already excluded symlinks, so the two agree.
            let metadata = entry.metadata().ok()?;
            if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Some(WatchCandidate {
                    path,
                    size: metadata.len(),
                    modified_epoch_secs: modified,
                    stable_polls: 0,
                })
            } else {
                None
            }
        })
        .collect()
}

pub fn is_extension_allowed(ext: &str, include: &[String], exclude: &[String]) -> bool {
    let lower = ext.to_ascii_lowercase();
    if !SUPPORTED_EXTENSIONS.contains(&lower.as_str()) {
        return false;
    }
    if !include.is_empty() && !include.iter().any(|i| i == &lower) {
        return false;
    }
    if exclude.iter().any(|e| e == &lower) {
        return false;
    }
    true
}

#[cfg(target_os = "windows")]
fn is_file_available_for_reading(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    fs::File::options()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_ok()
}

#[cfg(not(target_os = "windows"))]
fn is_file_available_for_reading(path: &Path) -> bool {
    fs::File::options()
        .read(true)
        .write(false)
        .open(path)
        .is_ok()
}

pub async fn watch_loop(
    watch_root: PathBuf,
    settle_secs: u64,
    poll_secs: u64,
    stable_polls_min: u32,
    include_extensions: Vec<String>,
    exclude_extensions: Vec<String>,
    tx: mpsc::Sender<PathBuf>,
) {
    let mut candidates: HashMap<PathBuf, WatchCandidate> = HashMap::new();
    let mut queued: HashMap<PathBuf, (u64, u64)> = HashMap::new();
    let stable_polls_min = stable_polls_min.max(1);

    let (_watcher, mut notify_rx) = match create_notify_watcher(&watch_root) {
        Ok((watcher, rx)) => (Some(watcher), Some(rx)),
        Err(e) => {
            tracing::warn!("Filesystem watcher unavailable ({}), using polling only", e);
            (None, None)
        }
    };

    tracing::info!(
        "Watcher started: root={} settle={}s poll={}s stable_polls_min={}",
        watch_root.display(),
        settle_secs,
        poll_secs,
        stable_polls_min,
    );

    // Walk on the first pass rather than a full interval in: files already
    // in the folder at start -- retained sources, and the jobs a stop or a
    // crash re-queued -- used to wait 60 s while newly notified files went
    // first (W-6).
    let mut tick_count: u64 = u64::MAX / 2;
    // Backs off to a reconciliation cadence while `notify` is delivering.
    let poll_ticks = effective_poll_interval_secs(poll_secs, notify_rx.is_some());
    tracing::info!(
        "Watcher reconciliation walk every {}s (notify {})",
        poll_ticks,
        if notify_rx.is_some() { "healthy" } else { "unavailable" },
    );
    let mut last_dropped_report: u64 = 0;
    // Paths `notify` has reported and that are not yet queued.
    let mut hot: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let hot_ticks = poll_secs.max(1);
    let rules = ReadinessRules {
        settle_secs,
        stable_polls_min,
        include_extensions: &include_extensions,
        exclude_extensions: &exclude_extensions,
    };

    loop {
        if let Some(ref mut rx) = notify_rx {
            while let Ok(event) = rx.try_recv() {
                for path in &event.paths {
                    if !path.is_file() || is_temp_file_name(path) {
                        continue;
                    }
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !is_extension_allowed(ext, &include_extensions, &exclude_extensions) {
                        continue;
                    }
                    let Ok(meta) = fs::metadata(path) else {
                        continue;
                    };
                    let size = meta.len();
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    let entry =
                        candidates
                            .entry(path.to_path_buf())
                            .or_insert_with(|| WatchCandidate {
                                path: path.to_path_buf(),
                                size: 0,
                                modified_epoch_secs: 0,
                                stable_polls: 0,
                            });

                    if size > entry.size {
                        entry.stable_polls = 0;
                    }
                    entry.size = size;
                    entry.modified_epoch_secs = modified;
                    hot.insert(path.to_path_buf());
                }
            }
        }

        tick_count += 1;
        if tick_count >= poll_ticks {
            tick_count = 0;

            // `collect_candidates` is a recursive `WalkDir` with a `metadata`
            // call per entry -- blocking syscalls, on the runtime that also
            // serves HTTP. On a large watch folder that stalled request
            // handling for the length of the walk (T3-7).
            let root_for_walk = watch_root.clone();
            let current_candidates =
                match tokio::task::spawn_blocking(move || collect_candidates(&root_for_walk)).await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Watch: directory walk task failed: {}", e);
                        continue;
                    }
                };

            let dropped = NOTIFY_DROPPED.load(std::sync::atomic::Ordering::Relaxed);
            if dropped > last_dropped_report {
                tracing::warn!(
                    "Watcher dropped {} filesystem event(s) since the last walk; \
                     the reconciliation pass has picked them up",
                    dropped - last_dropped_report
                );
                last_dropped_report = dropped;
            }

            let now_secs = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let current_paths: std::collections::HashSet<PathBuf> =
                current_candidates.iter().map(|c| c.path.clone()).collect();

            candidates.retain(|path, _| current_paths.contains(path));
            queued.retain(|path, _| current_paths.contains(path));

            // Whatever the walk found that is not queued yet is watched at the
            // fast cadence until it is, instead of waiting for further walks.
            for c in &current_candidates {
                let ext = c.path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if queued.get(&c.path).copied() != Some((c.size, c.modified_epoch_secs))
                    && is_extension_allowed(ext, &include_extensions, &exclude_extensions)
                {
                    hot.insert(c.path.clone());
                }
            }
            for (path, identity) in stable_and_ready(
                &current_candidates,
                &mut candidates,
                &queued,
                &rules,
                now_secs,
            ) {
                hot.remove(&path);
                if tx.send(path.clone()).await.is_err() {
                    tracing::error!("Watch: channel closed, stopping");
                    return;
                }
                queued.insert(path, identity);
            }
        } else if !hot.is_empty() && tick_count.is_multiple_of(hot_ticks) {
            // W-6. Between walks, re-stat only what `notify` reported. Only the
            // walk used to judge stability, and with a healthy `notify` that
            // walk backs off to every 60 s -- so with the default two stable
            // polls a file dropped into the folder sat for one to two minutes
            // before anything happened. A handful of `stat`s every `poll_secs`
            // is nothing next to a full walk.
            let paths: Vec<PathBuf> = hot.iter().cloned().collect();
            let observed = match tokio::task::spawn_blocking(move || stat_candidates(&paths)).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::error!("Watch: stat task failed: {}", e);
                    Vec::new()
                }
            };
            // Gone since the event: nothing to wait for.
            let present: std::collections::HashSet<&PathBuf> =
                observed.iter().map(|c| &c.path).collect();
            hot.retain(|p| present.contains(p));

            let now_secs = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for (path, identity) in
                stable_and_ready(&observed, &mut candidates, &queued, &rules, now_secs)
            {
                hot.remove(&path);
                if tx.send(path.clone()).await.is_err() {
                    tracing::error!("Watch: channel closed, stopping");
                    return;
                }
                queued.insert(path, identity);
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// The thresholds a candidate has to clear before it is handed on.
struct ReadinessRules<'a> {
    settle_secs: u64,
    stable_polls_min: u32,
    include_extensions: &'a [String],
    exclude_extensions: &'a [String],
}

/// Fold one round of observations into `candidates` and return the files that
/// are now stable, settled, readable and not already queued, with the
/// `(size, mtime)` identity to record for each.
///
/// Shared by the full walk and the between-walks re-stat of `notify`'s paths,
/// so both judge a file by exactly the same rules.
fn stable_and_ready(
    observed: &[WatchCandidate],
    candidates: &mut HashMap<PathBuf, WatchCandidate>,
    queued: &HashMap<PathBuf, (u64, u64)>,
    rules: &ReadinessRules<'_>,
    now_secs: u64,
) -> Vec<(PathBuf, (u64, u64))> {
    let mut ready = Vec::new();
    for candidate in observed {
        let ext = candidate
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !is_extension_allowed(ext, rules.include_extensions, rules.exclude_extensions) {
            continue;
        }

        let entry = candidates
            .entry(candidate.path.clone())
            .or_insert_with(|| WatchCandidate {
                path: candidate.path.clone(),
                size: 0,
                modified_epoch_secs: 0,
                stable_polls: 0,
            });

        if candidate.size > entry.size {
            entry.stable_polls = 0;
            entry.size = candidate.size;
            entry.modified_epoch_secs = candidate.modified_epoch_secs;
            continue;
        }

        if candidate.size == entry.size && candidate.modified_epoch_secs == entry.modified_epoch_secs {
            entry.stable_polls += 1;
        } else {
            entry.stable_polls = 1;
            entry.size = candidate.size;
            entry.modified_epoch_secs = candidate.modified_epoch_secs;
        }

        if entry.stable_polls < rules.stable_polls_min {
            continue;
        }

        let age_secs = now_secs.saturating_sub(candidate.modified_epoch_secs);
        if age_secs < rules.settle_secs {
            continue;
        }

        let identity = (candidate.size, candidate.modified_epoch_secs);
        if queued.get(&candidate.path).copied() == Some(identity) {
            continue;
        }

        if !is_file_available_for_reading(&candidate.path) {
            tracing::debug!("Watch: file still locked: {}", candidate.path.display());
            continue;
        }

        tracing::info!(
            "Watch: stable file ready: {} ({} bytes, age {}s, stable polls {})",
            candidate.path.display(),
            candidate.size,
            age_secs,
            entry.stable_polls,
        );
        ready.push((candidate.path.clone(), identity));
    }
    ready
}

/// `stat` each path; the ones that are gone or not regular files are left out.
fn stat_candidates(paths: &[PathBuf]) -> Vec<WatchCandidate> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = fs::metadata(path).ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some(WatchCandidate {
                path: path.clone(),
                size: meta.len(),
                modified_epoch_secs: modified,
                stable_polls: 0,
            })
        })
        .collect()
}

fn create_notify_watcher(
    watch_root: &Path,
) -> Result<
    (
        notify::RecommendedWatcher,
        tokio::sync::mpsc::Receiver<notify::Event>,
    ),
    String,
> {
    use notify::Watcher;

    // Bounded (T3-7). An unbounded channel fed by the OS during a bulk copy of
    // a few thousand files grows without limit while the loop is busy, and the
    // only backstop was the process running out of memory. A full channel now
    // drops the event and the reconciliation walk picks the file up instead --
    // latency, not loss.
    let (tx, rx) = tokio::sync::mpsc::channel(NOTIFY_CHANNEL_CAPACITY);
    let mut watcher = notify::RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                // `try_send` rather than `send`: this callback runs on the
                // notify backend's own thread and must never block it. A
                // dropped event is reconciled by the poll walk.
                if tx.try_send(event).is_err() {
                    NOTIFY_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| format!("Failed to create watcher: {}", e))?;

    watcher
        .watch(watch_root, notify::RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    Ok((watcher, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_is_temp_file_name_hidden_and_prefixes() {
        assert!(is_temp_file_name(Path::new(".hidden.mp4")));
        assert!(is_temp_file_name(Path::new(".DS_Store")));
        assert!(is_temp_file_name(Path::new(".tmp_uuid_clip.mp4")));
        assert!(is_temp_file_name(Path::new("~$clip.mp4")));
    }

    #[test]
    fn test_is_temp_file_name_sidecar_json() {
        assert!(is_temp_file_name(Path::new("clip.mp4.uuid.json")));
        assert!(is_temp_file_name(Path::new("metadata.json")));
    }

    #[test]
    fn test_is_temp_file_name_in_flight_extensions() {
        assert!(is_temp_file_name(Path::new("movie.mp4.crdownload")));
        assert!(is_temp_file_name(Path::new("movie.mp4.part")));
        assert!(is_temp_file_name(Path::new("movie.mp4.partial")));
        assert!(is_temp_file_name(Path::new("movie.mp4.download")));
        assert!(is_temp_file_name(Path::new("movie.mp4.tmp")));
        assert!(is_temp_file_name(Path::new("movie.mp4.filepart")));
        assert!(is_temp_file_name(Path::new("movie.mp4.upload")));
        assert!(is_temp_file_name(Path::new("movie.mp4.incomplete")));
    }

    #[test]
    fn test_is_temp_file_name_valid_media() {
        assert!(!is_temp_file_name(Path::new("news_intro.mp4")));
        assert!(!is_temp_file_name(Path::new("interview.mov")));
        assert!(!is_temp_file_name(Path::new("commercial.mxf")));
        assert!(!is_temp_file_name(Path::new("feature.mkv")));
        assert!(!is_temp_file_name(Path::new("bumper.ts")));
    }

    #[test]
    fn test_is_extension_allowed() {
        // Default allowed
        assert!(is_extension_allowed("mp4", &[], &[]));
        assert!(is_extension_allowed("MOV", &[], &[]));
        assert!(is_extension_allowed("mpg", &[], &[]));
        assert!(is_extension_allowed("mpeg", &[], &[]));
        assert!(is_extension_allowed("m4v", &[], &[]));
        assert!(is_extension_allowed("vob", &[], &[]));
        assert!(is_extension_allowed("mts", &[], &[]));
        assert!(is_extension_allowed("m2t", &[], &[]));
        assert!(is_extension_allowed("wmv", &[], &[]));
        assert!(is_extension_allowed("asf", &[], &[]));
        assert!(is_extension_allowed("flv", &[], &[]));
        assert!(!is_extension_allowed("txt", &[], &[]));
        assert!(!is_extension_allowed("exe", &[], &[]));

        // With include list
        let inc = vec!["mp4".to_string(), "mov".to_string()];
        assert!(is_extension_allowed("mp4", &inc, &[]));
        assert!(!is_extension_allowed("mxf", &inc, &[]));

        // With exclude list
        let exc = vec!["avi".to_string()];
        assert!(!is_extension_allowed("avi", &[], &exc));
        assert!(is_extension_allowed("mp4", &[], &exc));
    }

    #[test]
    fn test_collect_candidates_skips_temp_files() {
        let temp_dir = std::env::temp_dir().join("pt_v2_2c_test_watch");
        let _ = fs::create_dir_all(&temp_dir);

        // Create valid media
        let valid_file = temp_dir.join("valid_video.mp4");
        let mut f = fs::File::create(&valid_file).unwrap();
        writeln!(f, "fake video data").unwrap();
        drop(f);

        // Create temporary files
        let tmp_file = temp_dir.join(".tmp_uuid_valid_video.mp4");
        fs::File::create(&tmp_file).unwrap();

        let part_file = temp_dir.join("downloading.mp4.part");
        fs::File::create(&part_file).unwrap();

        let json_file = temp_dir.join("valid_video.mp4.uuid.json");
        fs::File::create(&json_file).unwrap();

        let candidates = collect_candidates(&temp_dir);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, valid_file);

        // Cleanup
        let _ = fs::remove_file(&valid_file);
        let _ = fs::remove_file(&tmp_file);
        let _ = fs::remove_file(&part_file);
        let _ = fs::remove_file(&json_file);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// W-6. The readiness rules the full walk and the notify re-stat share:
    /// a file is handed on once it has held still for `stable_polls_min`
    /// observations and is older than `settle_secs`, and only once per
    /// `(size, mtime)`.
    #[test]
    fn a_file_is_ready_after_it_holds_still_and_only_once() {
        let dir = std::env::temp_dir().join(format!("pt_w6_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("clip.mxf");
        fs::write(&file, b"media").unwrap();

        let rules = ReadinessRules {
            settle_secs: 5,
            stable_polls_min: 2,
            include_extensions: &[],
            exclude_extensions: &[],
        };
        let mut candidates = HashMap::new();
        let mut queued = HashMap::new();
        let obs = |size: u64| {
            vec![WatchCandidate {
                path: file.clone(),
                size,
                modified_epoch_secs: 1_000,
                stable_polls: 0,
            }]
        };

        // First sight: growing from nothing.
        assert!(stable_and_ready(&obs(5), &mut candidates, &queued, &rules, 2_000).is_empty());
        // Still growing: the count restarts.
        assert!(stable_and_ready(&obs(9), &mut candidates, &queued, &rules, 2_000).is_empty());
        // One still observation is not enough...
        assert!(stable_and_ready(&obs(9), &mut candidates, &queued, &rules, 2_000).is_empty());
        // ...two is.
        let ready = stable_and_ready(&obs(9), &mut candidates, &queued, &rules, 2_000);
        assert_eq!(ready.len(), 1);
        queued.insert(ready[0].0.clone(), ready[0].1);

        // Already queued with this identity: not again.
        assert!(stable_and_ready(&obs(9), &mut candidates, &queued, &rules, 2_000).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_inside_the_settle_window_waits() {
        let rules = ReadinessRules {
            settle_secs: 60,
            stable_polls_min: 1,
            include_extensions: &[],
            exclude_extensions: &[],
        };
        let mut candidates = HashMap::new();
        let queued = HashMap::new();
        let obs = vec![WatchCandidate {
            path: PathBuf::from("D:/w/fresh.mxf"),
            size: 10,
            modified_epoch_secs: 1_000,
            stable_polls: 0,
        }];
        let _ = stable_and_ready(&obs, &mut candidates, &queued, &rules, 1_010);
        assert!(stable_and_ready(&obs, &mut candidates, &queued, &rules, 1_010).is_empty());
    }

    #[test]
    fn stat_leaves_out_what_is_gone() {
        let dir = std::env::temp_dir().join(format!("pt_w6_stat_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let here = dir.join("here.mxf");
        fs::write(&here, b"x").unwrap();
        let got = stat_candidates(&[here.clone(), dir.join("gone.mxf"), dir.clone()]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, here);
        let _ = fs::remove_dir_all(&dir);
    }
}
