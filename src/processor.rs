use crate::{bootstrap, config, db, encoder, fingerprint, identity, jobs, probe, profiles};
use chrono::Utc;
use sqlx::SqlitePool;
use std::path::Path;
use uuid::Uuid;

pub fn process_file_sync(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
) {
    let watch_root = std::path::Path::new(&config.paths.watch_folder);
    let canonical_input = input_path.canonicalize().unwrap_or_else(|_| input_path.to_path_buf());
    let canonical_watch = watch_root.canonicalize().unwrap_or_else(|_| watch_root.to_path_buf());
    if !canonical_input.starts_with(&canonical_watch) {
        tracing::warn!("Rejected path traversal attempt: {}", input_path.display());
        return;
    }

    let handle = tokio::runtime::Handle::current();

    let fingerprint = match fingerprint::compute_fnv1a64(input_path) {
        Ok(fp) => fp,
        Err(e) => {
            tracing::error!("Fingerprint failed for {}: {}", input_path.display(), e);
            return;
        }
    };

    if let Ok(Some(existing)) = handle.block_on(db::find_by_fingerprint(pool, fingerprint)) {
        let _ = handle.block_on(db::update_path_by_fingerprint(
            pool,
            fingerprint,
            &input_path.to_string_lossy(),
        ));
        tracing::info!(
            "Dedup: asset {} already exists (fingerprint={}), updating path",
            existing.uuid,
            fingerprint,
        );
        return;
    }

    let metadata_uuid = Uuid::new_v4().to_string();
    let short_uuid = &metadata_uuid[..8];
    let video_dir = target_root.join("videos");
    let _ = std::fs::create_dir_all(&video_dir);
    let safe_stem = identity::sanitize_filename(
        &input_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy(),
    );
    let output_filename = if safe_stem.is_empty() {
        format!("{}.mp4", short_uuid)
    } else {
        format!("{}_{}.mp4", safe_stem, short_uuid)
    };
    let output_path = video_dir.join(&output_filename);

    if let Err(e) = handle.block_on(db::insert_processing(
        pool,
        &metadata_uuid,
        fingerprint,
        &input_path.to_string_lossy(),
        &safe_stem,
    )) {
        tracing::error!("DB insert processing failed for {}: {}", input_path.display(), e);
    }

    let mut job = jobs::JobRecord::new(&input_path.to_string_lossy(), "pending");
    job.state = jobs::JobState::Processing;
    job.current_stage = "Probing".to_string();
    job.uuid = Some(metadata_uuid.clone());
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
            queue.broadcast("failed", &serde_json::json!({"id": job.id, "error": format!("Probe: {}", e)}).to_string());
            let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
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
        queue.broadcast("failed", &serde_json::json!({"id": job.id, "error": "Profile disabled"}).to_string());
        let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
        return;
    }

    queue.update(&job.id, |j| {
        j.profile = profile_name.clone();
        j.current_stage = format!("Encoding {}", profile_name);
        j.source_frame_count = probe_data.frame_count;
        j.duration_secs = probe_data.duration_secs;
        j.duration_ms = (probe_data.duration_secs * 1000.0).round() as i64;
    });

    let (ptx, prx) = std::sync::mpsc::channel::<encoder::EncodeProgress>();
    let jid = job.id.clone();
    let qc = queue.clone();
    std::thread::spawn(move || {
        let mut last_broadcast = std::time::Instant::now();
        const THROTTLE_MS: u64 = 250;
        while let Ok(p) = prx.recv() {
            let pct = p.percent;
            qc.update(&jid, |j| {
                j.progress = pct;
                j.current_frame = p.frame;
                j.encode_fps = p.fps;
                j.encode_bitrate = p.bitrate.clone();
                j.encode_speed = p.speed.clone();
                j.current_time_ms = p.current_time_ms;
                j.duration_ms = p.duration_ms;
                j.current_stage = if pct >= 100.0 { "Finalizing".into() } else { format!("Encoding {:.0}%", pct) };
            });
            let now = std::time::Instant::now();
            if now.duration_since(last_broadcast).as_millis() as u64 >= THROTTLE_MS || pct >= 100.0 {
                last_broadcast = now;
                let determinate = p.duration_ms > 0 || p.total_frames > 0;
                let _ = qc.broadcast("progress", &serde_json::json!({
                    "id": jid,
                    "percent": pct,
                    "current_time_ms": p.current_time_ms,
                    "duration_ms": p.duration_ms,
                    "determinate": determinate,
                    "fps": p.fps,
                    "bitrate": p.bitrate,
                    "speed": p.speed,
                    "stage": if pct >= 100.0 { "Finalizing" } else { "Encoding" },
                }).to_string());
            }
        }
    });

    let result = encoder::transcode_file(tools, config, input_path, &probe_data, profile_id, &output_path, &metadata_uuid, ptx);

    // 500ms debounce sleep after the encoder finishes
    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut validation_ok = false;
    let mut validation_error = String::new();
    let mut final_probe = None;

    if result.success {
        // 1. File existence and size check
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
            validation_error = "File does not exist or has 0 bytes".to_string();
        } else {
            // 2. Write lock verification with exclusive write access
            #[cfg(target_os = "windows")]
            let lock_res = {
                use std::os::windows::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .share_mode(0)
                    .open(&result.output_path)
            };

            #[cfg(not(target_os = "windows"))]
            let lock_res = std::fs::OpenOptions::new()
                .write(true)
                .open(&result.output_path);

            let lock_ok = match lock_res {
                Ok(_) => true,
                Err(e) => {
                    tracing::error!("Exclusive write lock check failed for output file {:?}: {}", result.output_path, e);
                    false
                }
            };

            if !lock_ok {
                validation_error = "File write lock check failed (still being written or locked)".to_string();
            } else {
                // 3. FFprobe validation run
                match probe::probe_media(tools, &result.output_path) {
                    Ok(p) => {
                        // Check for at least one valid video stream and one valid audio stream
                        if p.width == 0 || p.height == 0 {
                            validation_error = "No valid video stream found in output file".to_string();
                        } else if p.audio_codec == "none" {
                            validation_error = "No valid audio stream found in output file".to_string();
                        } else {
                            // Check that duration matches expected duration within a tolerance
                            let output_duration = p.duration_secs;
                            let source_duration = probe_data.duration_secs;
                            let tolerance = (source_duration * 0.05).max(2.0);
                            if (output_duration - source_duration).abs() > tolerance {
                                validation_error = format!(
                                    "Duration mismatch: source={}s output={}s (tolerance={}s)",
                                    source_duration, output_duration, tolerance
                                );
                            } else {
                                validation_ok = true;
                                final_probe = Some(p);
                            }
                        }
                    }
                    Err(e) => {
                        validation_error = format!("FFprobe validation failed on output file: {}", e);
                    }
                }
            }
        }
    } else {
        validation_error = result.error.unwrap_or_else(|| "FFmpeg encoding failed".to_string());
    }

    if validation_ok && final_probe.is_some() {
        let output_probe = final_probe.unwrap();
        
        let keyframe_safe_start_ms = probe::get_keyframe_safe_start_ms(&tools.ffprobe, &result.output_path);
        
        let mut warnings_list = Vec::new();
        let mut mezzanine_ok = true;

        let fps = output_probe.fps();
        let expected_fps = 25.0;
        if (fps - expected_fps).abs() > 0.01 {
            warnings_list.push(format!("fps_mismatch: got {:.3} expected {:.3}", fps, expected_fps));
            mezzanine_ok = false;
        }

        let duration_ms = (output_probe.duration_secs * 1000.0).round() as i64;
        if duration_ms == 0 {
            warnings_list.push("zero_duration".to_string());
            mezzanine_ok = false;
        }

        let total_frames = output_probe.frame_count;
        let gop_frames = 25; // default closed GOP size in our profiles

        let _ = identity::write_sidecar_next_to_video(
            &result.output_path, 
            &metadata_uuid, 
            &probe_data, 
            &output_probe, 
            &profile_name, 
            "h264", 
            &config.encoding.audio_codec,
            mezzanine_ok,
            fps,
            total_frames,
            gop_frames,
            keyframe_safe_start_ms,
            &warnings_list,
        );

        let _ = handle.block_on(db::mark_ready(
            pool, 
            &metadata_uuid, 
            &result.output_path.to_string_lossy(), 
            duration_ms,
            mezzanine_ok,
            fps,
            total_frames,
            gop_frames,
            keyframe_safe_start_ms,
            &warnings_list,
        ));
        queue.update(&job.id, |j| {
            j.state = jobs::JobState::Completed;
            j.uuid = Some(metadata_uuid.clone());
            j.output_path = Some(result.output_path.to_string_lossy().into_owned());
            j.progress = 100.0;
            j.current_stage = "Completed".into();
            j.finished_at = Some(Utc::now().to_rfc3339());
        });
        queue.broadcast("completed", &serde_json::json!({"id":job.id,"uuid":metadata_uuid}).to_string());
        tracing::info!("Completed and verified: {} -> {} (uuid={})", input_path.display(), result.output_path.display(), metadata_uuid);
        if config.ingestion.clean_source_after_success {
            let _ = std::fs::remove_file(input_path);
        }
    } else {
        queue.update(&job.id, |j| {
            j.state = jobs::JobState::Failed;
            j.error = Some(validation_error.clone());
            j.current_stage = "Failed".into();
            j.finished_at = Some(Utc::now().to_rfc3339());
        });
        queue.broadcast("failed", &serde_json::json!({"id": job.id, "error": validation_error}).to_string());
        let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
        tracing::error!("Failed transcode validation: {} - {}", input_path.display(), validation_error);
        if result.output_path.exists() {
            let _ = std::fs::remove_file(&result.output_path);
        }
    }
    queue.prune_old(500);
}
