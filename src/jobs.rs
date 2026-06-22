use chrono::Utc;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum JobState {
    Pending,
    Processing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRecord {
    pub id: String,
    pub input_path: String,
    pub output_path: Option<String>,
    pub profile: String,
    pub uuid: Option<String>,
    pub state: JobState,
    pub progress: f32,
    pub current_stage: String,
    pub duration_secs: f64,
    pub error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub source_frame_count: i64,
    pub current_frame: i64,
    pub encode_fps: f64,
    pub encode_bitrate: String,
    pub encode_speed: String,
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
            progress: 0.0,
            current_stage: "Queued".to_string(),
            duration_secs: 0.0,
            error: None,
            created_at: Utc::now().to_rfc3339(),
            finished_at: None,
            source_frame_count: 0,
            current_frame: 0,
            encode_fps: 0.0,
            encode_bitrate: String::new(),
            encode_speed: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct JobQueue {
    jobs: Arc<RwLock<Vec<JobRecord>>>,
    event_tx: broadcast::Sender<String>,
}

impl JobQueue {
    pub fn new(event_tx: broadcast::Sender<String>) -> Self {
        Self {
            jobs: Arc::new(RwLock::new(Vec::new())),
            event_tx,
        }
    }

    pub fn event_sender(&self) -> broadcast::Sender<String> {
        self.event_tx.clone()
    }

    pub fn push(&self, job: JobRecord) {
        self.jobs.write().push(job);
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut JobRecord)) {
        let mut jobs = self.jobs.write();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            f(job);
        }
    }

    pub fn all(&self) -> Vec<JobRecord> {
        self.jobs.read().clone()
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

    pub fn prune_old(&self, max_entries: usize) {
        let mut jobs = self.jobs.write();
        if jobs.len() > max_entries {
            jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            jobs.truncate(max_entries);
        }
    }

    pub fn broadcast(&self, event_type: &str, payload: &str) {
        let msg = format!("event: {}\ndata: {}\n\n", event_type, payload);
        let _ = self.event_tx.send(msg);
    }
}
