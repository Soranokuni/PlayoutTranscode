# AGENTS.md — PlayoutTranscode

## Bold Rule

**No asset may become `ready` unless its API payload already matches PlayOutVue's hydrated playout contract.**

In practical terms, that means all of the following must be true before `db::mark_ready` is called:

- `path` (a.k.a. `current_path`) is final — points to the exact Caspar-playable mezzanine, not a transient ingest path.
- `duration_ms` is exact — recomputed from the final output via ffprobe, not from the source.
- `trim_in_ms` and `trim_out_ms` are valid — `trim_in_ms=0`, `trim_out_ms=duration_ms` on first publish.
- `fps_num` and `fps_den` are present and > 0 — the real rational from the output file, snapped to broadcast standards.
- `mezzanine_ok` is honestly evaluated — true only if closed GOP, faststart, 48 kHz audio, and fps match are all verified.

If any of these are not true, publish `status="processing"` or `status="error"` and keep the item out of the playout path.

## Integration Test

The boundary test lives in `tests/contract_boundary.rs`. It validates the full chain:

```
Transcode publishes ready asset
  -> PlayOutVue hydrates it without repair
  -> dispatcher can compute frame trim
  -> Caspar registration uses stable path and valid duration
```

Run it with:

```
cargo test --test contract_boundary
```

This single test catches most classes of contract breakage. Any change that alters trim semantics, path stability, FPS rational handling, or duration authority must keep this test green.

## Architecture

PlayoutTranscode is the **upstream media-preparation service**. It owns:
- Media ingestion (watch folder + polling)
- Probing (ffprobe: duration, fps rational, audio, keyframes)
- Transcoding (ffmpeg: libx264, CFR, closed GOP, faststart)
- Metadata publication (SQLite DB + REST API + JSON sidecar)
- Validation (output duration, fps match, closed GOP, faststart, audio 48 kHz)

PlayOutVue is the **downstream consumer**. It owns:
- Rundown editing and operator actions
- Frame-accurate trim computation (`compute_frame_trim`)
- CasparCG command dispatch (`PLAY ... SEEK ... LENGTH`)
- OSC-based playback tracking and auto-advance

## API Contract Surface

Every `GET /api/assets/{uuid}` response (the `AssetResponse` struct) must include:

| Field | Type | Constraint |
|---|---|---|
| `uuid` | string | stable unique identifier (= `playoutvue_id`) |
| `playoutvue_id` | string | alias for `uuid`, for PlayOutVue hydration |
| `current_path` | string | final Caspar-playable file path |
| `duration_ms` | i64 | exact duration in ms, > 0 when ready |
| `trim_in_ms` | i64 | absolute ms from file start, >= 0 |
| `trim_out_ms` | i64 | absolute ms from file start, > trim_in_ms, <= duration_ms |
| `fps_num` | i64 | rational numerator, > 0 when ready |
| `fps_den` | i64 | rational denominator, > 0 when ready |
| `mezzanine_ok` | bool | true only if frame-accurate-safe |
| `fps` | f64 | float approximation (for backward compat) |
| `total_frames` | i64 | total frame count |
| `gop_frames` | i64 | GOP size in frames |
| `keyframe_safe_start_ms` | i64 | first keyframe offset |
| `warnings` | string[] | validation warnings |
| `keyframe_offsets` | i64[] | all keyframe positions in ms |

## Encoding Profiles

| Profile | Target | Interlaced | Color | Use Case |
|---|---|---|---|---|
| A | 1920x1080 | No | bt709 | HD progressive |
| B | 1920x1080 | Yes (TFF) | bt709 | HD interlaced |
| C | 1920x1080 (pillarbox) | No | smpte170m | SD PAL 4:3 |

All profiles:
- libx264, CFR, closed GOP (2-second keyframe interval)
- Preserve source fps (snapped to broadcast rationals: 25/1, 30000/1001, 24000/1001, etc.)
- CRF-based (configurable per profile; smaller files = higher CRF)
- faststart enabled
- Audio: 48 kHz stereo (AAC/PCM/MP3 configurable)

## Do Not Break

If you touch this repo, do not merge changes that alter:
- Trim reference semantics (absolute ms from file start)
- Path normalization semantics (final path = mezzanine path)
- Ready/error state meanings (ready = playable now, no more processing needed)
- FPS rational handling (must be exact rational, not float approximation)
- Playback registration timing (metadata published only when truly ready)

Any such change must update both repos and the contract together.

## Build

```
cargo check          # type check
cargo test           # unit + integration tests
cargo build --release  # production binary
```

## File Map

| File | Responsibility |
|---|---|
| `src/main.rs` | Entry point, CLI, service lifecycle |
| `src/config.rs` | TOML config, wizard, validation |
| `src/bootstrap.rs` | FFmpeg/FFprobe discovery, download |
| `src/probe.rs` | ffprobe wrapper, fps snapping, duration resolution |
| `src/profiles.rs` | Encoding profiles A/B/C, ffmpeg arg builder |
| `src/encoder.rs` | ffmpeg transcode execution, progress parsing |
| `src/processor.rs` | Pipeline: probe -> encode -> validate -> publish |
| `src/db.rs` | SQLite schema, asset CRUD, trim/rating/tp mutations |
| `src/identity.rs` | JSON sidecar writer, filename sanitization |
| `src/fingerprint.rs` | SHA-256 content fingerprinting for dedup |
| `src/server.rs` | Axum REST API + SSE + SPA server |
| `src/watcher.rs` | Filesystem watcher (notify + polling) |
| `src/service_handle.rs` | Processing loop, ffmpeg PID tracking, shutdown |
| `src/jobs.rs` | In-memory job queue with SSE broadcast |
| `src/logging.rs` | tracing subscriber setup |
| `tests/contract_boundary.rs` | Integration test at the PlayOutVue boundary |
