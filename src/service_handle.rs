use crate::bootstrap::ToolPaths;
use crate::config::AppConfig;
use crate::jobs::JobQueue;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum ServiceCmd {
    Stop,
}

#[derive(Clone)]
pub struct ServiceHandle {
    pub running: Arc<Mutex<bool>>,
    pub cmd_tx: Arc<Mutex<Option<mpsc::Sender<ServiceCmd>>>>,
    pub download_status: Arc<Mutex<Option<String>>>,
    pub log_lines: Arc<Mutex<Vec<String>>>,
}

impl ServiceHandle {
    pub fn new() -> Self {
        Self {
            running: Arc::new(Mutex::new(false)),
            cmd_tx: Arc::new(Mutex::new(None)),
            download_status: Arc::new(Mutex::new(None)),
            log_lines: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn add_log(&self, level: &str, msg: &str) {
        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
        let mut logs = self.log_lines.lock();
        logs.push(format!("{} [{}] {}", ts, level.to_uppercase(), msg));
        while logs.len() > 500 {
            logs.remove(0);
        }
    }

    pub fn get_logs(&self) -> Vec<String> {
        self.log_lines.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        *self.running.lock()
    }
}

pub fn start_processing_loop(
    handle: &ServiceHandle,
    config: &AppConfig,
    job_queue: &JobQueue,
    tools: &ToolPaths,
) -> Result<(), String> {
    if *handle.running.lock() {
        return Err("Service already running".into());
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<ServiceCmd>(1);
    *handle.cmd_tx.lock() = Some(cmd_tx);
    *handle.running.lock() = true;

    let running = handle.running.clone();
    let watch = PathBuf::from(&config.paths.watch_folder);
    let target = PathBuf::from(&config.paths.target_folder);
    let tools = tools.clone();
    let jobs = job_queue.clone();
    let cfg = config.clone();

    handle.add_log("info", "Transcoding service started");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let _ = std::fs::create_dir_all(&target);

            let (file_tx, mut file_rx) = mpsc::channel::<PathBuf>(256);
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

            let mut cmd_rx = cmd_rx;
            loop {
                tokio::select! {
                    Some(path) = file_rx.recv() => {
                        let t = tools.clone();
                        let c = cfg.clone();
                        let jq = jobs.clone();
                        let tg = target.clone();
                        let s = sem.clone();
                        let _permit = s.acquire_owned().await;
                        tokio::task::spawn_blocking(move || {
                            crate::processor::process_file_sync(&jq, &t, &tg, &path, &c);
                        });
                    }
                    cmd = cmd_rx.recv() => {
                        if let Some(ServiceCmd::Stop) = cmd {
                            break;
                        }
                    }
                }
            }
        });

        *running.lock() = false;
    });

    Ok(())
}

pub fn stop_processing(handle: &ServiceHandle) {
    if let Some(ref tx) = *handle.cmd_tx.lock() {
        let _ = tx.try_send(ServiceCmd::Stop);
    }
    handle.add_log("info", "Service stop requested");
}

pub fn trigger_download(handle: &ServiceHandle) -> bool {
    let mut status = handle.download_status.lock();
    if status.is_some() {
        return false;
    }
    *status = Some("downloading".to_string());
    drop(status);
    handle.add_log("info", "Starting FFmpeg download (full build)...");

    let h = handle.clone();
    std::thread::spawn(move || {
        match crate::bootstrap::download_ffmpeg() {
            Ok(_) => {
                *h.download_status.lock() = Some("ok".to_string());
            }
            Err(e) => {
                *h.download_status.lock() = Some(format!("error: {}", e));
            }
        }
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
