use crate::{bootstrap, config, db, encoder, fingerprint, identity, jobs, probe, profiles};
use chrono::Utc;
use sqlx::SqlitePool;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use uuid::Uuid;

pub fn process_file_sync(
    queue: &jobs::JobQueue,
    tools: &bootstrap::ToolPaths,
    target_root: &Path,
    input_path: &Path,
    config: &config::AppConfig,
    pool: &SqlitePool,
    active_pids: Arc<StdMutex<Vec<u32>>>,
) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        process_file_inner(queue, tools, target_root, input_path, config, pool, active_pids);
    }));

    if let Err(panic_payload) = result {
        let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic in process_file_sync".to_string()
        };
        tracing::error!("PANIC in process_file_sync for {}: {}", input_path.display(), msg);
        queue.broadcast("failed", &serde_json::json!({
            "error": format!("Internal panic: {}", msg),
            "path": input_path.to_string_lossy(),
        }).to_string());
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
    let safe_stem = identity::sanitize_filename(
        &input_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy(),
    );

    let output_path = build_unique_output_path(&video_dir, &safe_stem, &metadata_uuid);

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

    let result = encoder::transcode_file(
        tools, config, input_path, &probe_data, profile_id,
        &output_path, &metadata_uuid, ptx, Some(active_pids),
    );

    if result.success {
        wait_for_file_flush(&result.output_path, 5000);
    }

    let mut validation_ok = false;
    let mut validation_error = String::new();
    let mut final_probe = None;

    // Capture stderr tail regardless of outcome so the UI collapsible section always has data.
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
                // Lock is busy: this happens momentarily on Windows when antivirus / search
                // indexer touch the freshly-written file. We do NOT hard-fail any more — we
                // log a warning and proceed to ffprobe, which will surface true lock issues.
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

    if validation_ok && final_probe.is_some() {
        let output_probe = final_probe.unwrap();

        let keyframe_safe_start_ms = probe::get_keyframe_safe_start_ms(&tools.ffprobe, &result.output_path);

        let mut warnings_list = Vec::new();
        let mut mezzanine_ok = true;

        let fps = output_probe.fps();
        let expected_fps = probe_data.fps();
        if (fps - expected_fps).abs() > 0.01 {
            warnings_list.push(format!("fps_mismatch: got {:.3} expected {:.3}", fps, expected_fps));
            mezzanine_ok = false;
        }

        let duration_ms = (output_probe.duration_secs * 1000.0).round() as i64;
        if duration_ms <= 0 {
            warnings_list.push("zero_duration".to_string());
            mezzanine_ok = false;
        }

        if output_probe.audio_sample_rate != 48000 {
            warnings_list.push(format!("audio_sample_rate_not_48k: got {} Hz", output_probe.audio_sample_rate));
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

        let _ = identity::write_sidecar_next_to_video(
            &result.output_path,
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
        );

        let keyframe_offsets_json = serde_json::to_string(&keyframe_offsets).unwrap_or_else(|_| "[]".to_string());

        let _ = handle.block_on(db::mark_ready(
            pool,
            &metadata_uuid,
            &result.output_path.to_string_lossy(),
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
    if gop > 0 { gop } else { 50 }
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
        tracing::warn!("Output has no audio stream (source had codec={})", source.audio_codec);
    }
    let output_duration = p.duration_secs;
    let source_duration = source.duration_secs;
    let fps = p.fps();
    let frame_duration_ms = if fps > 0.0 { (1000.0 / fps).round() as f64 } else { 40.0 };
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
        "-v", "error",
        "-select_streams", "v:0",
        "-skip_frame", "nokey",
        "-show_entries", "frame=pts_time",
        "-of", "csv=p=0",
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
}
