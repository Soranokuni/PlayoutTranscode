use crate::bootstrap::{self, ToolPaths};
use crate::config::{self, AppConfig};
use crate::db;
use crate::jobs::JobQueue;
use parking_lot::Mutex;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum ServiceCmd {
    Stop,
}

/// A manual retry, with the job record it should reuse.
#[derive(Debug, Clone)]
pub struct RetryRequest {
    pub path: std::path::PathBuf,
    /// The existing job to adopt. `None` dispatches as a fresh ingest.
    pub job_id: Option<String>,
}

/// Where the processing loop is in its lifecycle (T2-5).
///
/// This replaced a bare `running: bool`. The boolean could not distinguish
/// "stopped" from "stopping", so a stop immediately followed by a start
/// happily spawned a second watcher thread while the first was still tearing
/// down its FFmpeg children — two watchers on one folder, two dispatchers on
/// one semaphore (F-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

impl ServiceState {
    /// The value reported over the API. Stable; clients may key on it.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Stopping => "stopping",
        }
    }
}

/// The lifecycle state together with the run it belongs to.
///
/// `generation` is bumped on every start. Work dispatched under generation N
/// checks it before acquiring a semaphore permit, so a task that was queued
/// before a stop cannot wake up inside the *next* run and process a file the
/// new configuration never offered it.
#[derive(Debug, Clone, Copy)]
pub struct RunState {
    pub state: ServiceState,
    pub generation: u64,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            state: ServiceState::Stopped,
            generation: 0,
        }
    }
}

/// Signals a worker thread's exit to whoever is reaping it.
///
/// A `std::thread::JoinHandle` cannot be joined with a timeout, and a stop must
/// not block the HTTP handler that asked for it. The worker flips this on the
/// way out; the reaper waits on it, bounded, and only then reports `Stopped`.
#[derive(Default)]
struct WorkerExit {
    done: StdMutex<bool>,
    cv: Condvar,
}

impl WorkerExit {
    fn finish(&self) {
        if let Ok(mut done) = self.done.lock() {
            *done = true;
        }
        self.cv.notify_all();
    }

    /// Returns true if the worker finished within `timeout`.
    fn wait(&self, timeout: std::time::Duration) -> bool {
        let Ok(done) = self.done.lock() else {
            return false;
        };
        match self.cv.wait_timeout_while(done, timeout, |d| !*d) {
            Ok((guard, _)) => *guard,
            Err(_) => false,
        }
    }
}

/// How long a stop waits for the processing thread before it says so out loud.
/// It keeps waiting afterwards — reporting `Stopped` while a watcher is still
/// alive is what would let a second one start alongside it.
const WORKER_JOIN_WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// How many lines the in-memory log ring keeps for the UI.
const LOG_RING_CAPACITY: usize = 500;

/// One line of the UI log ring, with the cursor a viewer keys and polls on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub text: String,
}

/// The answer to `GET /api/logs?since=<seq>`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    /// The cursor to send next time.
    pub next: u64,
    /// The cursor had already fallen off the back of the ring, so lines were
    /// missed and the viewer should repaint rather than append.
    pub dropped: bool,
}

#[derive(Clone)]
pub struct ServiceHandle {
    run_state: Arc<Mutex<RunState>>,
    /// The processing thread of the current generation, taken by whoever reaps it.
    worker: Arc<StdMutex<Option<std::thread::JoinHandle<()>>>>,
    /// Flipped by the worker on its way out; awaited by the reaper.
    worker_exit: Arc<Mutex<Arc<WorkerExit>>>,
    pub cmd_tx: Arc<Mutex<Option<mpsc::Sender<ServiceCmd>>>>,
    /// Optional channel for the API to inject manual retries into the processing loop.
    pub retry_tx: Arc<StdMutex<Option<mpsc::Sender<RetryRequest>>>>,
    /// Hash of the config the processing loop was started with (T2-12).
    /// `None` when it has never been started.
    started_config_hash: Arc<Mutex<Option<u64>>>,
    pub download_status: Arc<Mutex<Option<String>>>,
    pub log_lines: Arc<Mutex<std::collections::VecDeque<LogLine>>>,
    pub active_pids: ActivePids,
}

impl ServiceHandle {
    pub fn new() -> Self {
        Self {
            run_state: Arc::new(Mutex::new(RunState::default())),
            worker: Arc::new(StdMutex::new(None)),
            worker_exit: Arc::new(Mutex::new(Arc::new(WorkerExit::default()))),
            cmd_tx: Arc::new(Mutex::new(None)),
            retry_tx: Arc::new(StdMutex::new(None)),
            started_config_hash: Arc::new(Mutex::new(None)),
            download_status: Arc::new(Mutex::new(None)),
            log_lines: Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
                LOG_RING_CAPACITY,
            ))),
            active_pids: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Submit a manual retry for an input file. Fails if the service is not running.
    ///
    /// `job_id` names the existing record to reuse. Passing it is what stops a
    /// retry from leaving the old job behind as a permanent ghost: the
    /// dispatcher adopts that record instead of creating a second one for the
    /// same file (F-13). `None` means "treat this as a fresh ingest".
    pub fn submit_retry(
        &self,
        path: std::path::PathBuf,
        job_id: Option<String>,
    ) -> Result<(), String> {
        if !self.is_running() {
            return Err("Service is not running".into());
        }
        let guard = self
            .retry_tx
            .lock()
            .map_err(|e| format!("retry channel lock: {}", e))?;
        match guard.as_ref() {
            Some(tx) => tx
                .try_send(RetryRequest { path, job_id })
                .map_err(|e| format!("retry queue full or closed: {}", e)),
            None => Err("retry channel not established".into()),
        }
    }

    pub fn add_log(&self, level: &str, msg: &str) {
        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
        let mut logs = self.log_lines.lock();
        let seq = logs.back().map(|l| l.seq + 1).unwrap_or(0);
        logs.push_back(LogLine {
            seq,
            text: format!("{} [{}] {}", ts, level.to_uppercase(), msg),
        });
        // `Vec::remove(0)` shifted 500 elements per line; a deque drops the
        // oldest in O(1).
        while logs.len() > LOG_RING_CAPACITY {
            logs.pop_front();
        }
    }

    /// The whole ring, oldest first. Unchanged shape: `GET /api/logs` with no
    /// cursor still answers a bare array of strings.
    pub fn get_logs(&self) -> Vec<String> {
        self.log_lines
            .lock()
            .iter()
            .map(|l| l.text.clone())
            .collect()
    }

    /// Only the lines added after `since`, with the cursor to pass next time.
    ///
    /// The ring shifts, so a viewer that keyed rows by index repainted all 500
    /// nodes on every poll and could not tell "nothing new" from "everything
    /// moved by one". `dropped` says the cursor fell off the back of the ring
    /// and the caller should repaint from scratch.
    pub fn logs_since(&self, since: u64) -> LogPage {
        let logs = self.log_lines.lock();
        let oldest = logs.front().map(|l| l.seq);
        let dropped = match oldest {
            Some(oldest) => since < oldest,
            None => false,
        };
        let lines: Vec<LogLine> = logs
            .iter()
            .filter(|l| l.seq >= since)
            .cloned()
            .collect();
        let next = logs.back().map(|l| l.seq + 1).unwrap_or(since);
        LogPage {
            lines,
            next,
            dropped,
        }
    }

    /// True only in [`ServiceState::Running`].
    ///
    /// Deliberately false while `Starting` and `Stopping`: every caller of this
    /// is asking "may I hand work to the processing loop?", and in both of
    /// those states the answer is no.
    pub fn is_running(&self) -> bool {
        self.run_state.lock().state == ServiceState::Running
    }

    /// The full lifecycle state, for `/api/service/status` and diagnostics.
    pub fn state(&self) -> ServiceState {
        self.run_state.lock().state
    }

    /// Does the running processing loop predate the current configuration?
    ///
    /// `PUT /api/config` writes the file and updates the in-memory config, but
    /// the watcher, the concurrency semaphore and the CPU budget were all
    /// captured by value when the loop started (F-23). An operator who changed
    /// `max_concurrency` or the watch folder and saw the UI accept it had no
    /// way to learn that nothing had actually changed until the next restart.
    ///
    /// False when the service is not running: there is nothing to restart.
    pub fn restart_required(&self, current: &AppConfig) -> bool {
        if !self.is_running() {
            return false;
        }
        match *self.started_config_hash.lock() {
            Some(started) => started != runtime_config_hash(current),
            // Running, but started before this field existed or by a path that
            // did not record it. Claiming a restart is needed would nag; say no.
            None => false,
        }
    }

    /// The current run's generation. Bumped once per successful start.
    pub fn generation(&self) -> u64 {
        self.run_state.lock().generation
    }

    /// True when the service is still serving the run `generation` belongs to.
    /// Used by [`dispatch_one`] to drop work queued under a previous run.
    fn is_current_run(&self, generation: u64) -> bool {
        let rs = self.run_state.lock();
        rs.generation == generation && rs.state == ServiceState::Running
    }

    /// Claim the right to start. On success the state is `Starting` and the
    /// returned generation identifies this run.
    fn begin_start(&self) -> Result<u64, String> {
        let mut rs = self.run_state.lock();
        match rs.state {
            ServiceState::Running => Err("Service already running".into()),
            ServiceState::Starting => Err("Service is starting".into()),
            ServiceState::Stopping => Err("Service is stopping".into()),
            ServiceState::Stopped => {
                rs.generation += 1;
                rs.state = ServiceState::Starting;
                Ok(rs.generation)
            }
        }
    }

    /// Roll a failed `Starting` back, without disturbing a later run.
    fn abandon_start(&self, generation: u64) {
        let mut rs = self.run_state.lock();
        if rs.generation == generation && rs.state == ServiceState::Starting {
            rs.state = ServiceState::Stopped;
        }
    }

    /// Publish the worker thread and move `Starting` to `Running`.
    ///
    /// Both under the same `run_state` lock, because a stop arriving between
    /// the two would otherwise find no worker to reap and report `Stopped`
    /// while the thread was still alive — the exact race this step exists to
    /// remove. When the stop wins, the handle comes back here instead and the
    /// caller reaps it.
    fn install_worker(
        &self,
        generation: u64,
        worker: std::thread::JoinHandle<()>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let mut rs = self.run_state.lock();
        if rs.generation != generation || rs.state != ServiceState::Starting {
            return Some(worker);
        }
        if let Ok(mut slot) = self.worker.lock() {
            *slot = Some(worker);
        }
        rs.state = ServiceState::Running;
        None
    }

    /// Move to `Stopping` and take the worker to reap, in one step.
    /// `None` means there was nothing to stop.
    #[allow(clippy::type_complexity)]
    fn begin_stop(&self) -> Option<(u64, Option<std::thread::JoinHandle<()>>, Arc<WorkerExit>)> {
        let mut rs = self.run_state.lock();
        match rs.state {
            ServiceState::Stopped | ServiceState::Stopping => None,
            ServiceState::Starting | ServiceState::Running => {
                rs.state = ServiceState::Stopping;
                let worker = self.worker.lock().ok().and_then(|mut slot| slot.take());
                let exit = self.worker_exit.lock().clone();
                Some((rs.generation, worker, exit))
            }
        }
    }

    fn mark_stopped(&self, generation: u64) {
        let mut rs = self.run_state.lock();
        if rs.generation == generation {
            rs.state = ServiceState::Stopped;
        }
    }

    pub fn active_pids_count(&self) -> usize {
        self.active_pids.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// Kill every running FFmpeg. Used on service stop, not on job cancel —
    /// see [`kill_ffmpeg_for_job`] for the per-job path.
    pub fn kill_active_ffmpeg(&self) {
        let pids: Vec<u32> = {
            match self.active_pids.lock() {
                Ok(mut map) => {
                    let snapshot = map.values().copied().collect();
                    map.clear();
                    snapshot
                }
                Err(e) => {
                    tracing::error!("active_pids lock poisoned: {}", e);
                    return;
                }
            }
        };

        for pid in pids {
            kill_process_tree(pid);
        }
    }
}

/// FFmpeg process ids keyed by the job that owns them.
///
/// This used to be a flat `Vec<u32>` shared by the whole service, so a cancel
/// on one job killed every concurrent encode (F-12).
pub type ActivePids = Arc<StdMutex<HashMap<String, u32>>>;

/// Kill the FFmpeg belonging to `job_id` and nothing else.
///
/// `killer` is injected so the selection logic is unit-testable without
/// spawning processes.
pub fn kill_ffmpeg_for_job(pids: &ActivePids, job_id: &str, mut killer: impl FnMut(u32)) -> bool {
    let pid = match pids.lock() {
        Ok(mut map) => map.remove(job_id),
        Err(e) => {
            tracing::error!("active_pids lock poisoned: {}", e);
            return false;
        }
    };
    match pid {
        Some(pid) => {
            killer(pid);
            true
        }
        None => false,
    }
}

pub fn kill_process_tree(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let pid_str = pid.to_string();
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid_str, "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// Hash of the configuration fields the processing loop captures at start.
///
/// Deliberately not the whole `AppConfig`: most of it is read per request or
/// per job and takes effect immediately, and hashing those would report a
/// restart as needed for a change that has already applied. These are the ones
/// the loop copies by value and then never re-reads.
pub fn runtime_config_hash(config: &AppConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = fnv::FnvHasher::default();

    config.paths.watch_folder.hash(&mut h);
    config.paths.target_folder.hash(&mut h);
    config.ingestion.max_concurrency.hash(&mut h);
    config.ingestion.settle_secs.hash(&mut h);
    config.ingestion.poll_secs.hash(&mut h);
    config.ingestion.stable_polls_min.hash(&mut h);
    config.ingestion.include_extensions.hash(&mut h);
    config.ingestion.exclude_extensions.hash(&mut h);
    config.ingestion.auto_retry_on_start.hash(&mut h);
    config.encoding.cpu_cores.hash(&mut h);
    config.encoding.ffmpeg_threads.hash(&mut h);

    h.finish()
}

pub fn start_processing_loop(
    handle: &ServiceHandle,
    config: &AppConfig,
    job_queue: &JobQueue,
    tools: &ToolPaths,
    pool: Arc<SqlitePool>,
    registry_id: &str,
) -> Result<(), String> {
    // Claims the state machine before anything else, so two concurrent
    // `POST /api/service/start` calls cannot both get past this point.
    let generation = handle.begin_start()?;
    *handle.started_config_hash.lock() = Some(runtime_config_hash(config));

    // Refused outright: `create_dir_all("")` succeeds, and the claim would
    // then stamp the working directory.
    if config.paths.target_folder.trim().is_empty() {
        handle.abandon_start(generation);
        return Err("Target folder is not configured".to_string());
    }

    // Creating the target folder used to be a side effect of
    // `AppConfig::validate()`, which meant an unauthenticated `PUT /api/config`
    // could create arbitrary directories (F-03). It belongs here, where the
    // operator has actually asked the service to run.
    if let Err(e) = std::fs::create_dir_all(&config.paths.target_folder) {
        handle.abandon_start(generation);
        return Err(format!(
            "Cannot create target folder '{}': {}",
            config.paths.target_folder, e
        ));
    }

    // W-4. The T-3 ownership claim used to run only in `run_service`, against
    // the folder configured at process start. `PUT /api/config` with a new
    // `target_folder` followed by `POST /api/service/start` then published into
    // a folder nobody had claimed -- or one another registry already owns.
    // Every start comes through here, so this is where it is enforced.
    if let Err(e) = crate::media_root::claim(Path::new(&config.paths.target_folder), registry_id) {
        tracing::error!("{}", e);
        handle.abandon_start(generation);
        return Err(e.to_string());
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<ServiceCmd>(1);
    *handle.cmd_tx.lock() = Some(cmd_tx);

    // A fresh exit signal per run; the previous one has already been consumed
    // by the reaper that let this start happen.
    let exit = Arc::new(WorkerExit::default());
    *handle.worker_exit.lock() = exit.clone();
    let exit_for_reaper = exit.clone();

    let watch = PathBuf::from(&config.paths.watch_folder);
    let target = PathBuf::from(&config.paths.target_folder);
    let tools = tools.clone();
    let jobs = job_queue.clone();
    let cfg = config.clone();
    let active_pids = handle.active_pids.clone();
    // Clone for the worker thread; the closure below captures this clone by move.
    let handle_for_thread = handle.clone();
    // The retry_tx cleanup slot is also held by this handle. Once handle_for_thread moves
    // into the closure, only the closure can clear it on stop.
    let cleanup_retry_tx = handle_for_thread.retry_tx.clone();

    handle.add_log("info", "Transcoding service started");

    let per_encode_threads = config
        .encoding
        .effective_threads_per_encode(config.ingestion.max_concurrency);
    let total_threads = config
        .encoding
        .effective_total_threads(config.ingestion.max_concurrency);
    handle.add_log(
        "info",
        &format!(
            "CPU budget: {} cores / max_concurrency={} -> {} threads/encode ({} total)",
            if config.encoding.cpu_cores > 0 {
                config.encoding.cpu_cores.to_string()
            } else {
                "auto".to_string()
            },
            config.ingestion.max_concurrency,
            per_encode_threads,
            total_threads,
        ),
    );

    let worker = std::thread::spawn(move || {
        // `Runtime::new().unwrap()` here was a panic on a worker thread with no
        // one to catch it: the service reported Running and then silently did
        // nothing forever (F-30). A runtime that will not build is a start
        // failure, reported as one.
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("Failed to build the processing runtime: {}", e);
                handle_for_thread.add_log(
                    "error",
                    &format!("Could not start the processing runtime: {}", e),
                );
                if let Ok(mut slot) = cleanup_retry_tx.lock() {
                    *slot = None;
                }
                handle_for_thread.mark_stopped(generation);
                exit.finish();
                return;
            }
        };

        rt.block_on(async move {
            let _ = std::fs::create_dir_all(&target);

            // Recovery sweep: purge DB rows whose source is still in the watch folder so the
            // watcher re-queues them; also purge rows whose source is gone. This is the
            // "auto-purge + retry on start" behaviour.
            match db::recover_failed_assets(
                &pool,
                &watch,
                cfg.ingestion.auto_retry_on_start,
                |sha| crate::processor::qc_verdict_key(sha, &cfg),
            )
            .await
            {
                Ok(o) => {
                    if o.purged_for_retry > 0 || o.purged_dead > 0 || o.kept_dead > 0 {
                        handle_for_thread.add_log(
                            "info",
                            &format!(
                                "Recovery: purged {} retryable / {} dead, kept {} dead rows for inspection",
                                o.purged_for_retry, o.purged_dead, o.kept_dead
                            ),
                        );
                    }
                    // Said separately, and said whenever it is non-zero. An
                    // operator who leaves a file in the watch folder and sees
                    // nothing happen is owed the reason, or they will conclude
                    // the watcher is broken.
                    if o.kept_permanent > 0 {
                        handle_for_thread.add_log(
                            "info",
                            &format!(
                                "Recovery: {} asset(s) already failed permanently on this exact                                  media and these exact settings, so they were not queued again.                                  Replace the file, change the encoding or validation settings,                                  or delete the asset to have it re-examined.",
                                o.kept_permanent
                            ),
                        );
                    }
                }
                Err(e) => {
                    handle_for_thread.add_log("error", &format!("Recovery sweep DB error: {}", e));
                }
            }

            let orphan_cleaned = crate::processor::cleanup_orphan_staging_files(&target.join("videos"), 1800);
            if orphan_cleaned > 0 {
                handle_for_thread.add_log(
                    "info",
                    &format!("Startup sweep: removed {} orphaned staging temp file(s)", orphan_cleaned),
                );
            }

            let (file_tx, mut file_rx) = mpsc::channel::<PathBuf>(256);
            let (retry_tx, mut retry_rx) = mpsc::channel::<RetryRequest>(256);
            if let Ok(mut slot) = handle_for_thread.retry_tx.lock() {
                *slot = Some(retry_tx);
            }
            let sem = Arc::new(tokio::sync::Semaphore::new(cfg.ingestion.max_concurrency));

            let excl_ext = cfg.ingestion.exclude_extensions.clone();
            let incl_ext = cfg.ingestion.include_extensions.clone();
            let settle_secs = cfg.ingestion.settle_secs;
            let poll_secs = cfg.ingestion.poll_secs;
            let stable_polls_min = cfg.ingestion.stable_polls_min;
            let w_root = watch.clone();

            tokio::spawn(async move {
                crate::watcher::watch_loop(
                    w_root, settle_secs, poll_secs, stable_polls_min,
                    incl_ext, excl_ext, file_tx,
                )
                .await;
            });

            let gate = handle_for_thread.clone();

            let mut cmd_rx = cmd_rx;
            loop {
                tokio::select! {
                    Some(path) = file_rx.recv() => {
                        // A file the watcher offered may already have a pending
                        // record -- one recovered at startup, or one a retry
                        // re-queued. Adopt it rather than creating a duplicate.
                        let existing = jobs
                            .find_pending_by_input_path(&path.to_string_lossy())
                            .map(|j| j.id);
                        dispatch_one(&tools, &cfg, &jobs, &target, &sem, &pool, &active_pids, path, existing, &gate, generation);
                    }
                    Some(retry) = retry_rx.recv() => {
                        handle_for_thread.add_log(
                            "info",
                            &format!("Manual retry submitted for {}", retry.path.display()),
                        );
                        dispatch_one(&tools, &cfg, &jobs, &target, &sem, &pool, &active_pids, retry.path, retry.job_id, &gate, generation);
                    }
                    cmd = cmd_rx.recv() => {
                        if let Some(ServiceCmd::Stop) = cmd {
                            break;
                        }
                    }
                }
            }
        });

        // Service stopped: clear retry channel so API retries fail fast until the service restarts.
        if let Ok(mut slot) = cleanup_retry_tx.lock() {
            *slot = None;
        }
        // The reaper, not the worker, publishes `Stopped` — see
        // `stop_processing`. Announcing it here would let a start race a
        // thread that has not yet unwound.
        exit.finish();
    });

    if let Some(orphan) = handle.install_worker(generation, worker) {
        // A stop landed between the spawn and here. The thread has already been
        // told to stop (`cmd_tx` was installed before the spawn), so all that is
        // left is to wait for it before anyone reports `Stopped`.
        spawn_reaper(handle.clone(), generation, orphan, exit_for_reaper);
    }

    Ok(())
}

/// Wait for a processing thread to unwind, then publish `Stopped`.
///
/// Runs on its own thread: a stop must not block the HTTP handler that asked
/// for it, and `JoinHandle` cannot be joined with a timeout.
fn spawn_reaper(
    handle: ServiceHandle,
    generation: u64,
    worker: std::thread::JoinHandle<()>,
    exit: Arc<WorkerExit>,
) {
    std::thread::spawn(move || {
        if !exit.wait(WORKER_JOIN_WARN_AFTER) {
            tracing::warn!(
                "Processing thread has not exited {}s after the stop request; still waiting",
                WORKER_JOIN_WARN_AFTER.as_secs()
            );
        }
        if worker.join().is_err() {
            tracing::error!("Processing thread panicked during shutdown");
        }
        handle.mark_stopped(generation);
        handle.add_log("info", "Service stopped");
    });
}

/// Dispatches one input file through the concurrency semaphore into the processor.
/// The semaphore permit is acquired asynchronously and held inside the blocking task
/// until processing completes, strictly enforcing max_concurrency.
fn dispatch_one(
    tools: &bootstrap::ToolPaths,
    cfg: &config::AppConfig,
    jobs: &JobQueue,
    target: &std::path::Path,
    sem: &Arc<tokio::sync::Semaphore>,
    pool: &SqlitePool,
    active_pids: &ActivePids,
    path: std::path::PathBuf,
    // The existing job record to reuse, if this file already has one.
    existing_job_id: Option<String>,
    // The handle and the generation this dispatch belongs to. A task that was
    // queued behind a full semaphore before a stop must not wake up inside the
    // next run (T2-5).
    gate: &ServiceHandle,
    generation: u64,
) {
    let t = tools.clone();
    let c = cfg.clone();
    let jq = jobs.clone();
    let tg = target.to_path_buf();
    let s = sem.clone();
    let p = pool.clone();
    let apids = active_pids.clone();
    let gate = gate.clone();
    let existing = existing_job_id.and_then(|id| jobs.get(&id));

    tokio::spawn(async move {
        // Wait for an available concurrency slot without blocking the main event loop
        let Ok(permit) = s.acquire_owned().await else {
            return;
        };

        // If the service stopped -- or stopped and started again -- while this
        // task waited for a permit, exit cleanly.
        if !gate.is_current_run(generation) {
            return;
        }

        // PL-02. The processor used to have no idea the service was stopping:
        // a stop killed ffmpeg, the processor read that as a retryable
        // "ffmpeg exited with code 1", and relaunched an ffmpeg nobody would
        // ever kill -- which is also why a stop sat in `Stopping` for the
        // whole re-encode.
        let run_gate = gate.clone();
        let still_running: crate::processor::StillRunning =
            Arc::new(move || run_gate.is_current_run(generation));
        let _ = tokio::task::spawn_blocking(move || {
            // Permit is moved here and kept alive for the full duration of processing
            let _held_permit = permit;
            crate::processor::process_file_sync_gated(
                &jq,
                &t,
                &tg,
                &path,
                &c,
                &p,
                apids,
                existing,
                still_running,
            );
        })
        .await;
    });
}

/// Ask the processing loop to stop, and reap it.
///
/// Returns immediately. The state goes to `Stopping` here and only reaches
/// `Stopped` once the worker thread has actually unwound, which is what stops a
/// restart from racing a teardown (F-17). Calling it while already stopped or
/// stopping is a no-op.
pub fn stop_processing(handle: &ServiceHandle) {
    let Some((generation, worker, exit)) = handle.begin_stop() else {
        return;
    };

    if let Some(ref tx) = *handle.cmd_tx.lock() {
        let _ = tx.try_send(ServiceCmd::Stop);
    }
    if let Ok(mut slot) = handle.retry_tx.lock() {
        *slot = None;
    }
    handle.kill_active_ffmpeg();
    handle.add_log("info", "Service stop requested");

    match worker {
        Some(worker) => spawn_reaper(handle.clone(), generation, worker, exit),
        // No thread was ever spawned for this generation -- a start that failed
        // before the spawn, or a stop racing a start that will find its handle
        // rejected by `install_worker` and reap it itself. Nothing to wait for.
        None => handle.mark_stopped(generation),
    }
}

/// Block until the service reaches `Stopped`, up to `timeout`.
///
/// Used by the shutdown path and by tests; a plain `stop_processing` is
/// asynchronous by design so an HTTP handler is not held open by a teardown.
pub fn wait_until_stopped(handle: &ServiceHandle, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if handle.state() == ServiceState::Stopped {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn trigger_download(handle: &ServiceHandle) -> bool {
    let mut status = handle.download_status.lock();
    if status.is_some() {
        return false;
    }
    *status = Some("downloading".to_string());
    drop(status);
    handle.add_log("info", "Starting FFmpeg download (pinned 9.0.2 essentials build)");

    let h = handle.clone();
    std::thread::spawn(move || {
        // A panic in here used to leave `download_status` stuck at
        // "downloading" forever, disabling the button until a process restart
        // (F-24). Always land on a terminal status.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            crate::bootstrap::download_ffmpeg,
        ));
        let status = match outcome {
            Ok(Ok(_)) => "ok".to_string(),
            Ok(Err(e)) => format!("error: {}", e),
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                tracing::error!("PANIC in FFmpeg download worker: {}", msg);
                format!("error: download worker panicked: {}", msg)
            }
        };
        *h.download_status.lock() = Some(status);
    });
    true
}

pub fn poll_download_status(handle: &ServiceHandle) -> Option<String> {
    let mut s = handle.download_status.lock();
    if let Some(ref msg) = *s {
        if msg == "ok" {
            *s = None;
            drop(s);
            let (_, status) = crate::bootstrap::audit_toolchain();
            let ver = status.ffmpeg_version.unwrap_or_default();
            handle.add_log("info", &format!("FFmpeg {} downloaded and ready", ver));
            return Some("ok".into());
        } else if msg.starts_with("error:") {
            let err = msg.clone();
            *s = None;
            drop(s);
            handle.add_log("error", &err);
            return Some(err);
        }
    }
    s.clone()
}

#[cfg(test)]
mod pid_tests {
    use super::*;

    fn map_with(entries: &[(&str, u32)]) -> ActivePids {
        let mut m = HashMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), *v);
        }
        Arc::new(StdMutex::new(m))
    }

    #[test]
    fn cancel_kills_only_the_requesting_job() {
        let pids = map_with(&[("job-a", 1111), ("job-b", 2222), ("job-c", 3333)]);
        let killed = Arc::new(StdMutex::new(Vec::new()));

        let sink = killed.clone();
        let found = kill_ffmpeg_for_job(&pids, "job-a", move |pid| {
            sink.lock().unwrap().push(pid);
        });

        assert!(found);
        assert_eq!(*killed.lock().unwrap(), vec![1111]);

        // B and C keep running and stay registered.
        let remaining = pids.lock().unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining.get("job-b"), Some(&2222));
        assert_eq!(remaining.get("job-c"), Some(&3333));
        assert!(remaining.get("job-a").is_none());
    }

    #[test]
    fn cancel_of_unregistered_job_kills_nothing() {
        let pids = map_with(&[("job-a", 1111)]);
        let killed = Arc::new(StdMutex::new(Vec::new()));

        let sink = killed.clone();
        let found = kill_ffmpeg_for_job(&pids, "job-z", move |pid| {
            sink.lock().unwrap().push(pid);
        });

        assert!(!found);
        assert!(killed.lock().unwrap().is_empty());
        assert_eq!(pids.lock().unwrap().len(), 1);
    }

    #[test]
    fn service_stop_still_clears_every_pid() {
        let handle = ServiceHandle::new();
        {
            let mut m = handle.active_pids.lock().unwrap();
            m.insert("job-a".into(), 1111);
            m.insert("job-b".into(), 2222);
        }
        assert_eq!(handle.active_pids_count(), 2);
        handle.kill_active_ffmpeg();
        assert_eq!(handle.active_pids_count(), 0);
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::time::Duration;

    /// SB-03 / UX-05: the ring is a deque with a monotonic cursor, so a viewer
    /// can append what is new instead of repainting 500 rows, and can tell
    /// that it fell behind.
    #[test]
    fn the_log_ring_drops_from_the_front_and_hands_out_a_usable_cursor() {
        let handle = ServiceHandle::new();

        for i in 0..5 {
            handle.add_log("info", &format!("line {}", i));
        }

        // No cursor: the whole ring, as a bare array of strings, as before.
        let all = handle.get_logs();
        assert_eq!(all.len(), 5);
        assert!(all[0].contains("[INFO] line 0"));

        let page = handle.logs_since(0);
        assert_eq!(page.lines.len(), 5);
        assert_eq!(page.lines[0].seq, 0);
        assert_eq!(page.next, 5);
        assert!(!page.dropped);

        // Nothing new since the cursor.
        let page = handle.logs_since(page.next);
        assert!(page.lines.is_empty());
        assert_eq!(page.next, 5);
        assert!(!page.dropped);

        handle.add_log("warn", "line 5");
        let page = handle.logs_since(5);
        assert_eq!(page.lines.len(), 1);
        assert!(page.lines[0].text.contains("[WARN] line 5"));
        assert_eq!(page.next, 6);

        // Overflow the ring: the cap holds, the oldest lines go, and a stale
        // cursor is reported as dropped rather than silently returning a gap.
        for i in 6..700 {
            handle.add_log("info", &format!("line {}", i));
        }
        assert_eq!(handle.get_logs().len(), 500);
        let stale = handle.logs_since(0);
        assert!(stale.dropped, "a cursor off the back of the ring is dropped");
        assert_eq!(stale.lines.len(), 500);
        assert_eq!(stale.next, 700);

        let fresh = handle.logs_since(699);
        assert!(!fresh.dropped);
        assert_eq!(fresh.lines.len(), 1);
        assert!(fresh.lines[0].text.contains("line 699"));
    }

    /// Stand-in for `start_processing_loop`'s bookkeeping, without the Tokio
    /// runtime, database pool and FFmpeg toolchain a real start needs. The
    /// worker is a thread that parks until told to finish, which is exactly the
    /// shape of the real one: a loop that exits on `ServiceCmd::Stop`.
    fn fake_start(handle: &ServiceHandle) -> (u64, Arc<WorkerExit>, Arc<StdMutex<bool>>) {
        let generation = handle.begin_start().expect("start must be allowed");
        let exit = Arc::new(WorkerExit::default());
        *handle.worker_exit.lock() = exit.clone();

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<ServiceCmd>(1);
        *handle.cmd_tx.lock() = Some(cmd_tx);

        let release = Arc::new(StdMutex::new(false));
        let release_for_thread = release.clone();
        let exit_for_thread = exit.clone();
        let worker = std::thread::spawn(move || {
            // Drain the stop command so the channel behaves like the real loop.
            while cmd_rx.try_recv().is_err() && !*release_for_thread.lock().unwrap() {
                std::thread::sleep(Duration::from_millis(5));
            }
            while !*release_for_thread.lock().unwrap() {
                std::thread::sleep(Duration::from_millis(5));
            }
            exit_for_thread.finish();
        });

        assert!(
            handle.install_worker(generation, worker).is_none(),
            "nothing has asked for a stop, so the handle must be accepted"
        );
        (generation, exit, release)
    }

    #[test]
    fn a_start_while_running_is_refused() {
        let handle = ServiceHandle::new();
        let (_gen, _exit, release) = fake_start(&handle);
        assert_eq!(handle.state(), ServiceState::Running);

        assert_eq!(
            handle.begin_start().unwrap_err(),
            "Service already running".to_string()
        );

        *release.lock().unwrap() = true;
        stop_processing(&handle);
        assert!(wait_until_stopped(&handle, Duration::from_secs(5)));
    }

    #[test]
    fn a_start_immediately_after_a_stop_is_refused_until_the_worker_exits() {
        let handle = ServiceHandle::new();
        let (_gen, _exit, release) = fake_start(&handle);

        // The worker is still parked, so the stop cannot complete yet.
        stop_processing(&handle);
        assert_eq!(handle.state(), ServiceState::Stopping);
        assert_eq!(
            handle.begin_start().unwrap_err(),
            "Service is stopping".to_string()
        );

        // Let the worker unwind; the reaper then publishes Stopped.
        *release.lock().unwrap() = true;
        assert!(
            wait_until_stopped(&handle, Duration::from_secs(5)),
            "the reaper must report Stopped once the worker thread has exited"
        );

        // And only now may the service start again.
        let next = handle.begin_start().expect("start must succeed once stopped");
        assert_eq!(next, 2, "each start gets its own generation");
        handle.abandon_start(next);
    }

    #[test]
    fn a_repeated_stop_is_a_no_op() {
        let handle = ServiceHandle::new();
        let (_gen, _exit, release) = fake_start(&handle);

        stop_processing(&handle);
        // A second stop must not spawn a second reaper for a worker handle that
        // has already been taken, nor reset the generation.
        stop_processing(&handle);
        assert_eq!(handle.state(), ServiceState::Stopping);

        *release.lock().unwrap() = true;
        assert!(wait_until_stopped(&handle, Duration::from_secs(5)));
        assert_eq!(handle.generation(), 1);
    }

    #[test]
    fn a_stop_from_stopped_does_nothing() {
        let handle = ServiceHandle::new();
        assert_eq!(handle.state(), ServiceState::Stopped);
        stop_processing(&handle);
        assert_eq!(handle.state(), ServiceState::Stopped);
    }

    #[test]
    fn work_queued_under_an_old_generation_is_dropped() {
        let handle = ServiceHandle::new();
        let (first, _exit, release) = fake_start(&handle);
        assert!(handle.is_current_run(first));

        *release.lock().unwrap() = true;
        stop_processing(&handle);
        assert!(wait_until_stopped(&handle, Duration::from_secs(5)));

        // A second run. A task that was waiting for a semaphore permit under
        // the first one must not process a file inside this one.
        let (second, _exit2, release2) = fake_start(&handle);
        assert!(handle.is_current_run(second));
        assert!(
            !handle.is_current_run(first),
            "a dispatch from the previous run must not survive a restart"
        );

        *release2.lock().unwrap() = true;
        stop_processing(&handle);
        assert!(wait_until_stopped(&handle, Duration::from_secs(5)));
    }

    #[test]
    fn a_retry_is_refused_unless_the_service_is_running() {
        let handle = ServiceHandle::new();
        assert_eq!(
            handle
                .submit_retry(PathBuf::from("x.mxf"), None)
                .unwrap_err(),
            "Service is not running".to_string()
        );

        let (_gen, _exit, release) = fake_start(&handle);
        // Running, but `start_processing_loop` is what installs the retry
        // channel, so the fake start has none: the failure moves on from
        // "not running" to "no channel", which is the distinction that matters.
        assert_eq!(
            handle
                .submit_retry(PathBuf::from("x.mxf"), None)
                .unwrap_err(),
            "retry channel not established".to_string()
        );

        *release.lock().unwrap() = true;
        stop_processing(&handle);
        assert!(wait_until_stopped(&handle, Duration::from_secs(5)));
    }

    /// W-4. `PUT /api/config` can point the service at a new `target_folder`
    /// while it is stopped; the next start must claim that folder, and refuse
    /// one another registry owns, exactly as `run_service` does at boot.
    #[tokio::test]
    async fn a_start_refuses_a_target_folder_owned_by_another_registry() {
        let root = std::env::temp_dir().join(format!("pt_test_w4_{}", uuid::Uuid::new_v4()));
        let watch = root.join("watch");
        let target = root.join("target");
        std::fs::create_dir_all(&watch).unwrap();
        crate::media_root::claim(&target, "someone-else").unwrap();
        let marker = target.join(crate::media_root::MARKER_FILE_NAME);
        let before = std::fs::read_to_string(&marker).unwrap();

        let pool = Arc::new(crate::db::init_pool(&root.join("test.db")).await.unwrap());
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let jobs = JobQueue::new_in_memory(tx);
        let tools = ToolPaths {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
        };
        let mut cfg = AppConfig::default();
        cfg.paths.watch_folder = watch.to_string_lossy().to_string();
        cfg.paths.target_folder = target.to_string_lossy().to_string();

        let handle = ServiceHandle::new();
        let err = start_processing_loop(&handle, &cfg, &jobs, &tools, pool.clone(), "ours")
            .unwrap_err();
        assert!(err.contains("already owned"), "{err}");
        assert_eq!(handle.state(), ServiceState::Stopped, "a refused start leaves nothing running");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            before,
            "the other registry's claim is untouched"
        );

        // No target at all is refused too, rather than claiming the cwd.
        cfg.paths.target_folder = String::new();
        let err = start_processing_loop(&handle, &cfg, &jobs, &tools, pool, "ours").unwrap_err();
        assert!(err.contains("not configured"), "{err}");
        assert_eq!(handle.state(), ServiceState::Stopped);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_reported_state_string_is_stable() {
        assert_eq!(ServiceState::Stopped.as_str(), "stopped");
        assert_eq!(ServiceState::Starting.as_str(), "starting");
        assert_eq!(ServiceState::Running.as_str(), "running");
        assert_eq!(ServiceState::Stopping.as_str(), "stopping");
    }
}
