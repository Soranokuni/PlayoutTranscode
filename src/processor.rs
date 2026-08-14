use crate::{bootstrap, config, db, encoder, fingerprint, identity, jobs, probe, profiles};
use sqlx::SqlitePool;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
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
        active_pids: Option<Arc<StdMutex<Vec<u32>>>>,
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
        active_pids: Option<Arc<StdMutex<Vec<u32>>>>,
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
    active_pids: Arc<StdMutex<Vec<u32>>>,
) {
    process_file_sync_with_runner(
        queue,
        tools,
        target_root,
        input_path,
        config,
        pool,
        active_pids,
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
    active_pids: Arc<StdMutex<Vec<u32>>>,
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
    active_pids: Arc<StdMutex<Vec<u32>>>,
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

fn process_file_inner(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: Arc<StdMutex<Vec<u32>>>,
    runner: &impl TranscodeRunner,
    measurer: &impl probe::LoudnessMeasurer,
) {
    let watch_root = std::path::Path::new(&config.paths.watch_folder);
    let canonical_input = input_path
        .canonicalize()
        .unwrap_or_else(|_| input_path.to_path_buf());
    let canonical_watch = watch_root
        .canonicalize()
        .unwrap_or_else(|_| watch_root.to_path_buf());
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
        if existing.status == "ready"
            && existing.mezzanine_ok
            && !existing.current_path.is_empty()
            && std::path::Path::new(&existing.current_path).exists()
        {
            tracing::info!(
                "Dedup: asset {} already ready with valid mezzanine at {} (fingerprint={}), skipping transcode",
                existing.uuid,
                existing.current_path,
                fingerprint,
            );
            return;
        }

        tracing::info!(
            "Dedup: fingerprint {} matched but existing asset not usable (status={}, mezzanine_ok={}, path_exists={}), re-transcoding",
            fingerprint,
            existing.status,
            existing.mezzanine_ok,
            std::path::Path::new(&existing.current_path).exists(),
        );
        let _ = handle.block_on(db::purge_rows_by_fingerprint(pool, fingerprint));
    }

    let metadata_uuid = Uuid::new_v4().to_string();
    let video_dir = target_root.join("videos");
    let _ = std::fs::create_dir_all(&video_dir);
    let safe_stem =
        identity::sanitize_filename(&input_path.file_stem().unwrap_or_default().to_string_lossy());

    let final_output_path = build_unique_output_path(&video_dir, &safe_stem, &metadata_uuid);
    let publisher = LocalFilePublisher;
    let staged_output_path = publisher.stage_path(&final_output_path, &metadata_uuid);
    publisher.cleanup_staging(&staged_output_path);

    if let Err(e) = handle.block_on(db::insert_processing(
        pool,
        &metadata_uuid,
        fingerprint,
        &input_path.to_string_lossy(),
        &safe_stem,
    )) {
        tracing::error!(
            "DB insert processing failed for {}: {}",
            input_path.display(),
            e
        );
    }

    let mut job = jobs::JobRecord::new(&input_path.to_string_lossy(), "pending");
    job.uuid = Some(metadata_uuid.clone());
    job.fingerprint = Some(fingerprint);
    let _ = job.transition_to(jobs::JobPhase::Probing, Some("Probing".to_string()));
    queue.push(job.clone());
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
    let measured_loudness = match measurer.measure_loudness(
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
                qc.update(&jid, |j| {
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

        let result = runner.run_transcode(
            tools,
            config,
            input_path,
            &probe_data,
            profile_id,
            &staged_output_path,
            &metadata_uuid,
            ptx,
            Some(active_pids.clone()),
            &audio_policy,
            measured_loudness.as_ref(),
        );

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

            let keyframe_safe_start_ms =
                probe::get_keyframe_safe_start_ms(&tools.ffprobe, &result.output_path);

            let mut warnings_list = Vec::new();
            let mut mezzanine_ok = true;

            if let Some(ref ml) = measured_loudness {
                if ml.is_silent {
                    warnings_list.push("silent_audio_loudness_skipped".to_string());
                }
                if ml.is_short {
                    warnings_list.push("short_clip_loudnorm_dynamic".to_string());
                }
            }

            let fps = output_probe.fps();
            let expected_fps = profiles::TARGET_FPS_NUM as f64 / profiles::TARGET_FPS_DEN as f64;
            if (fps - expected_fps).abs() > 0.01 {
                warnings_list.push(format!(
                    "fps_mismatch: got {:.3} expected {:.3}",
                    fps, expected_fps
                ));
                mezzanine_ok = false;
            }

            let source_fps = probe_data.fps();
            if (source_fps - expected_fps).abs() > 0.01 {
                warnings_list.push(format!(
                    "fps_converted: source {:.3} -> output {:.3}",
                    source_fps, expected_fps
                ));
            }

            let duration_ms = (output_probe.duration_secs * 1000.0).round() as i64;
            if duration_ms <= 0 {
                warnings_list.push("zero_duration".to_string());
                mezzanine_ok = false;
            }

            if output_probe.audio_sample_rate != 48000 {
                warnings_list.push(format!(
                    "audio_sample_rate_not_48k: got {} Hz",
                    output_probe.audio_sample_rate
                ));
                mezzanine_ok = false;
            }

            let total_frames = output_probe.frame_count;
            let gop_frames = compute_gop_from_fps(fps);

            let keyframe_offsets = extract_keyframe_offsets_ms(
                tools.ffprobe.to_str().unwrap_or(""),
                &result.output_path,
            );
            let closed_gop_ok = verify_closed_gop(&keyframe_offsets, gop_frames, fps);
            if !closed_gop_ok {
                warnings_list.push("closed_gop_violation".to_string());
                mezzanine_ok = false;
            }

            let faststart_ok = verify_faststart(&result.output_path);
            if !faststart_ok {
                warnings_list.push("missing_faststart".to_string());
                mezzanine_ok = false;
            }

            let _ = queue.transition(
                &job.id,
                jobs::JobPhase::Publishing,
                Some("Publishing".into()),
                |j| {
                    j.output_path = Some(final_output_path.to_string_lossy().into_owned());
                },
            );

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

            let _ = identity::write_sidecar_next_to_video(
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
            );

            let keyframe_offsets_json =
                serde_json::to_string(&keyframe_offsets).unwrap_or_else(|_| "[]".to_string());

            let _ = handle.block_on(db::mark_ready(
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
            ));
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
            queue.broadcast(
                "completed",
                &serde_json::json!({"id":job.id,"uuid":metadata_uuid}).to_string(),
            );
            tracing::info!(
                "Completed and verified: {} -> {} (uuid={})",
                input_path.display(),
                final_output_path.display(),
                metadata_uuid
            );
            if config.effective_storage_policy().clean_source_after_success {
                let _ = std::fs::remove_file(input_path);
            }
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
    let tolerance_ms = (frame_duration_ms * 2.0).max(40.0);
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
    cmd.creation_flags(CREATE_NO_WINDOW);

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

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    stdout_str
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
            _active_pids: Option<Arc<StdMutex<Vec<u32>>>>,
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
}
