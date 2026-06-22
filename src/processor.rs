use crate::{bootstrap, config, encoder, identity, jobs, probe, profiles};
use chrono::Utc;
use std::path::Path;

pub fn process_file_sync(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
) {
    let watch_root = std::path::Path::new(&config.paths.watch_folder);
    let canonical_input = input_path.canonicalize().unwrap_or_else(|_| input_path.to_path_buf());
    let canonical_watch = watch_root.canonicalize().unwrap_or_else(|_| watch_root.to_path_buf());
    if !canonical_input.starts_with(&canonical_watch) {
        tracing::warn!("Rejected path traversal attempt: {}", input_path.display());
        return;
    }

    let mut job = jobs::JobRecord::new(&input_path.to_string_lossy(), "pending");
    job.state = jobs::JobState::Processing;
    job.current_stage = "Probing".to_string();
    queue.push(job.clone());
    queue.broadcast("job_update", &serde_json::json!({"id": job.id, "stage": "Probing"}).to_string());

    let probe_data = match probe::probe_media(tools, input_path) {
        Ok(p) => p,
        Err(e) => {
            queue.update(&job.id, |j| {
                j.state = jobs::JobState::Failed;
                j.error = Some(format!("Probe: {}", e));
                j.current_stage = "Failed".into();
                j.finished_at = Some(Utc::now().to_rfc3339());
            });
            tracing::error!("Probe failed for {}: {}", input_path.display(), e);
            return;
        }
    };

    let profile_id = probe_data.profile_id();
    let profile_name = profile_id.to_string();
    let profile = profiles::EncodingProfile::by_id(profile_id);
    if !profile.config_for(config).enabled {
        queue.update(&job.id, |j| {
            j.state = jobs::JobState::Failed;
            j.error = Some("Profile disabled".into());
        });
        return;
    }

    queue.update(&job.id, |j| {
        j.profile = profile_name.clone();
        j.current_stage = format!("Encoding {}", profile_name);
        j.source_frame_count = probe_data.frame_count;
        j.duration_secs = probe_data.duration_secs;
    });

    let output_filename = identity::output_filename(&input_path.to_string_lossy(), &profile_name);
    let video_dir = target_root.join("videos");
    let _ = std::fs::create_dir_all(&video_dir);
    let output_path = video_dir.join(&output_filename);

    let metadata_uuid = identity::generate_uuid();

    let (ptx, prx) = std::sync::mpsc::channel::<encoder::EncodeProgress>();
    let jid = job.id.clone();
    let qc = queue.clone();
    std::thread::spawn(move || {
        while let Ok(p) = prx.recv() {
            let pct = if p.total_frames > 0 { p.percent } else { 50.0 };
            qc.update(&jid, |j| {
                j.progress = pct;
                j.current_frame = p.frame;
                j.encode_fps = p.fps;
                j.encode_bitrate = p.bitrate.clone();
                j.encode_speed = p.speed.clone();
                j.current_stage = if pct >= 100.0 { "Finalizing".into() } else { format!("Encoding {:.0}%", pct) };
            });
        }
    });

    let result = encoder::transcode_file(tools, config, input_path, &probe_data, profile_id, &output_path, &metadata_uuid, ptx);

    if result.success {
        let output_probe = probe::probe_media(tools, &result.output_path)
            .unwrap_or_else(|_| probe_data.clone());
        let _ = identity::write_sidecar_next_to_video(&result.output_path, &metadata_uuid, &probe_data, &output_probe, &profile_name, "h264", &config.encoding.audio_codec);
        queue.update(&job.id, |j| {
            j.state = jobs::JobState::Completed;
            j.uuid = Some(metadata_uuid.clone());
            j.output_path = Some(result.output_path.to_string_lossy().into_owned());
            j.progress = 100.0;
            j.current_stage = "Completed".into();
            j.finished_at = Some(Utc::now().to_rfc3339());
        });
        queue.broadcast("job_completed", &serde_json::json!({"id":job.id,"uuid":metadata_uuid}).to_string());
        tracing::info!("Completed: {} -> {} (uuid={})", input_path.display(), result.output_path.display(), metadata_uuid);
        if config.ingestion.clean_source_after_success {
            let _ = std::fs::remove_file(input_path);
        }
    } else {
        let msg = result.error.unwrap_or_default();
        queue.update(&job.id, |j| {
            j.state = jobs::JobState::Failed;
            j.error = Some(msg.clone());
            j.current_stage = "Failed".into();
            j.finished_at = Some(Utc::now().to_rfc3339());
        });
        tracing::error!("Failed: {} - {}", input_path.display(), msg);
        let _ = std::fs::remove_file(&result.output_path);
    }
    queue.prune_old(500);
}
