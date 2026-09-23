use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Pending,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Pending => "Pending",
            JobState::Processing => "Processing",
            JobState::Completed => "Completed",
            JobState::Failed => "Failed",
            JobState::Cancelled => "Cancelled",
        }
    }
}

impl std::str::FromStr for JobState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Pending" => Ok(JobState::Pending),
            "Processing" => Ok(JobState::Processing),
            "Completed" => Ok(JobState::Completed),
            "Failed" => Ok(JobState::Failed),
            "Cancelled" => Ok(JobState::Cancelled),
            other => Err(format!("Unknown JobState: {}", other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Probing,
    Planned,
    Encoding,
    NormalizingAudio,
    Validating,
    Publishing,
    Completed,
    Failed,
    CancelRequested,
    Cancelled,
    Recoverable,
    /// Terminal, and **not** an error: the file was already ingested, confirmed
    /// byte-identical to an existing asset, so no work was needed (T2-6).
    ///
    /// Before this existed the only terminal state reachable from a queued job
    /// was `Failed`, so a duplicate was reported as a failure and PlayOut had
    /// to key on `error_category` to avoid showing it as one. It maps to the
    /// v1 `Completed` state, so the wire contract does not change.
    Skipped,
}

impl JobPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobPhase::Completed
                | JobPhase::Cancelled
                | JobPhase::Failed
                | JobPhase::Skipped
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            JobPhase::Queued => "queued",
            JobPhase::Probing => "probing",
            JobPhase::Planned => "planned",
            JobPhase::Encoding => "encoding",
            JobPhase::NormalizingAudio => "normalizing_audio",
            JobPhase::Validating => "validating",
            JobPhase::Publishing => "publishing",
            JobPhase::Completed => "completed",
            JobPhase::Failed => "failed",
            JobPhase::CancelRequested => "cancel_requested",
            JobPhase::Cancelled => "cancelled",
            JobPhase::Recoverable => "recoverable",
            JobPhase::Skipped => "skipped",
        }
    }

    pub fn as_v1_state(&self) -> JobState {
        match self {
            JobPhase::Queued => JobState::Pending,
            JobPhase::Probing
            | JobPhase::Planned
            | JobPhase::Encoding
            | JobPhase::NormalizingAudio
            | JobPhase::Validating
            | JobPhase::Publishing
            | JobPhase::CancelRequested
            | JobPhase::Recoverable => JobState::Processing,
            // Deliberately `Completed`: no work was needed and nothing went
            // wrong. A client that only reads `state` sees a job that finished.
            JobPhase::Completed | JobPhase::Skipped => JobState::Completed,
            JobPhase::Failed => JobState::Failed,
            JobPhase::Cancelled => JobState::Cancelled,
        }
    }

    pub fn can_transition_to(&self, next: JobPhase) -> bool {
        if *self == next {
            return true;
        }
        match self {
            JobPhase::Queued => matches!(
                next,
                JobPhase::Probing
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
                    | JobPhase::Failed
                    | JobPhase::Skipped
            ),
            JobPhase::Probing => matches!(
                next,
                JobPhase::Planned
                    | JobPhase::NormalizingAudio
                    | JobPhase::Failed
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
                    | JobPhase::Skipped
            ),
            JobPhase::NormalizingAudio => matches!(
                next,
                JobPhase::Planned
                    | JobPhase::Encoding
                    | JobPhase::Failed
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
            ),
            JobPhase::Planned => matches!(
                next,
                JobPhase::Encoding
                    | JobPhase::Failed
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
            ),
            JobPhase::Encoding => matches!(
                next,
                JobPhase::Validating
                    | JobPhase::Failed
                    | JobPhase::Recoverable
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
            ),
            JobPhase::Validating => matches!(
                next,
                JobPhase::Publishing
                    | JobPhase::Failed
                    | JobPhase::Recoverable
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
            ),
            JobPhase::Publishing => matches!(next, JobPhase::Completed | JobPhase::Failed),
            JobPhase::Recoverable => matches!(
                next,
                JobPhase::Encoding
                    | JobPhase::Probing
                    | JobPhase::Failed
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
            ),
            JobPhase::CancelRequested => matches!(next, JobPhase::Cancelled | JobPhase::Failed),
            JobPhase::Failed => matches!(next, JobPhase::Queued),
            // A skip is re-triable: the operator may have purged the asset that
            // caused it, and then the file genuinely does need ingesting.
            JobPhase::Skipped => matches!(next, JobPhase::Queued),
            JobPhase::Completed | JobPhase::Cancelled => false,
        }
    }
}

impl std::str::FromStr for JobPhase {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(JobPhase::Queued),
            "probing" => Ok(JobPhase::Probing),
            "planned" => Ok(JobPhase::Planned),
            "encoding" => Ok(JobPhase::Encoding),
            "normalizing_audio" => Ok(JobPhase::NormalizingAudio),
            "validating" => Ok(JobPhase::Validating),
            "publishing" => Ok(JobPhase::Publishing),
            "completed" => Ok(JobPhase::Completed),
            "failed" => Ok(JobPhase::Failed),
            "cancel_requested" => Ok(JobPhase::CancelRequested),
            "cancelled" => Ok(JobPhase::Cancelled),
            "recoverable" => Ok(JobPhase::Recoverable),
            "skipped" => Ok(JobPhase::Skipped),
            other => Err(format!("Unknown JobPhase: {}", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub input_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub state: JobState,
    pub phase: JobPhase,
    pub progress: f32,
    pub current_stage: String,
    pub duration_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<Vec<String>>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub max_attempts: u32,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leased_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<String>,
    #[serde(default)]
    pub cancel_requested: bool,
    pub source_frame_count: i64,
    pub current_frame: i64,
    pub encode_fps: f64,
    pub encode_bitrate: String,
    pub encode_speed: String,
    pub current_time_ms: i64,
    pub duration_ms: i64,
}

impl JobRecord {
    pub fn new(input_path: &str, profile: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            input_path: input_path.to_string(),
            output_path: None,
            profile: profile.to_string(),
            uuid: None,
            state: JobState::Pending,
            phase: JobPhase::Queued,
            progress: 0.0,
            current_stage: "Queued".to_string(),
            duration_secs: 0.0,
            error: None,
            error_category: None,
            stderr_log: None,
            attempt: 1,
            max_attempts: 1,
            created_at: Utc::now().to_rfc3339(),
            started_at: None,
            finished_at: None,
            fingerprint: None,
            request_hash: None,
            worker_id: None,
            leased_until: None,
            heartbeat_at: None,
            cancel_requested: false,
            source_frame_count: 0,
            current_frame: 0,
            encode_fps: 0.0,
            encode_bitrate: String::new(),
            encode_speed: String::new(),
            current_time_ms: 0,
            duration_ms: 0,
        }
    }

    pub fn transition_to(
        &mut self,
        next: JobPhase,
        stage_description: Option<String>,
    ) -> Result<(), String> {
        if !self.phase.can_transition_to(next) {
            return Err(format!(
                "Illegal state transition from {:?} to {:?}",
                self.phase, next
            ));
        }
        self.phase = next;
        self.state = next.as_v1_state();
        if let Some(desc) = stage_description {
            self.current_stage = desc;
        } else {
            self.current_stage = format!("{:?}", next);
        }
        let now = Utc::now().to_rfc3339();
        if self.started_at.is_none() && next != JobPhase::Queued {
            self.started_at = Some(now.clone());
        }
        if next.is_terminal() {
            self.finished_at = Some(now);
        }
        Ok(())
    }
}

/// How many records may be queued for persistence before writes are coalesced
/// in memory instead. Sized so a burst of FFmpeg progress lines across every
/// concurrent encode fits without ever blocking a worker thread.
const PERSIST_CHANNEL_CAPACITY: usize = 1024;

/// The persister flushes at least this often.
const PERSIST_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// ...or as soon as this many distinct jobs are waiting, whichever comes first.
const PERSIST_FLUSH_BATCH: usize = 100;

/// What the persister task receives.
enum PersistMsg {
    /// A snapshot of a job. Newer snapshots for the same id supersede older
    /// ones, which is what makes coalescing safe.
    Record(Box<JobRecord>),
    /// Flush everything outstanding and acknowledge. Used on shutdown, and by
    /// tests that need a deterministic point to assert after.
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Channel, dirty set and pool behind the coalescing persister.
struct Persistence {
    tx: tokio::sync::mpsc::Sender<PersistMsg>,
    /// Taken by `spawn_persister`. Held here so `JobQueue::new` keeps its
    /// signature and the caller decides when the task starts.
    rx: parking_lot::Mutex<Option<tokio::sync::mpsc::Receiver<PersistMsg>>>,
    /// Jobs whose latest snapshot was dropped because the channel was full.
    /// The persister re-reads these from memory on its next tick, so a full
    /// channel costs freshness, never correctness.
    dirty: parking_lot::Mutex<std::collections::HashSet<String>>,
    pool: Arc<SqlitePool>,
}

/// One server-sent event, kept as the two strings the wire format needs.
///
/// Shared behind an `Arc` because every subscriber gets the same frame and
/// none of them mutate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// The `event:` field.
    pub event: String,
    /// The `data:` field, already serialised by the producer.
    pub data: String,
}

/// The counters behind `/api/stats`, produced without cloning the queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct JobStateCounts {
    pub pending: usize,
    pub active: usize,
    pub completed: usize,
    pub failed: usize,
    pub total: usize,
}

/// Why a job could not be taken off the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DismissError {
    NotFound,
    /// The job is still pending or running. Cancel it first; dismissing a
    /// record whose encoder is still going would leave an ffmpeg process with
    /// nothing tracking it.
    StillActive,
}

/// Why a cancel was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelError {
    NotFound,
    /// Completed, failed, skipped or already cancelled: nothing left to stop.
    AlreadyFinished,
    /// The atomic publish is under way and cannot be interrupted.
    Publishing,
}

/// Why a retry was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryError {
    NotFound,
    /// Only a failed or skipped job can be retried; this one is not.
    NotRetryable(JobPhase),
}

#[derive(Clone)]
pub struct JobQueue {
    jobs: Arc<RwLock<Vec<JobRecord>>>,
    event_tx: broadcast::Sender<Arc<SseFrame>>,
    persist: Option<Arc<Persistence>>,
}

impl JobQueue {
    pub fn new(event_tx: broadcast::Sender<Arc<SseFrame>>, pool: Option<Arc<SqlitePool>>) -> Self {
        let persist = pool.map(|pool| {
            let (tx, rx) = tokio::sync::mpsc::channel(PERSIST_CHANNEL_CAPACITY);
            Arc::new(Persistence {
                tx,
                rx: parking_lot::Mutex::new(Some(rx)),
                dirty: parking_lot::Mutex::new(std::collections::HashSet::new()),
                pool,
            })
        });
        Self {
            jobs: Arc::new(RwLock::new(Vec::new())),
            event_tx,
            persist,
        }
    }

    /// Queue a snapshot for persistence. Never blocks and never fails loudly.
    ///
    /// Before T2-4 this spawned an independent upsert per call -- and from the
    /// std progress thread, which has no Tokio handle, that meant a brand new
    /// OS thread *and* a new current-thread runtime per FFmpeg progress line.
    /// Writes also raced: two rapid updates could land out of order and leave
    /// the row claiming "Encoding 97%" after the job had completed (F-13).
    fn enqueue_persist(&self, job: JobRecord) {
        let Some(p) = self.persist.as_ref() else {
            return;
        };
        let id = job.id.clone();
        match p.tx.try_send(PersistMsg::Record(Box::new(job))) {
            Ok(()) => {
                // This snapshot supersedes anything previously dropped.
                let mut dirty = p.dirty.lock();
                if !dirty.is_empty() {
                    dirty.remove(&id);
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                p.dirty.lock().insert(id);
            }
            // The persister is gone (shutdown). Dropping is correct.
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    /// Start the persister. Call once, on the main runtime, after `new`.
    ///
    /// Returns `None` when the queue has no pool (in-memory tests) or the task
    /// has already been started.
    pub fn spawn_persister(&self) -> Option<tokio::task::JoinHandle<()>> {
        let p = self.persist.clone()?;
        let rx = p.rx.lock().take()?;
        let jobs = self.jobs.clone();
        Some(tokio::spawn(persister_loop(p, rx, jobs)))
    }

    /// Flush every outstanding write and wait for it to hit the database.
    ///
    /// Called on shutdown before the pool is closed, so a stop does not discard
    /// the final state of the jobs it just stopped.
    pub async fn flush_persister(&self) {
        let Some(p) = self.persist.as_ref() else {
            return;
        };
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        if p.tx.send(PersistMsg::Flush(ack_tx)).await.is_err() {
            return;
        }
        let _ = ack_rx.await;
    }

    #[allow(dead_code)]
    pub fn new_in_memory(event_tx: broadcast::Sender<Arc<SseFrame>>) -> Self {
        Self::new(event_tx, None)
    }

    pub fn populate(&self, records: Vec<JobRecord>) {
        let mut jobs = self.jobs.write();
        for r in records {
            if !jobs.iter().any(|j| j.id == r.id) {
                jobs.push(r);
            }
        }
    }

    pub fn event_sender(&self) -> broadcast::Sender<Arc<SseFrame>> {
        self.event_tx.clone()
    }

    pub fn push(&self, job: JobRecord) {
        self.jobs.write().push(job.clone());
        self.announce(&job);
        self.enqueue_persist(job);
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut JobRecord)) {
        let mut jobs = self.jobs.write();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            f(job);
            let job_clone = job.clone();
            drop(jobs);
            self.announce(&job_clone);
            self.enqueue_persist(job_clone);
        }
    }

    /// Mutate a job in memory without queueing a write.
    ///
    /// For the FFmpeg progress path, which produces a line every few frames.
    /// The UI reads the in-memory record, so it stays live; the row is written
    /// by the throttled `persist_now` alongside the progress broadcast.
    pub fn update_local(&self, id: &str, f: impl FnOnce(&mut JobRecord)) {
        let mut jobs = self.jobs.write();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            f(job);
        }
    }

    /// Queue the current snapshot of one job for persistence.
    pub fn persist_now(&self, id: &str) {
        let snapshot = self.jobs.read().iter().find(|j| j.id == id).cloned();
        if let Some(job) = snapshot {
            self.enqueue_persist(job);
        }
    }

    /// The oldest pending job for this input path, if any.
    ///
    /// The dispatcher uses this to adopt a record recovered at startup instead
    /// of creating a second one for the same file, which is how retries and
    /// crash recovery used to leave permanent ghosts (F-13).
    pub fn find_pending_by_input_path(&self, path: &str) -> Option<JobRecord> {
        let jobs = self.jobs.read();
        let mut matches: Vec<&JobRecord> = jobs
            .iter()
            .filter(|j| j.state == JobState::Pending && j.input_path == path)
            .collect();
        matches.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        matches.first().map(|j| (*j).clone())
    }

    pub fn transition(
        &self,
        id: &str,
        next: JobPhase,
        stage_description: Option<String>,
        f: impl FnOnce(&mut JobRecord),
    ) -> Result<(), String> {
        let mut jobs = self.jobs.write();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.transition_to(next, stage_description)?;
            f(job);
            let job_clone = job.clone();
            drop(jobs);
            self.announce(&job_clone);
            self.enqueue_persist(job_clone);
            Ok(())
        } else {
            Err(format!("Job {} not found", id))
        }
    }

    pub fn heartbeat(&self, id: &str, worker_id: &str, extend_secs: i64) -> Result<bool, String> {
        let mut jobs = self.jobs.write();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            let now = Utc::now();
            job.worker_id = Some(worker_id.to_string());
            job.heartbeat_at = Some(now.to_rfc3339());
            job.leased_until = Some((now + chrono::Duration::seconds(extend_secs)).to_rfc3339());
            let cancel_req = job.cancel_requested || job.phase == JobPhase::CancelRequested;
            let job_clone = job.clone();
            drop(jobs);
            self.enqueue_persist(job_clone);
            Ok(cancel_req)
        } else {
            Err(format!("Job {} not found", id))
        }
    }

    /// Ask a job to stop.
    ///
    /// Refused for a job that has already finished and for one that is
    /// publishing. Both used to be accepted: a finished job kept a stale
    /// `cancel_requested = true` that killed its next *retry* two seconds in,
    /// and a publishing job reported "cancelling" while the publish -- which
    /// is an atomic rename plus a registry write, and cannot be interrupted --
    /// went ahead anyway (UI-03).
    pub fn request_cancel(&self, id: &str) -> Result<(), CancelError> {
        let mut jobs = self.jobs.write();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return Err(CancelError::NotFound);
        };
        if job.phase.is_terminal() {
            return Err(CancelError::AlreadyFinished);
        }
        if job.phase == JobPhase::Publishing {
            return Err(CancelError::Publishing);
        }
        job.cancel_requested = true;
        if job.phase == JobPhase::Queued {
            let _ = job.transition_to(JobPhase::Cancelled, Some("Cancelled".into()));
            job.error = Some("Cancelled by user".into());
            job.error_category = Some("cancelled".into());
        } else if job.phase != JobPhase::CancelRequested {
            let _ = job.transition_to(JobPhase::CancelRequested, Some("Cancel requested".into()));
        }
        let job_clone = job.clone();
        drop(jobs);
        self.announce(&job_clone);
        self.enqueue_persist(job_clone);
        Ok(())
    }

    /// Put a failed or skipped job back on the queue, atomically.
    ///
    /// The check and the transition happen under one write lock. The handler
    /// used to look the job up, dispatch it, and only then transition it --
    /// and `Queued -> Queued` is a legal no-op -- so a double-clicked Retry
    /// dispatched the same record twice, and a dispatcher that reached
    /// Probing first made the later `Failed -> Queued` illegal, leaving the
    /// job `Pending` for its whole encode (UI-03).
    pub fn requeue_for_retry(
        &self,
        id: &str,
        stage: &str,
    ) -> Result<JobRecord, RetryError> {
        let mut jobs = self.jobs.write();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return Err(RetryError::NotFound);
        };
        if !matches!(job.phase, JobPhase::Failed | JobPhase::Skipped) {
            return Err(RetryError::NotRetryable(job.phase));
        }
        job.transition_to(JobPhase::Queued, Some(stage.to_string()))
            .map_err(|_| RetryError::NotRetryable(job.phase))?;
        job.error = None;
        job.error_category = None;
        job.stderr_log = None;
        job.finished_at = None;
        job.cancel_requested = false;
        job.progress = 0.0;
        job.attempt = job.attempt.saturating_add(1);
        let job_clone = job.clone();
        drop(jobs);
        self.announce(&job_clone);
        self.enqueue_persist(job_clone.clone());
        Ok(job_clone)
    }

    /// Put a job the service stop interrupted back to `Queued` (PL-02).
    ///
    /// Deliberately outside the phase machine: no operator transition leads
    /// from `Encoding` back to `Queued`, but "the run this belonged to ended"
    /// is not an operator transition. What it replaces is the crash-recovery
    /// sweep, which only runs at process start -- so after an in-process
    /// Stop/Start the record sat in `Processing` forever. Queued, the watcher's
    /// next offer of the same file adopts it (`find_pending_by_input_path`).
    pub fn requeue_interrupted(&self, id: &str) {
        let mut jobs = self.jobs.write();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return;
        };
        if job.phase.is_terminal() {
            return;
        }
        job.phase = JobPhase::Queued;
        job.state = JobState::Pending;
        job.current_stage = "Re-queued (service stopped)".into();
        job.progress = 0.0;
        job.worker_id = None;
        job.leased_until = None;
        let job_clone = job.clone();
        drop(jobs);
        self.announce(&job_clone);
        self.enqueue_persist(job_clone);
    }

    /// Undo [`Self::requeue_for_retry`] when the dispatcher refused the job.
    pub fn revert_retry(&self, id: &str, reason: &str) {
        let reason = reason.to_string();
        let _ = self.transition(id, JobPhase::Failed, Some("Failed".into()), |j| {
            j.error = Some(reason);
            j.attempt = j.attempt.saturating_sub(1);
        });
    }

    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<JobRecord> {
        self.jobs.read().iter().find(|j| j.id == id).cloned()
    }

    pub fn all(&self) -> Vec<JobRecord> {
        self.jobs.read().clone()
    }

    /// The five counters `/api/stats` reports, counted under the read lock.
    ///
    /// This used to be `all()` -- a clone of the whole vector, each record
    /// carrying up to 200 lines of `stderr_log` for a failed job -- thrown
    /// away after five `filter().count()` passes. The UI polled it every 2 s
    /// per tab.
    pub fn stats(&self) -> JobStateCounts {
        let jobs = self.jobs.read();
        let mut counts = JobStateCounts {
            total: jobs.len(),
            ..Default::default()
        };
        for job in jobs.iter() {
            match job.state {
                JobState::Pending => counts.pending += 1,
                JobState::Processing => counts.active += 1,
                JobState::Completed => counts.completed += 1,
                JobState::Failed => counts.failed += 1,
                JobState::Cancelled => {}
            }
        }
        counts
    }

    pub fn all_recent(&self) -> Vec<JobRecord> {
        let mut jobs = self.jobs.read().clone();
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        jobs
    }

    pub fn pending(&self) -> Vec<JobRecord> {
        let mut pending: Vec<_> = self
            .jobs
            .read()
            .iter()
            .filter(|j| j.state == JobState::Pending)
            .cloned()
            .collect();
        pending.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        pending
    }

    pub fn active(&self) -> Vec<JobRecord> {
        self.jobs
            .read()
            .iter()
            .filter(|j| j.state == JobState::Processing)
            .cloned()
            .collect()
    }

    pub fn completed(&self) -> Vec<JobRecord> {
        let mut done: Vec<_> = self
            .jobs
            .read()
            .iter()
            .filter(|j| j.state == JobState::Completed)
            .cloned()
            .collect();
        done.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));
        done
    }

    pub fn failed(&self) -> Vec<JobRecord> {
        let mut failed: Vec<_> = self
            .jobs
            .read()
            .iter()
            .filter(|j| j.state == JobState::Failed)
            .cloned()
            .collect();
        failed.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));
        failed
    }

    #[allow(dead_code)]
    pub fn cancelled(&self) -> Vec<JobRecord> {
        let mut cancelled: Vec<_> = self
            .jobs
            .read()
            .iter()
            .filter(|j| j.state == JobState::Cancelled)
            .cloned()
            .collect();
        cancelled.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));
        cancelled
    }

    /// Take one finished job off the queue.
    ///
    /// Only terminal jobs: a completed, failed, skipped or cancelled record is
    /// history, and history is the operator's to clear. A pending or running
    /// one is refused -- see [`DismissError::StillActive`] -- because
    /// dismissing a record whose encoder is still going would leave an ffmpeg
    /// process with nothing tracking it.
    ///
    /// Returns the removed record so the caller can report what went.
    pub fn dismiss(&self, id: &str) -> Result<JobRecord, DismissError> {
        let mut jobs = self.jobs.write();
        let Some(idx) = jobs.iter().position(|j| j.id == id) else {
            return Err(DismissError::NotFound);
        };
        if !jobs[idx].phase.is_terminal() {
            return Err(DismissError::StillActive);
        }
        Ok(jobs.remove(idx))
    }

    /// Take every finished job in `states` off the queue at once.
    ///
    /// The bulk form of [`Self::dismiss`], and it inherits the same rule:
    /// anything still running is left exactly where it is, whatever the caller
    /// asked for.
    pub fn dismiss_all(&self, states: &[JobState]) -> Vec<String> {
        let mut jobs = self.jobs.write();
        let mut removed = Vec::new();
        jobs.retain(|j| {
            let go = j.phase.is_terminal() && states.contains(&j.state);
            if go {
                removed.push(j.id.clone());
            }
            !go
        });
        removed
    }

    pub fn prune_old(&self, max_entries: usize) {
        let mut jobs = self.jobs.write();
        if jobs.len() > max_entries {
            jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            jobs.truncate(max_entries);
        }
    }

    /// Is anyone subscribed to the SSE stream right now?
    ///
    /// Lets a caller skip building a payload nobody will read -- the progress
    /// thread produces one every 250 ms per running encode.
    pub fn has_subscribers(&self) -> bool {
        self.event_tx.receiver_count() > 0
    }

    /// Tell every subscriber what one job looks like now (UI-01).
    ///
    /// Called from each mutation that changes what an operator sees -- push,
    /// transition, cancel request -- so a client no longer has to wait for its
    /// 15 s safety-net poll. Before this only the terminal events went out:
    /// pressing cancel, a job reaching `Cancelled`, a retry going back to
    /// `Queued` and a fresh ingest appearing were all invisible until the next
    /// poll or a page refresh. Emitting here, at the one choke point, rather
    /// than at each call site is what keeps a future transition from being
    /// silent again.
    ///
    /// Additive over the documented `{ id, stage }` payload: `phase`, `state`
    /// and the full `job` record ride along. The progress path goes through
    /// `update_local` and is deliberately not announced here; it has its own
    /// throttled `progress` event.
    fn announce(&self, job: &JobRecord) {
        if !self.has_subscribers() {
            return;
        }
        let payload = serde_json::json!({
            "id": job.id,
            "stage": job.current_stage,
            "phase": job.phase,
            "state": job.state,
            "job": job,
        });
        self.broadcast("job_update", &payload.to_string());
    }

    /// Publish one SSE frame.
    ///
    /// This used to parse the already-serialised payload back into a
    /// `serde_json::Value`, wrap it in an envelope and serialise the whole
    /// thing again, and then every subscriber's stream parsed that envelope
    /// and serialised `data` a third time. The event name and the data body
    /// are both already strings, so the frame now carries them verbatim and
    /// the subscribers share one `Arc` instead of each doing a round trip
    /// through JSON.
    pub fn broadcast(&self, event_type: &str, payload: &str) {
        if !self.has_subscribers() {
            return;
        }
        let _ = self.event_tx.send(Arc::new(SseFrame {
            event: event_type.to_string(),
            data: payload.to_string(),
        }));
    }
}

/// Drains the persist channel, coalescing by job id, and writes each batch in
/// one transaction.
///
/// Coalescing is the whole point: during an encode the same job is updated many
/// times a second, and only the newest snapshot is worth writing. Ordering
/// follows from there -- one writer, one transaction per batch, newest wins --
/// so the row can no longer end up behind the in-memory record.
async fn persister_loop(
    p: Arc<Persistence>,
    mut rx: tokio::sync::mpsc::Receiver<PersistMsg>,
    jobs: Arc<RwLock<Vec<JobRecord>>>,
) {
    use std::collections::HashMap;

    // Keyed by job id: many snapshots of the same job collapse to one write.
    let mut pending: HashMap<String, JobRecord> = HashMap::new();
    let mut ticker = tokio::time::interval(PERSIST_FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(PersistMsg::Record(job)) => {
                    pending.insert(job.id.clone(), *job);
                    if pending.len() >= PERSIST_FLUSH_BATCH {
                        flush(&p, &jobs, &mut pending).await;
                    }
                }
                Some(PersistMsg::Flush(ack)) => {
                    adopt_dirty(&p, &jobs, &mut pending);
                    flush(&p, &jobs, &mut pending).await;
                    let _ = ack.send(());
                }
                // Every sender is gone: the queue itself has been dropped.
                None => {
                    adopt_dirty(&p, &jobs, &mut pending);
                    flush(&p, &jobs, &mut pending).await;
                    return;
                }
            },
            _ = ticker.tick() => {
                adopt_dirty(&p, &jobs, &mut pending);
                flush(&p, &jobs, &mut pending).await;
            }
        }
    }
}

/// Pull snapshots for jobs whose write was dropped by a full channel.
fn adopt_dirty(
    p: &Persistence,
    jobs: &RwLock<Vec<JobRecord>>,
    pending: &mut std::collections::HashMap<String, JobRecord>,
) {
    let ids = {
        let mut dirty = p.dirty.lock();
        if dirty.is_empty() {
            return;
        }
        std::mem::take(&mut *dirty)
    };
    let snapshot = jobs.read();
    for id in ids {
        // The in-memory record is by definition newer than whatever was
        // dropped, so it overwrites any queued snapshot for the same id.
        if let Some(job) = snapshot.iter().find(|j| j.id == id) {
            pending.insert(id, job.clone());
        }
    }
}

/// Write the coalesced batch.
///
/// Each id is re-read from memory first. That is what makes ordering safe: a
/// snapshot sitting in the channel may already be stale relative to the
/// in-memory record -- which is exactly how the database used to end up
/// claiming "Encoding 97%" for a completed job (F-13). The queued snapshot is
/// only a fallback for a job that has since been pruned out of memory.
async fn flush(
    p: &Persistence,
    jobs: &RwLock<Vec<JobRecord>>,
    pending: &mut std::collections::HashMap<String, JobRecord>,
) {
    if pending.is_empty() {
        return;
    }
    let batch: Vec<JobRecord> = {
        let live = jobs.read();
        pending
            .iter()
            .map(|(id, queued)| {
                live.iter()
                    .find(|j| j.id == *id)
                    .cloned()
                    .unwrap_or_else(|| queued.clone())
            })
            .collect()
    };
    match crate::db::persist_jobs(&p.pool, &batch).await {
        Ok(()) => pending.clear(),
        Err(e) => {
            // Keep the records queued rather than losing them; the next tick
            // retries. Coalescing means a persistent failure costs bounded
            // memory, not an unbounded backlog.
            tracing::error!("Failed to persist {} job record(s): {}", batch.len(), e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SB-03: `stats()` counts under the read lock. It must agree exactly with
    /// the clone-and-filter it replaced, including that `Cancelled` counts
    /// towards `total` but towards none of the four buckets.
    #[test]
    fn stats_agree_with_counting_a_full_clone() {
        let (tx, _rx) = broadcast::channel(16);
        let queue = JobQueue::new(tx, None);

        let states = [
            JobState::Pending,
            JobState::Pending,
            JobState::Processing,
            JobState::Completed,
            JobState::Completed,
            JobState::Completed,
            JobState::Failed,
            JobState::Cancelled,
        ];
        for (i, state) in states.iter().enumerate() {
            let mut job = JobRecord::new(&format!("D:/media/clip{}.mov", i), "ProfileA");
            job.state = *state;
            queue.push(job);
        }

        let all = queue.all();
        let counts = queue.stats();
        assert_eq!(
            counts.pending,
            all.iter().filter(|j| j.state == JobState::Pending).count()
        );
        assert_eq!(
            counts.active,
            all.iter()
                .filter(|j| j.state == JobState::Processing)
                .count()
        );
        assert_eq!(
            counts.completed,
            all.iter()
                .filter(|j| j.state == JobState::Completed)
                .count()
        );
        assert_eq!(
            counts.failed,
            all.iter().filter(|j| j.state == JobState::Failed).count()
        );
        assert_eq!(counts.total, all.len());
        assert_eq!(counts.total, 8);
        assert_eq!(counts.pending + counts.active + counts.completed + counts.failed, 7);
    }

    async fn pool_in_temp(tag: &str) -> (Arc<SqlitePool>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pt-jobs-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pool = crate::db::init_pool(&dir.join("jobs.db"))
            .await
            .expect("init pool");
        (Arc::new(pool), dir)
    }

    #[tokio::test]
    async fn a_burst_of_updates_coalesces_to_one_row_with_the_final_state() {
        let (pool, dir) = pool_in_temp("coalesce").await;
        let (event_tx, _rx) = broadcast::channel::<std::sync::Arc<crate::jobs::SseFrame>>(16);
        let queue = JobQueue::new(event_tx, Some(pool.clone()));
        let persister = queue.spawn_persister().expect("persister started");

        let mut job = JobRecord::new("D:/media/burst.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        let id = job.id.clone();
        queue.push(job);

        // The write amplification this step is about: one row per progress line.
        for i in 0..1_000 {
            queue.update(&id, |j| {
                j.progress = i as f32 / 10.0;
                j.current_frame = i;
            });
        }
        queue.update(&id, |j| {
            j.progress = 100.0;
            j.current_frame = 1_000;
            j.current_stage = "Finalizing".into();
        });

        queue.flush_persister().await;

        let rows = crate::db::load_all_durable_jobs(&pool).await.unwrap();
        assert_eq!(rows.len(), 1, "a burst produced more than one row");
        assert_eq!(rows[0].id, id);
        // The whole point: the row reflects the newest state, not whichever
        // write happened to land last.
        assert_eq!(rows[0].progress, 100.0);
        assert_eq!(rows[0].current_frame, 1_000);
        assert_eq!(rows[0].current_stage, "Finalizing");

        persister.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_full_channel_still_converges_on_the_latest_state() {
        let (pool, dir) = pool_in_temp("backpressure").await;
        let (event_tx, _rx) = broadcast::channel::<std::sync::Arc<crate::jobs::SseFrame>>(16);
        let queue = JobQueue::new(event_tx, Some(pool.clone()));

        let mut job = JobRecord::new("D:/media/flood.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        let id = job.id.clone();
        queue.push(job);

        // No persister yet, so the channel fills and every further snapshot is
        // dropped in favour of the dirty flag.
        for i in 0..(PERSIST_CHANNEL_CAPACITY * 2) {
            queue.update(&id, |j| j.progress = i as f32);
        }
        queue.update(&id, |j| {
            j.progress = 42.0;
            j.current_stage = "Latest".into();
        });

        let persister = queue.spawn_persister().expect("persister started");
        queue.flush_persister().await;

        let rows = crate::db::load_all_durable_jobs(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].current_stage, "Latest",
            "the dirty flag did not recover the dropped snapshot"
        );
        assert_eq!(rows[0].progress, 42.0);

        persister.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn update_local_does_not_write_until_persist_now() {
        let (pool, dir) = pool_in_temp("local").await;
        let (event_tx, _rx) = broadcast::channel::<std::sync::Arc<crate::jobs::SseFrame>>(16);
        let queue = JobQueue::new(event_tx, Some(pool.clone()));
        let persister = queue.spawn_persister().expect("persister started");

        let mut job = JobRecord::new("D:/media/local.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        let id = job.id.clone();
        queue.push(job);
        queue.flush_persister().await;

        queue.update_local(&id, |j| j.current_stage = "Encoding 50%".into());
        queue.flush_persister().await;
        let rows = crate::db::load_all_durable_jobs(&pool).await.unwrap();
        assert_ne!(
            rows[0].current_stage, "Encoding 50%",
            "update_local wrote to the database"
        );
        // ...but the in-memory record, which the API serves, is current.
        assert_eq!(queue.get(&id).unwrap().current_stage, "Encoding 50%");

        queue.persist_now(&id);
        queue.flush_persister().await;
        let rows = crate::db::load_all_durable_jobs(&pool).await.unwrap();
        assert_eq!(rows[0].current_stage, "Encoding 50%");

        persister.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_pending_by_input_path_returns_the_oldest_match() {
        let (event_tx, _rx) = broadcast::channel::<std::sync::Arc<crate::jobs::SseFrame>>(16);
        let queue = JobQueue::new_in_memory(event_tx);

        let mut old = JobRecord::new("D:/media/same.mov", "ProfileA");
        old.created_at = "2026-01-01T00:00:00Z".into();
        let mut newer = JobRecord::new("D:/media/same.mov", "ProfileA");
        newer.created_at = "2026-06-01T00:00:00Z".into();
        let mut other = JobRecord::new("D:/media/different.mov", "ProfileA");
        other.created_at = "2025-01-01T00:00:00Z".into();
        let old_id = old.id.clone();
        queue.push(newer);
        queue.push(old);
        queue.push(other);

        let found = queue.find_pending_by_input_path("D:/media/same.mov");
        assert_eq!(found.map(|j| j.id), Some(old_id));

        // A job that is no longer pending is not adopted.
        let mut done = JobRecord::new("D:/media/done.mov", "ProfileA");
        done.state = JobState::Completed;
        queue.push(done);
        assert!(queue
            .find_pending_by_input_path("D:/media/done.mov")
            .is_none());
        assert!(queue
            .find_pending_by_input_path("D:/media/missing.mov")
            .is_none());
    }

    #[test]
    fn test_valid_forward_phase_transitions() {
        let mut job = JobRecord::new("input.mov", "ProfileA");
        assert_eq!(job.phase, JobPhase::Queued);
        assert_eq!(job.state, JobState::Pending);
        assert!(job.started_at.is_none());

        assert!(job
            .transition_to(JobPhase::Probing, Some("Probing media".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Probing);
        assert_eq!(job.state, JobState::Processing);
        assert!(job.started_at.is_some());

        assert!(job
            .transition_to(JobPhase::Planned, Some("Profile resolved".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Planned);

        assert!(job
            .transition_to(JobPhase::Encoding, Some("Encoding".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Encoding);

        assert!(job
            .transition_to(JobPhase::Validating, Some("Validating".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Validating);

        assert!(job
            .transition_to(JobPhase::Publishing, Some("Publishing".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Publishing);

        assert!(job
            .transition_to(JobPhase::Completed, Some("Completed".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Completed);
        assert_eq!(job.state, JobState::Completed);
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn test_illegal_phase_transitions_rejected() {
        let mut job = JobRecord::new("input.mov", "ProfileA");

        // Cannot jump directly from Queued to Publishing or Completed
        assert!(job.transition_to(JobPhase::Publishing, None).is_err());
        assert!(job.transition_to(JobPhase::Completed, None).is_err());

        // Probing -> Completed is illegal (must go through planned/encoding/validating/publishing)
        assert!(job.transition_to(JobPhase::Probing, None).is_ok());
        assert!(job.transition_to(JobPhase::Completed, None).is_err());

        // Completed is terminal (cannot transition further)
        job.phase = JobPhase::Completed;
        assert!(job.transition_to(JobPhase::Encoding, None).is_err());
        assert!(job.transition_to(JobPhase::Queued, None).is_err());
    }

    #[test]
    fn test_retryable_flow() {
        let mut job = JobRecord::new("input.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        job.transition_to(JobPhase::Planned, None).unwrap();
        job.transition_to(JobPhase::Encoding, None).unwrap();

        // Encoding failure -> Recoverable
        assert!(job
            .transition_to(JobPhase::Recoverable, Some("Retrying in 2000ms".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Recoverable);
        assert_eq!(job.state, JobState::Processing);

        // Recoverable -> Encoding (attempt 2)
        assert!(job
            .transition_to(JobPhase::Encoding, Some("Encoding attempt 2".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Encoding);
    }

    #[test]
    fn test_cancellation_flow() {
        let mut job = JobRecord::new("input.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        job.transition_to(JobPhase::Planned, None).unwrap();
        job.transition_to(JobPhase::Encoding, None).unwrap();

        // User requests cancellation
        assert!(job
            .transition_to(JobPhase::CancelRequested, Some("Cancelling".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::CancelRequested);
        assert_eq!(job.state, JobState::Processing);

        // Process stops and enters Cancelled
        assert!(job
            .transition_to(JobPhase::Cancelled, Some("Cancelled".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Cancelled);
        assert_eq!(job.state, JobState::Cancelled);
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn test_failed_retry_allows_requeue() {
        let mut job = JobRecord::new("input.mov", "ProfileA");
        job.transition_to(JobPhase::Probing, None).unwrap();
        job.transition_to(JobPhase::Failed, Some("Probe failed".into()))
            .unwrap();
        assert_eq!(job.phase, JobPhase::Failed);
        assert_eq!(job.state, JobState::Failed);

        // Manual retry from Failed returns to Queued
        assert!(job
            .transition_to(JobPhase::Queued, Some("Queued".into()))
            .is_ok());
        assert_eq!(job.phase, JobPhase::Queued);
        assert_eq!(job.state, JobState::Pending);
    }

    #[test]
    fn test_job_record_json_serialization_compatibility() {
        let mut job = JobRecord::new("D:/media/in.mp4", "ProfileA");
        job.uuid = Some("test-uuid-123".into());
        job.transition_to(JobPhase::Probing, None).unwrap();
        job.transition_to(JobPhase::Planned, None).unwrap();
        job.transition_to(JobPhase::Encoding, None).unwrap();
        job.transition_to(JobPhase::Validating, None).unwrap();
        job.transition_to(JobPhase::Publishing, None).unwrap();
        job.transition_to(JobPhase::Completed, None).unwrap();
        job.progress = 100.0;

        let json = serde_json::to_value(&job).unwrap();
        assert_eq!(json["state"], "Completed");
        assert_eq!(json["phase"], "completed");
        assert_eq!(json["current_stage"], "Completed");
        assert_eq!(json["progress"], 100.0);
        assert_eq!(json["input_path"], "D:/media/in.mp4");
        assert_eq!(json["uuid"], "test-uuid-123");
    }

    fn queue() -> JobQueue {
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        JobQueue::new_in_memory(tx)
    }

    fn terminal_job(input: &str, phase: JobPhase) -> JobRecord {
        let mut j = JobRecord::new(input, "ProfileA");
        // Walk the phase machine rather than assigning, so these fixtures stay
        // honest if the allowed transitions change.
        let _ = j.transition_to(JobPhase::Probing, None);
        match phase {
            JobPhase::Failed => {
                let _ = j.transition_to(JobPhase::Failed, None);
            }
            JobPhase::Skipped => {
                let _ = j.transition_to(JobPhase::Skipped, None);
            }
            _ => {
                let _ = j.transition_to(JobPhase::Planned, None);
                let _ = j.transition_to(JobPhase::Encoding, None);
                let _ = j.transition_to(JobPhase::Validating, None);
                let _ = j.transition_to(JobPhase::Publishing, None);
                let _ = j.transition_to(JobPhase::Completed, None);
            }
        }
        assert!(j.phase.is_terminal(), "fixture must be terminal");
        j
    }

    /// The × button. A finished record is history and history is the
    /// operator's to clear.
    #[test]
    fn a_finished_job_can_be_dismissed() {
        let q = queue();
        let job = terminal_job("a.mxf", JobPhase::Failed);
        let id = job.id.clone();
        q.push(job);
        assert_eq!(q.all().len(), 1);

        let removed = q.dismiss(&id).expect("a failed job must be dismissable");
        assert_eq!(removed.id, id);
        assert!(q.all().is_empty());
        assert_eq!(q.dismiss(&id).unwrap_err(), DismissError::NotFound);
    }

    /// Dismissing a running job would leave an ffmpeg process with nothing
    /// tracking it -- no progress, no cancel, no way to find the PID.
    #[test]
    fn a_running_job_cannot_be_dismissed() {
        let q = queue();
        let mut job = JobRecord::new("live.mxf", "ProfileA");
        let _ = job.transition_to(JobPhase::Probing, None);
        let _ = job.transition_to(JobPhase::Planned, None);
        let _ = job.transition_to(JobPhase::Encoding, None);
        assert_eq!(job.phase, JobPhase::Encoding, "fixture must be running");
        let id = job.id.clone();
        q.push(job);

        assert_eq!(q.dismiss(&id).unwrap_err(), DismissError::StillActive);
        assert_eq!(q.all().len(), 1, "and it stays on the queue");
    }

    /// "Clear all failed" clears the failed ones and nothing else.
    #[test]
    fn a_bulk_dismiss_spares_everything_it_was_not_asked_for() {
        let q = queue();
        for _ in 0..3 {
            q.push(terminal_job("bad.mxf", JobPhase::Failed));
        }
        q.push(terminal_job("good.mxf", JobPhase::Completed));
        let mut running = JobRecord::new("live.mxf", "ProfileA");
        let _ = running.transition_to(JobPhase::Probing, None);
        let _ = running.transition_to(JobPhase::Planned, None);
        let _ = running.transition_to(JobPhase::Encoding, None);
        assert_eq!(running.phase, JobPhase::Encoding, "fixture must be running");
        q.push(running);

        let removed = q.dismiss_all(&[JobState::Failed]);
        assert_eq!(removed.len(), 3);

        let left = q.all();
        assert_eq!(left.len(), 2, "the completed one and the running one stay");
        assert!(left.iter().any(|j| j.state == JobState::Completed));
        assert!(left.iter().any(|j| j.phase == JobPhase::Encoding));
    }

    /// A bulk dismiss must never take a running job, even when its state was
    /// asked for -- the terminal check is not the caller's to waive.
    #[test]
    fn a_bulk_dismiss_never_takes_a_running_job() {
        let q = queue();
        let mut running = JobRecord::new("live.mxf", "ProfileA");
        let _ = running.transition_to(JobPhase::Probing, None);
        let _ = running.transition_to(JobPhase::Planned, None);
        let _ = running.transition_to(JobPhase::Encoding, None);
        assert_eq!(running.phase, JobPhase::Encoding, "fixture must be running");
        let state = running.state;
        q.push(running);

        assert!(q.dismiss_all(&[state]).is_empty());
        assert_eq!(q.all().len(), 1);
    }

    fn job_in(phases: &[JobPhase]) -> JobRecord {
        let mut j = JobRecord::new("clip.mxf", "ProfileA");
        for p in phases {
            j.transition_to(*p, None).unwrap();
        }
        j
    }

    const RUNNING: &[JobPhase] = &[JobPhase::Probing, JobPhase::Planned, JobPhase::Encoding];

    /// UI-03. A finished job used to accept a cancel and keep
    /// `cancel_requested = true`, which then killed its next retry.
    #[test]
    fn a_finished_job_refuses_a_cancel() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.push(JobPhase::Failed);
        let job = job_in(&phases);
        let id = job.id.clone();
        q.push(job);

        assert_eq!(q.request_cancel(&id), Err(CancelError::AlreadyFinished));
        assert!(!q.get(&id).unwrap().cancel_requested);
    }

    /// UI-03. The publish cannot be interrupted, so a cancel must not claim it.
    #[test]
    fn a_publishing_job_refuses_a_cancel() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.extend([JobPhase::Validating, JobPhase::Publishing]);
        let job = job_in(&phases);
        let id = job.id.clone();
        q.push(job);

        assert_eq!(q.request_cancel(&id), Err(CancelError::Publishing));
        assert_eq!(q.get(&id).unwrap().phase, JobPhase::Publishing);
    }

    /// UI-03. A cancel during validation must make the Publishing transition
    /// illegal -- that is what lets the processor abandon the publish.
    #[test]
    fn a_cancel_during_validation_blocks_the_publish() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.push(JobPhase::Validating);
        let job = job_in(&phases);
        let id = job.id.clone();
        q.push(job);

        q.request_cancel(&id).unwrap();
        assert!(q
            .transition(&id, JobPhase::Publishing, None, |_| {})
            .is_err());
        q.transition(&id, JobPhase::Cancelled, None, |_| {}).unwrap();
        assert_eq!(q.get(&id).unwrap().state, JobState::Cancelled);
    }

    #[test]
    fn a_queued_job_is_cancelled_outright() {
        let q = queue();
        let job = JobRecord::new("queued.mxf", "ProfileA");
        let id = job.id.clone();
        q.push(job);

        q.request_cancel(&id).unwrap();
        let live = q.get(&id).unwrap();
        assert_eq!(live.phase, JobPhase::Cancelled);
        assert!(live.finished_at.is_some());
    }

    /// UI-03. `Queued -> Queued` is a legal no-op, so the old handler let a
    /// double-clicked Retry dispatch the same record twice.
    #[test]
    fn a_second_retry_of_the_same_job_is_refused() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.push(JobPhase::Failed);
        let mut job = job_in(&phases);
        job.cancel_requested = true;
        let id = job.id.clone();
        q.push(job);

        let requeued = q.requeue_for_retry(&id, "retry").unwrap();
        assert_eq!(requeued.phase, JobPhase::Queued);
        assert!(!requeued.cancel_requested, "a stale cancel must not kill the retry");
        assert_eq!(
            q.requeue_for_retry(&id, "retry").unwrap_err(),
            RetryError::NotRetryable(JobPhase::Queued)
        );
    }

    #[test]
    fn a_refused_dispatch_puts_the_job_back_as_failed() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.push(JobPhase::Failed);
        let job = job_in(&phases);
        let attempt = job.attempt;
        let id = job.id.clone();
        q.push(job);

        q.requeue_for_retry(&id, "retry").unwrap();
        q.revert_retry(&id, "Service is not running");
        let live = q.get(&id).unwrap();
        assert_eq!(live.phase, JobPhase::Failed);
        assert_eq!(live.attempt, attempt);
        assert_eq!(live.error.as_deref(), Some("Service is not running"));
    }

    /// UI-01. Every transition reaches SSE subscribers with the full record.
    #[test]
    fn a_transition_is_announced_with_the_record() {
        let (tx, mut rx) = broadcast::channel(16);
        let q = JobQueue::new_in_memory(tx);
        let job = JobRecord::new("clip.mxf", "ProfileA");
        let id = job.id.clone();
        q.push(job);
        q.transition(&id, JobPhase::Probing, None, |_| {}).unwrap();

        let pushed = rx.try_recv().unwrap();
        assert_eq!(pushed.event, "job_update");
        let probing = rx.try_recv().unwrap();
        let data: serde_json::Value = serde_json::from_str(&probing.data).unwrap();
        assert_eq!(data["phase"], "probing");
        assert_eq!(data["job"]["id"], id.as_str());
    }

    /// PL-02. A job interrupted by a service stop goes back to Queued so the
    /// next start's watcher offer adopts it, instead of staying `Processing`
    /// forever (crash recovery only runs at process start).
    #[test]
    fn an_interrupted_job_is_requeued_and_adoptable() {
        let q = queue();
        let job = job_in(RUNNING);
        let id = job.id.clone();
        let input = job.input_path.clone();
        q.push(job);

        q.requeue_interrupted(&id);
        let live = q.get(&id).unwrap();
        assert_eq!(live.phase, JobPhase::Queued);
        assert_eq!(live.state, JobState::Pending);
        assert_eq!(q.find_pending_by_input_path(&input).unwrap().id, id);
    }

    #[test]
    fn a_finished_job_is_not_requeued_by_an_interruption() {
        let q = queue();
        let mut phases = RUNNING.to_vec();
        phases.push(JobPhase::Failed);
        let job = job_in(&phases);
        let id = job.id.clone();
        q.push(job);

        q.requeue_interrupted(&id);
        assert_eq!(q.get(&id).unwrap().phase, JobPhase::Failed);
    }

}
