# PlayoutTranscode

PlayoutTranscode is the upstream media ingestion, analysis, and preparation service. It is designed to run as a background service on Windows, monitoring ingestion paths, normalizing source files into broadcast-compliant, frame-accurate progressive/interlaced mezzanine streams, and publishing structured metadata for consumption by the downstream **PlayOutVue** client.

---

## Architecture & Data Flow

Below is the flow of media and metadata within the `PlayoutTranscode` system, starting from filesystem detection to REST and SSE API publication.

```mermaid
graph TD
    classDef source fill:#3b82f6,stroke:#1d4ed8,stroke-width:2px,color:#fff;
    classDef process fill:#f59e0b,stroke:#b45309,stroke-width:2px,color:#fff;
    classDef db fill:#10b981,stroke:#047857,stroke-width:2px,color:#fff;
    classDef api fill:#8b5cf6,stroke:#6d28d9,stroke-width:2px,color:#fff;
    
    WatchFolder[[Watch Folder / Ingest Path]]:::source
    
    subgraph Ingest_Pipeline [Ingest & Transcode Pipeline]
        Watcher[watcher.rs - Filesystem Watcher]:::process
        ServiceHandle[service_handle.rs - Service Coordinator]:::process
        JobQueue[jobs.rs - In-Memory Job Queue]:::process
        Processor[processor.rs - Ingest Pipeline Coordinator]:::process
        
        Bootstrap[bootstrap.rs - ffmpeg/ffprobe discovery]:::process
        Probe[probe.rs - ffprobe analyzer & rational FPS snapper]:::process
        Encoder[encoder.rs - ffmpeg runner & progress parser]:::process
        Fingerprint[fingerprint.rs - SHA-256 builder]:::process
        Identity[identity.rs - JSON sidecar writer]:::process
    end
    
    subgraph Storage [Storage Layer]
        SQLiteDB[(SQLite Database - logs/media_assets.db)]:::db
        JSONSidecar[[JSON Sidecar file: .uuid.json]]:::db
        ComplianceMezzanine[[Compliance Mezzanine File]]:::db
    end

    subgraph API_Layer [API & Distribution Layer]
        AxumServer[server.rs - Axum Web Server<br>Port: 4353]:::api
        REST_API[REST API endpoints<br>/api/assets<br>/api/health<br>/api/jobs]:::api
        SSE_Stream[SSE Endpoint<br>/api/events]:::api
    end
    
    PlayOutClient[PlayOutVue Client]:::source

    %% Inflow and queuing
    WatchFolder -- New File Detected --> Watcher
    Watcher -- Adds file info --> JobQueue
    ServiceHandle -- Polls and triggers processing --> JobQueue
    JobQueue -- Job details --> Processor
    
    %% Processing steps
    Processor -- 1. Locate binaries --> Bootstrap
    Processor -- 2. ffprobe metadata --> Probe
    Processor -- 3. SHA-256 check --> Fingerprint
    Processor -- 4. Transcode to Profiles A/B/C --> Encoder
    Processor -- 5. Validate & write sidecar --> Identity
    Processor -- 6. Insert metadata --> SQLiteDB
    
    Encoder -- Output file --> ComplianceMezzanine
    Identity -- Output sidecar --> JSONSidecar
    
    %% API and communication
    SQLiteDB <--> REST_API
    JobQueue -- Real-time status --> SSE_Stream
    
    PlayOutClient -- Polls/mutates --> REST_API
    PlayOutClient -- Subscribes to events --> SSE_Stream
```

---

## Core Features

1. **Active Folder Watching**: Monitors folders for incoming file additions. Settles files to avoid processing during copy/write.
2. **Rational FPS Snapping**: Extracts metadata via `ffprobe` and snaps frame rates to exact broadcast rationals (e.g. `25/1`, `30000/1001`, `24000/1001`) instead of float approximations.
3. **FFmpeg Ingestion Engine**: Transcodes files into compliance profiles A/B/C using `libx264`, constant frame rate (CFR), closed GOP, faststart, and 48 kHz stereo audio.
4. **Fingerprinting & De-duplication**: Uses SHA-256 hashing to check files for duplicate content, preventing redundant transcoding jobs.
5. **Mezzanine Contract Validation**: Enforces that no asset is marked `ready` in the database unless it conforms to the strict playback requirements of the downstream player.
6. **Sidecar Identity Metadata**: Writes `.uuid.json` sidecar files containing stable media definitions.

---

## Ingest Encoding Profiles

| Profile | Output Specs | Color Space | Interlacing | Intended Use |
|---|---|---|---|---|
| **Profile A** | 1920x1080 | BT.709 | Progressive (CFR) | HD progressive distribution |
| **Profile B** | 1920x1080 | BT.709 | Interlaced (TFF) | HD broadcast transmission |
| **Profile C** | 1920x1080 (pillarboxed) | SMPTE 170M | Progressive (CFR) | SD 4:3 legacy conversion |

---

## API Surface

- `GET /api/health`: Health check endpoint.
- `GET /api/assets`: List all registered ready assets.
- `GET /api/assets/{uuid}`: Fetch detailed metadata for a specific asset.
- `POST /api/assets/{uuid}/trim`: Update non-destructive trim points.
- `POST /api/assets/{uuid}/rating`: Update compliance rating/descriptors.
- `GET /api/jobs`: List transcode job history.
- `GET /api/events`: Event stream (SSE) emitting real-time status updates (`Pending`, `Processing`, `Completed`, `Failed`).

---

## Build and Run

### Build Service
Ensure you have the Rust toolchain installed:
```powershell
cargo check
cargo build --release
```

### Run Service
```powershell
cargo run --release -- --config logs/config.toml
```

---

## Verification & Testing
Execute unit and integration tests to check transcode contracts and API boundary invariants:
```powershell
cargo test
cargo test --test contract_boundary
```
