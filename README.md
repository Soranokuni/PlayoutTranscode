# PlayoutTranscode

[![Author](https://img.shields.io/badge/author-Soranokuni-blue.svg)](https://github.com/Soranokuni)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.77%2B-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Axum](https://img.shields.io/badge/Axum-0.8-9333ea.svg)](https://github.com/tokio-rs/axum)
[![FFmpeg](https://img.shields.io/badge/FFmpeg-Ready-green.svg)](https://ffmpeg.org/)
[![SQLite](https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white)](https://www.sqlite.org/)

**PlayoutTranscode** is an automated, broadcast-grade media ingestion, analysis, and mezzanine transcoding engine developed by **[Soranokuni](https://github.com/Soranokuni)**. Engineered in **Rust** as a resilient Windows service daemon, it monitors watch folders, performs rational FPS snapping, normalizes audio to **EBU R128 / ATSC A/85** loudness standards, transcodes incoming media into standardized frame-accurate mezzanine streams (Profiles A/B/C), writes JSON identity sidecars, and exposes a high-performance REST and Server-Sent Events (SSE) API to downstream clients like **[PlayOutVue](https://github.com/Soranokuni/PlayOutVue)**.

---

## Architecture & Ingest Pipeline

The pipeline guarantees zero partial-read hazards through atomic `.tmp` staging, strict FFprobe validation, two-pass loudness normalization, and deferred database publication.

```mermaid
graph TD
    WatchFolder[Watch Folder / Ingest Root]

    subgraph Ingest [PlayoutTranscode Ingestion Engine]
        Watcher[watcher.rs - Watcher & Settling Debounce]
        Queue[jobs.rs - In-Memory Job Queue]
        Processor[processor.rs - Pipeline Coordinator]
        Probe[probe.rs - FFprobe & Rational FPS Snapper]
        Loudness[probe.rs - Pass 1: EBU R128 Loudness Measurer]
        Fingerprint[fingerprint.rs - SHA-256 Deduplication]
        Encoder[encoder.rs - Pass 2: FFmpeg CFR & Loudnorm Transcoder]
        Validator[processor.rs - Mezzanine Quality Validation]
        Sidecar[identity.rs - Atomic JSON Sidecar Writer]
    end

    subgraph Storage [Storage Layer]
        StagingFile[Temporary Staging: .tmp_uuid_file]
        Mezzanine[Final Mezzanine: 1080p/1080i]
        JSONFile[.uuid.json Sidecar Metadata]
        SQLite[(SQLite DB: media_assets.db)]
    end

    subgraph API [REST & Real-Time Distribution]
        Axum[server.rs - Axum Web Server :4353]
        REST[REST API: /api/assets, /api/jobs, /api/folders]
        SSE[SSE Stream: /api/events]
        DBViewer[Embedded DB Viewer & UI]
    end

    Client[PlayOutVue MCR Client]

    WatchFolder -->|New File Detected| Watcher
    Watcher -->|Settling Confirmed| Queue
    Queue --> Processor
    Processor --> Probe --> Loudness --> Fingerprint --> Encoder
    Encoder -->|Write Transcode| StagingFile
    Processor -->|Validate Stream| Validator
    Validator --> StagingFile
    Processor -->|Atomic Rename| Mezzanine
    Processor -->|Write Sidecar| Sidecar --> JSONFile
    Processor -->|db::mark_ready| SQLite
    SQLite <--> REST
    Queue -->|Status Updates| SSE
    REST & SSE & DBViewer --> Axum
    Axum <--> Client
```

---

## Core Ingestion Features

1. **Active Settling & Debounce**: Detects file writes across network and local filesystems, applying file-settling heuristics to avoid reading partial files during copy operations.
2. **Rational FPS Snapping**: Snaps probed frame rates to exact broadcast rationals (e.g. `25/1`, `30000/1001`, `24000/1001`, `60000/1001`) instead of lossy float approximations.
3. **EBU R128 & ATSC A/85 Audio Normalization**: Automated two-pass loudness analysis and dynamic linear correction targeting statutory broadcast levels (-23 LUFS / -24 LUFS) with ITU-R BS.775 downmixing.
4. **Deterministic Transcoding**: Uses `libx264`, constant frame rate (CFR), 2.0-second closed GOPs, faststart `moov` atom placement, and 48 kHz stereo audio resampling.
5. **Zero Partial-Read Staging**: Encodes media directly into temporary files (`.tmp_{uuid}_{filename}`) on the target volume and atomically renames them only after complete validation.
6. **Mezzanine Contract Enforcement**: Never calls `db::mark_ready` until duration, closed GOP, audio rate, faststart header, and sidecar JSON are verified.
7. **Soft-Delete Recycle Bin & Reference Checking**: Allows soft-deleting media into a recycle bin, preventing physical deletion of assets that are actively scheduled in broadcast rundowns.
8. **Process Priority & Concurrency Throttling**: Limits simultaneous FFmpeg processes and sets OS thread priorities to `Below-Normal` to ensure playout machines remain responsive.

---

## Audio Loudness Normalization Engine

PlayoutTranscode provides professional, broadcast-standard audio normalization to eliminate loudness jumps between commercials, live shows, and legacy content:

### 1. Supported Loudness Standards
- **EBU R128 (European Broadcast Standard)**:
  - Integrated Loudness: **`-23.0 LUFS`** (configurable)
  - Maximum True Peak: **`-1.0 dBTP`**
  - Loudness Range (LRA): **`7.0 LU`**
- **ATSC A/85 (US / North American Standard)**:
  - Integrated Loudness: **`-24.0 LUFS`** (configurable)
  - Maximum True Peak: **`-2.0 dBTP`**
  - Loudness Range (LRA): **`11.0 LU`**
- **Legacy / Custom Mode**: Resamples to 48 kHz stereo with configurable bitrates (`aac`, `pcm_s16le`, `libmp3lame`).

### 2. Two-Pass Measurement & Correction
- **Pass 1 (Analysis)**: `RealLoudnessMeasurer` (`probe.rs`) runs FFmpeg with the `loudnorm` filter in JSON output mode, extracting exact `input_i`, `input_tp`, `input_lra`, and `input_thresh`.
- **Pass 2 (Correction)**: `profiles.rs` injects the measured values into the encoding filterchain (`measured_I`, `measured_TP`, `measured_LRA`, `measured_thresh`, `offset`, `linear`) to perform linear gain adjustment without dynamic pumping or distortion.

### 3. Channel Mapping & ITU-R BS.775 Downmix
- **Mono (1.0) $\rightarrow$ Dual Mono / Stereo**: `pan=stereo|c0=c0|c1=c0`
- **5.1 Surround (6.0) $\rightarrow$ Stereo Downmix**:
  ```text
  pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5
  ```
- **Passthrough Mode**: Set `preserve_original = true` to preserve source multi-channel audio tracks.

---

## Broadcast Encoding Profiles

PlayoutTranscode provides three standard broadcast profiles configurable in `config.toml`:

| Profile | Target Resolution | Scan Type | Color Matrix | FFmpeg Flags | Typical Use Case |
|---|---|---|---|---|---|
| **Profile A** | 1920x1080 | Progressive (CFR) | BT.709 | `-c:v libx264 -pix_fmt yuv420p -movflags +faststart` | HD progressive transmission & digital master |
| **Profile B** | 1920x1080 | Interlaced (TFF) | BT.709 | `-c:v libx264 -flags +ilme+ildct -top 1 -pix_fmt yuv420p` | HD 1080i broadcast playout |
| **Profile C** | 1920x1080 (pillarbox) | Progressive (CFR) | SMPTE 170M | `-vf "scale=1440:1080,pad=1920:1080:(ow-iw)/2:(oh-ih)/2"` | SD 4:3 archive upconversion |

### Common Stream Properties (All Profiles)
- **Video Codec**: `libx264` (CRF-based, closed GOP: 50 frames @ 25fps / 60 frames @ 29.97fps)
- **Audio Codec**: AAC / PCM stereo at **48,000 Hz** (EBU R128 / ATSC A/85 normalized)
- **Container**: MP4 with faststart enabled (`moov` atom at file beginning)

---

## API Surface

PlayoutTranscode exposes a RESTful API and SSE stream on port `4353`:

### Asset & Media Management
- `GET /api/assets`: List all registered, playable mezzanine assets.
- `GET /api/assets/{uuid}`: Retrieve detailed metadata for a specific asset.
- `PUT /api/assets/{uuid}/trim`: Update non-destructive in/out trim points (`trim_in_ms`, `trim_out_ms`).
- `PUT /api/assets/{uuid}/rating`: Update Greek NCRTV age rating (`K`, `8`, `12`, `16`, `18`) and timed advisory banner.
- `PUT /api/assets/{uuid}/tp`: Update Product Placement (`TP`) status.
- `POST /api/assets/{uuid}/subclip`: Create a virtual subclip without duplicating physical media.
- `POST /api/assets/{uuid}/trash`: Soft-delete an asset to the Recycle Bin.
- `POST /api/assets/{uuid}/restore`: Restore an asset from the Recycle Bin.
- `DELETE /api/assets/{uuid}/purge`: Permanently delete an asset from disk (reference-checked).
- `POST /api/assets/{uuid}/regenerate-sidecar`: Re-export `.uuid.json` metadata sidecar.

### Folders & Recycle Bin
- `GET /api/folders/colors`: Get color tags for virtual folder trees.
- `PUT /api/folders/colors`: Set folder color metadata.
- `POST /api/folders/trash`: Soft-delete an entire virtual folder tree.
- `POST /api/folders/restore`: Restore a trashed folder tree.
- `DELETE /api/folders/purge`: Permanently purge a trashed folder tree.
- `GET /api/recycle-bin`: List all trashed assets and folders.
- `DELETE /api/recycle-bin/purge`: Empty the Recycle Bin.
- `POST /api/recycle-bin/auto-purge`: Trigger background cleanup of expired trash items.

### Job Engine & Real-Time Monitoring
- `GET /api/jobs`: List transcode job history.
- `GET /api/jobs/active`: List currently running transcode jobs.
- `GET /api/jobs/pending`: List queued jobs waiting for execution.
- `GET /api/jobs/failed`: List failed jobs with error logs.
- `POST /api/jobs/{id}/retry`: Retry a failed job.
- `POST /api/jobs/{id}/cancel`: Cancel an active or pending job.
- `GET /api/events`: Server-Sent Events (SSE) stream emitting real-time job progress and completion events.

### System & Diagnostics
- `GET /api/health`: Service health, uptime, and database status.
- `GET /api/toolchain`: Probed status of `ffmpeg` and `ffprobe` binaries.
- `GET /api/config`: Read current runtime configuration.
- `PUT /api/config`: Update and reload configuration dynamically.
- `GET /api/diagnostics`: Export diagnostic package for troubleshooting.
- `GET /api/db/overview`: Embedded database viewer and table row counts.

---

## Configuration (`config.toml`)

> **Security warning.** The HTTP API is unauthenticated. `bind_address` must be a
> loopback address (`127.0.0.1`, `::1` or `localhost`); any other value is
> rejected at startup, because binding to `0.0.0.0` would expose every mutating
> route — including config changes and library purge — to the whole LAN.

```toml
[server]
web_port = 4353
bind_address = "127.0.0.1"
# Extra browser origins allowed by CORS. Loopback origins on web_port are
# always allowed; this is only needed for the Vue dev server.
allowed_origins = []

[paths]
watch_folder = "D:/Media/Ingest"
target_folder = "D:/Media/Mezzanine"
database_path = "D:/PlayoutTranscode/logs/media_assets.db"

[transcode]
default_profile = "ProfileA"
max_concurrency = 2
process_priority = "BelowNormal"
settling_delay_seconds = 5

[audio]
mode = "ebu_r128"          # Options: "ebu_r128", "atsc_a85", "legacy_v1_encode", "passthrough_validate"
codec = "aac"
bitrate = "320k"
sample_rate_hz = 48000
channels = 2
target_lufs = -23.0
true_peak_dbtp = -1.0
lra_target = 7.0
preserve_original = false

[cleanup]
auto_purge_days = 30
verified_source_cleanup = false
```

---

## Build & Run

### 1. Build from Source
Ensure Rust toolchain `1.77.2+` is installed:
```powershell
cargo check
cargo build --release
```

### 2. Run Interactively
```powershell
cargo run --release -- --config config.toml
```

### 3. Run Automated Contract Tests
Verify contract boundary invariants against PlayOutVue:
```powershell
cargo test
cargo test --test contract_boundary
```

---

## Author & Project Information

- **Author**: **[Soranokuni](https://github.com/Soranokuni)** (Alex Fountas)
- **Email**: [shadowsora13@hotmail.gr](mailto:shadowsora13@hotmail.gr)
- **Repository**: [https://github.com/Soranokuni/PlayoutTranscode](https://github.com/Soranokuni/PlayoutTranscode)

---

## License

This project is licensed under the **MIT License**.
