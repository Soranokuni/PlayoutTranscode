use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc;
use walkdir::WalkDir;

static SUPPORTED_EXTENSIONS: &[&str] = &["mp4", "mov", "mxf", "mkv", "avi", "webm", "ts", "m2ts"];

#[derive(Debug, Clone)]
pub struct WatchCandidate {
    pub path: PathBuf,
    pub size: u64,
    pub modified_epoch_secs: u64,
    pub stable_polls: u32,
}

pub fn is_temp_file_name(path: &Path) -> bool {
    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        file_name.starts_with('.') || file_name.starts_with(".tmp_")
    } else {
        false
    }
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
            let metadata = fs::metadata(&path).ok()?;
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
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

    let mut tick_count: u64 = 0;
    let poll_ticks = poll_secs.max(1);

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

                    let entry = candidates
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
                }
            }
        }

        tick_count += 1;
        if tick_count >= poll_ticks {
            tick_count = 0;

            let current_candidates = collect_candidates(&watch_root);
            let now_secs = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let current_paths: std::collections::HashSet<PathBuf> =
                current_candidates.iter().map(|c| c.path.clone()).collect();

            candidates.retain(|path, _| current_paths.contains(path));
            queued.retain(|path, _| current_paths.contains(path));

            for candidate in &current_candidates {
                let ext = candidate
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if !is_extension_allowed(ext, &include_extensions, &exclude_extensions) {
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

                if candidate.size == entry.size
                    && candidate.modified_epoch_secs == entry.modified_epoch_secs
                {
                    entry.stable_polls += 1;
                } else {
                    entry.stable_polls = 1;
                    entry.size = candidate.size;
                    entry.modified_epoch_secs = candidate.modified_epoch_secs;
                }

                if entry.stable_polls < stable_polls_min {
                    continue;
                }

                let age_secs = now_secs.saturating_sub(candidate.modified_epoch_secs);
                if age_secs < settle_secs {
                    continue;
                }

                let identity = (candidate.size, candidate.modified_epoch_secs);
                if queued.get(&candidate.path).copied() == Some(identity) {
                    continue;
                }

                if !is_file_available_for_reading(&candidate.path) {
                    tracing::debug!(
                        "Watch: file still locked: {}",
                        candidate.path.display()
                    );
                    continue;
                }

                tracing::info!(
                    "Watch: stable file ready: {} ({} bytes, age {}s, stable polls {})",
                    candidate.path.display(),
                    candidate.size,
                    age_secs,
                    entry.stable_polls,
                );

                if tx.send(candidate.path.clone()).await.is_err() {
                    tracing::error!("Watch: channel closed, stopping");
                    return;
                }
                queued.insert(candidate.path.clone(), identity);
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn create_notify_watcher(
    watch_root: &Path,
) -> Result<
    (
        notify::RecommendedWatcher,
        tokio::sync::mpsc::UnboundedReceiver<notify::Event>,
    ),
    String,
> {
    use notify::Watcher;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
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
