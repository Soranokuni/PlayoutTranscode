use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
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
}

impl JobPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobPhase::Completed | JobPhase::Cancelled | JobPhase::Failed
        )
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
            JobPhase::Completed => JobState::Completed,
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
            ),
            JobPhase::Probing => matches!(
                next,
                JobPhase::Planned
                    | JobPhase::NormalizingAudio
                    | JobPhase::Failed
                    | JobPhase::CancelRequested
                    | JobPhase::Cancelled
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
            JobPhase::Completed | JobPhase::Cancelled => false,
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
    /// One-line, human-readable error summary shown in the UI tile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Categorized error code/type (e.g. validation_error, io_lock, ffmpeg_error)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
    /// Verbose multi-line diagnostic log (e.g., last ffmpeg stderr lines). Rendered inside a
    /// collapsible `<details>` element so it never floods the dashboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<Vec<String>>,
    /// How many times this job has been retried (incremented on each retry).
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
            Ok(())
        } else {
            Err(format!("Job {} not found", id))
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<JobRecord> {
        self.jobs.read().iter().find(|j| j.id == id).cloned()
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

    pub fn prune_old(&self, max_entries: usize) {
        let mut jobs = self.jobs.write();
        if jobs.len() > max_entries {
            jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            jobs.truncate(max_entries);
        }
    }

    pub fn broadcast(&self, event_type: &str, payload: &str) {
        let envelope = serde_json::json!({"event": event_type, "data": serde_json::from_str::<serde_json::Value>(payload).unwrap_or(serde_json::Value::String(payload.to_string()))}).to_string();
        let _ = self.event_tx.send(envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
