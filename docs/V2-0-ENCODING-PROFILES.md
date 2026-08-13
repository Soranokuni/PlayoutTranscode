# V2-0 Encoding Profiles — Frozen V1 FFmpeg Generation and Output Behavior

Baseline: commit `f1b86cc5428b02b3cb24762598b0a4909e5d599d` (`main`).
Reference code: `src/profiles.rs`, `src/encoder.rs`, `src/processor.rs`, `src/probe.rs`, `src/config.rs`.

This document freezes how V1 builds FFmpeg command lines and how it validates output. V2-0 does **not** change any of it.

## 1. Profile matrix (compile-time constants, `src/profiles.rs:39-76`)

| | Profile A | Profile B | Profile C |
|---|---|---|---|
| Target | 1920x1080 | 1920x1080 | 1920x1080 |
| Interlaced | no | yes (TFF) | no |
| colorspace / color_trc / color_primaries | bt709 | bt709 | smpte170m |
| h264 profile / level | high / 4.2 | high / 4.2 | high / 4.2 |
| pixel format | yuv420p | yuv420p | yuv420p |
| Default CRF / maxrate / bufsize | 24 / 15M / 16M | 23 / 15M / 16M | 20 / 5M / 6M |
| Extras | — | `-top 1 -field_order tt`, `x264 interlaced=1:pic-struct=1` | — |

Profile selection is made from the **source probe**: `height > 900` and interlaced field order (`tt|tb|tff|bff|bb|bt`) → B; `height > 900` progressive → A; else C (`src/probe.rs:39-48`). A disabled profile fails the job (`src/processor.rs:138-146`).

## 2. Observed V1 FPS behavior — MUST NOT be changed in V2-0

> V1 currently forces all encoded output to 25/1 CFR.
> This is observed behavior, not yet the final V2 contract.
> V2 must decide in a later profile/versioning PR whether to preserve source rational FPS or continue explicit target-profile normalization.

Evidence:
- `TARGET_FPS_NUM = 25`, `TARGET_FPS_DEN = 1` (`src/profiles.rs:4-5`) are used unconditionally; the source rationals passed into `build_ffmpeg_args` are ignored (`_source_fps_num`/`_source_fps_den`, `src/profiles.rs:100-101,105-106`).
- The `-vf` chain starts with `fps=25/1` and `-r 25/1` is passed to the encoder (`src/profiles.rs:123-124,141`), with `-fps_mode cfr` (`src/profiles.rs:127`).
- Output validation compares output fps against 25.0 (`src/processor.rs:267-272`); a source converted from another rate emits the informational warning `fps_converted` (`src/processor.rs:274-277`).
- `snap_fps_rational` still snaps source rationals to broadcast values (25/1, 30000/1001, 24000/1001, …, `src/probe.rs:286-301`), but the encoder then normalizes everything to 25/1.

## 3. Exact FFmpeg argument sequence (Profile A, default config, `src/profiles.rs:95-231` + `src/encoder.rs:100-103`)

Position-ordered (values in parentheses are defaults; `{}` = runtime value):

```text
-y
-hide_banner
-loglevel info
-stats
-analyzeduration (500M)
-probesize       (500M)
-fflags +genpts
-i <input_path>
-map_metadata -1
-map_chapters -1
-vf fps=25/1,scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p
-fps_mode cfr
-video_track_timescale 1000
-f mp4
-c:v libx264
-preset (medium)
-crf (24/23/20 per profile)
-maxrate (15M/15M/5M)
-bufsize (16M/16M/6M)
-profile:v high
-level 4.2
-pix_fmt yuv420p
-r 25/1
-colorspace <profile>
-color_trc <profile>
-color_primaries <profile>
-g 50
-keyint_min 50
-sc_threshold 0
-x264-params open-gop=0:keyint=50:min-keyint=50:scenecut=0          (Profile B appends :interlaced=1:pic-struct=1)
-top 1 -field_order tt                                               (Profile B only)
-tune (film, omitted when "none")
-movflags +faststart
-threads <effective_threads_per_encode>
-map 0:v:0
-map 0:a:0?
-c:a <audio_codec>
<audio branch args — see §4>
-max_muxing_queue_size 4096
-metadata playoutvue_id=<uuid>           ← inserted here by encoder (src/encoder.rs:100-103)
<output_path>
```

Properties inherited from these args:
- GOP = 2 seconds: `compute_gop_size(fps) = round(fps × 2)`, 50 frames at 25 fps (`src/profiles.rs:244-248`); enforced by `-g`, `-keyint_min`, `open-gop=0`, `scenecut=0`.
- Container: MP4, faststart (`+faststart`). `-map_metadata -1` strips source metadata except the injected `playoutvue_id` tag.
- Audio mapping is optional (`0:a:0?`): sources without audio still encode.
- `-threads` is always explicit: `ffmpeg_threads` override, else `cpu_cores / max_concurrency` (min 1) (`src/config.rs:74-94`).
- `-video_track_timescale` = `fps_den × 1000` = 1000.

## 4. Audio branch (`src/profiles.rs:202-225`)

Always resampled/downmixed to **48 kHz stereo (2 channels)**:

| audio_codec | extra args | notes |
|---|---|---|
| `aac` (default) | `-b:a` (320k default), `-ar 48000`, `-ac 2`, `-async 1` | `-async 1` only on this branch |
| `pcm_s16le` | `-ar 48000`, `-ac 2` | no bitrate arg |
| `libmp3lame` | `-b:a` (320k), `-ar 48000`, `-ac 2` | |

No loudness processing, no channel-layout preservation, no passthrough, no mono/dual-mono policy exists in V1. **Two-pass loudnorm is NOT implemented** (no `loudnorm`/`ebur128` reference exists in any source file; see the V2-8 roadmap slice).

## 5. Progress and diagnostics (`src/encoder.rs`)

- Parsed from `-stats` stderr lines via regexes: `time=`, `frame=`, `fps=`, `bitrate=`, `speed=` (`src/encoder.rs:12-16,182-236`); progress events throttled to ≥ 250 ms in `src/processor.rs:159-191`.
- stderr ring buffer capped at 200 lines (`src/encoder.rs:160`); on failure the last 50 lines are surfaced as `stderr_log` on the job (`src/encoder.rs:284`) and a one-line summary via `summarize_stderr` (`src/encoder.rs:50-78`).
- The ffmpeg PID is registered in `active_pids` for service-stop kill (`src/encoder.rs:130-135`).

## 6. Post-encode validation sequence (`src/processor.rs:198-307`)

1. Output file exists and size > 0 (`:219-227`).
2. Advisory exclusive-lock probe (`try_acquire_output_lock`, `:232-240,472-501`) — failure is **advisory only**; validation continues.
3. `probe_with_retry`: ffprobe of the output, 3 attempts × 500 ms (`:506-531`).
4. `classify_probe_match`: video stream present; audio absence is warn-only; output duration within 2 frames of the source duration (min tolerance 40 ms) (`:535-559`).
5. Contract checks (`:259-307`), each appending to `warnings` and clearing `mezzanine_ok` on failure:

| warning code | condition | blocks ready? |
|---|---|---|
| `fps_mismatch: got X expected 25` | output fps deviates > 0.01 from 25.0 | no — warning only; `mezzanine_ok=false`, **status still `ready`** |
| `fps_converted: source X -> output Y` | source fps ≠ 25 | informational only |
| `zero_duration` | output duration ≤ 0 ms | no — warning only |
| `audio_sample_rate_not_48k: got N Hz` | output audio sample rate ≠ 48000 | no — warning only |
| `closed_gop_violation` | keyframe spacing not uniform at ≤ 2 s interval (tolerance half frame; fewer than 2 keyframes passes) | no — warning only |
| `missing_faststart` | "moov" atom not found in first 64 KiB | no — warning only |
| `trim_in_not_keyframe_aligned` (subclip API only, `src/server.rs:917-930`) | subclip trim_in not within half a frame of a keyframe | n/a — sets subclip `mezzanine_ok=false` |

**Observed publication semantics:** in the current code these checks only produce warnings. `db::mark_ready` is called regardless with the `mezzanine_ok` flag, so an asset can be published with `status="ready"` and `mezzanine_ok=false`. Hard failures that route to `status="error"` + output deletion are: probe failure, encode failure, missing/zero-byte output, and duration mismatch beyond tolerance (`src/processor.rs:358-371`). V2 (AGENTS.md "Bold Rule") intends stricter ready semantics; that is a future, deliberate change — not V2-0.

6. `mezzanine_ok` additionally requires: `fps` field (float), `fps_num/fps_den` rationals from the output probe, `total_frames`, `gop_frames` (= round(fps×2)), `keyframe_safe_start_ms` (first keyframe, `src/probe.rs:303-332`), `keyframe_offsets` ms list (`src/processor.rs:596-637`).

## 7. Sidecar and DB publication order (`src/processor.rs:309-354`)

encode → flush-wait (`wait_for_file_flush`, `:561-594`) → validate → **write sidecar** → **`mark_ready`** → emit `completed` event. There is no `.tmp`-then-rename stage; ffmpeg writes directly to the final `videos/<stem>_<uuid>.mp4` name with `-y` (observable V1 behavior; atomic publication is the V2-6 slice).

## 8. Frozen configuration inputs

Fields of `[encoding]` / `[profiles]` that directly shape commands: `preset`, `tune`, `probesize`, `analyzeduration`, `audio_codec`, `audio_bitrate`, `ffmpeg_threads`, `cpu_cores`, per-profile `enabled/crf/maxrate/bufsize` (`src/config.rs:36-130`). Unknown/missing fields fall back to serde defaults on load; there is no config version key in V1 (V2-1 adds versioning — not V2-0).