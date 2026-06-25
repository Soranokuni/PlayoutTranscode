# Plan: PlayoutTranscode WebUI Real-Time Progress + MCR Dark-Mode Reskin

## Goal
Refactor the `web-ui` dashboard into a clean, production-grade, dark-mode broadcast monitor with live transcode progress bars driven by the Axum backend. Fix the stuck progress bars by repairing the SSE protocol and using the streamed chunk data. Handle missing/corrupted duration metadata gracefully with an elegant indeterminate pulse animation.

## Current state
- Vue 3 + Vite + TypeScript frontend in `web-ui/`.
- Rust/Axum backend in `src/server.rs` already exposes `GET /api/events` via SSE and `tokio::sync::broadcast`.
- `src/encoder.rs` parses FFmpeg progress lines (`frame`, `time`, `fps`, `speed`, `bitrate`) and emits `EncodeProgress`.
- **Root cause of stuck bars:** `src/jobs.rs::broadcast` hand-formats SSE text (`event: …\ndata: …\n\n`), then `src/server.rs::sse_events` wraps that entire string in `Event::default().data(...)`. The double framing breaks named `EventSource` listeners, so the UI falls back to 2s polling and progress never updates in real time.
- The asset database (`src/db.rs`) supports `ready | processing | error` status but there is no listing endpoint.

## Decisions
1. **Transport:** Keep SSE (route `/api/events`), do **not** switch to WebSockets.
2. **Payload:** Broadcast a JSON envelope `{ "event": <string>, "data": <json> }` from `JobQueue`, and let `server.rs` build a proper `Event::default().event(...).data(...)`.
3. **Frontend architecture:** Split the monolithic `App.vue` into small components and a composable:
   - `components/BroadcastTopBar.vue`
   - `components/IngestQueuePanel.vue`
   - `components/AssetRegistryGrid.vue`
   - `components/ProgressBar.vue`
   - `composables/useEventStream.ts`
4. **Data sources:**
   - Active queue: `/api/jobs/active` + `/api/jobs/failed` on mount, then live `progress`/`state`/`completed`/`failed` SSE events.
   - Global registry: new `GET /api/assets[?status=...]`, refreshed on asset state changes.
5. **Progress formula shown in UI:** `percent = (current_processed_time_ms / total_source_duration_ms) * 100`. The backend sends both raw values; the UI computes percent and ETA client-side.
6. **Styling:** Refine existing custom CSS variables in `main.css` to a Slate/Zinc, high-contrast broadcast palette. No Tailwind.
7. **Failed items:** Show processing and failed rows inside the Active Ingest Queue Table. Failed rows render a crimson alert card populated with the exact `error` text from the job.

## Backend work

### 1. Fix the SSE framing
- In `src/jobs.rs`:
  - Change the internal `broadcast` format from the raw SSE text line to a JSON envelope:
    ```rust
    let envelope = serde_json::json!({"event": event_type, "data": payload}).to_string();
    let _ = self.event_tx.send(envelope);
    ```
  - Keep the broadcast channel capacity at `256` but consider throttling progress emitters.
- In `src/server.rs::sse_events`:
  - Receive the JSON envelope, parse into a small helper struct.
  - Produce:
    ```rust
    Event::default().event(envelope.event).data(envelope.data)
    ```
  - Keep `KeepAlive::default()`.

### 2. Enrich progress data
- In `src/encoder.rs`:
  - Add `current_time_ms: i64` and `duration_ms: i64` to `EncodeProgress`.
  - Parse `time=HH:MM:SS.XX` into milliseconds and store in `current_time_ms`.
  - Compute `percent`:
    - If `duration_ms > 0`: `(current_time_ms as f64 / duration_ms as f64) * 100.0`, capped at `99.0`.
    - Else if `total_frames > 0`: frame-based ratio (existing behavior), capped at `99.0`.
    - Else `0.0`.
  - Set `duration_ms` from `source_probe.duration_secs * 1000`.
- In `src/processor.rs` progress-receiver thread:
  - Update the existing local `JobRecord` fields (`progress`, `current_frame`, `encode_*`).
  - Broadcast `progress` events under the JSON envelope, throttled to one emit every **250 ms** or on every `>= 0.5%` percent change.
  - Include in each `progress` event:
    - `job_id`, `current_time_ms`, `duration_ms`, `percent`, `determinate` (`duration_ms > 0 || total_frames > 0`), `speed`, `fps`, `bitrate`, `stage`.
  - On completion broadcast `completed` event; on failure broadcast `failed` event including the full `error` text.

### 3. Add uptime + global asset listing
- In `src/server.rs`:
  - Track service up-time via `std::time::Instant` stored in `ServerState`.
  - Add `uptime_ms: u64` to `/api/health`.
  - Add route `GET /api/assets` that calls a new `db::find_all(pool, status_filter)`.
- In `src/db.rs`:
  - Add `pub async fn find_all(pool, status_filter: Option<&str>) -> Result<Vec<MediaAsset>>` with an optional `WHERE status = ?` clause.

## Frontend work

### 1. `composables/useEventStream.ts`
- Create `EventSource('/api/events')` in `onMounted`.
- Typed event handlers for: `connected`, `progress`, `state`, `completed`, `failed`.
- Maintain a reactive `jobs` map keyed by `id` merged from the initial `/api/jobs/active` & `/api/jobs/failed` fetch and live events.
- On `progress`, merge fields into the matching job without refetching.
- On `completed`/`failed`, refresh the active/failed lists from REST and refresh the asset registry grid.
- Auto-reconnect with exponential backoff (max 5s) on `onerror`.
- Provide a helper `isDeterminate(job): boolean` and `etaText(job): string`.

### 2. `components/ProgressBar.vue`
- Props: `percent: number`, `determinate: boolean`, `speed?: string`, `eta?: string`.
- Determinate: outer bar with `.progress-fill` whose `width` is clamped `0–100%` and a CSS `transition: width 300ms ease-in-out`. Show percentage text and, when available, ETA + speed.
- Indeterminate: an animated CSS pulse bar (`@keyframes pulse`) with text "Analyzing…" and no percentage.
- Guard all arithmetic with `Number.isFinite(percent)`; fallback to indeterminate if `NaN`/`Infinity`.

### 3. `components/BroadcastTopBar.vue`
- Display from `/api/watchfolder` + `/api/health`:
  - Watch folder path
  - Target folder path
  - `max_concurrency`
  - Uptime (`uptime_ms` → `HH:MM:SS`)
- Keep toolchain status and service start/stop controls from the existing header.

### 4. `components/IngestQueuePanel.vue`
- Fetch active and failed jobs on mount (`/api/jobs/active`, `/api/jobs/failed`).
- Render rows:
  - **Processing:** filename, profile, stage, `ProgressBar` with percent/ETA/speed, small `fps`/`bitrate` badges.
  - **Failed:** crimson alert card (`class="error-alert"`) containing the exact `error` text returned by FFmpeg/encoder.
- Empty state: "No active or failed ingests."

### 5. `components/AssetRegistryGrid.vue`
- Fetch all assets via `GET /api/assets` on mount and after SSE state changes.
- Status filter tabs: `All`, `Ready`, `Processing`, `Error` mapped to database statuses.
- High-density table columns: `display_name`, `status` chip, `duration_ms`, `rating`, `virtual_folder`, `current_path`.
- Add a small search input that filters by `display_name` and `current_path` client-side.

### 6. `App.vue` + `main.css`
- Compose the three zones:
  1. `BroadcastTopBar`
  2. `IngestQueuePanel`
  3. `AssetRegistryGrid`
- Keep the existing Configuration / Logs tabs.
- Reskin `main.css`:
  - Slate 950 / Zinc 900/800 backgrounds.
  - Explicit bordered panel boundaries (`1px solid rgba(255,255,255,0.08)`).
  - Accent Cyan/Amber/Crimson/Emerald for broadcast status semantics.
  - Progress bar styles, crimson alert box, status chips, compact table cells.
  - Respect `prefers-reduced-motion`.

## Edge cases / requirements from spec
- **Corrupted/missing duration:**
  - Backend sends `determinate = false` and `duration_ms = 0`, `percent = 0`.
  - UI renders indeterminate pulse and does **not** compute percent, so there is no `NaN` or freeze.
- **Invalid argument color parameter faults:** the exact encoder exit error is stored in `JobRecord.error`; UI displays it verbatim inside the crimson alert box.
- **Stale SSE client:** auto-reconnect and re-fetch active jobs on reconnect.
- **High-frequency progress lines:** backend throttle protects the broadcast channel and the browser from flooding.

## Validation plan
1. `cargo check` from repo root.
2. `cd web-ui && npm install && npm run build`.
3. Run the service and drop a healthy clip into the watch folder:
   - Open browser DevTools → Network → EventStream → `/api/events`.
   - Confirm `progress` events arrive with `current_time_ms`, `duration_ms`, `percent`, `speed`.
   - Confirm the bar animates smoothly and the ETA/speed text updates.
4. Trigger an FFmpeg error (e.g. drop a file that causes the invalid color parameter fault):
   - Confirm the job row switches to the crimson alert showing the exact stderr/exit error.
5. Drop a file whose metadata FFprobe cannot read (or replace probe duration with `0`):
   - Confirm `determinate = false`, the bar shows the pulse animation, and no percentage is rendered.
6. Check the Asset Registry Grid filters and confirm counts match `status` values in the database.

## Files likely to change
- `src/jobs.rs`
- `src/server.rs`
- `src/encoder.rs`
- `src/processor.rs`
- `src/db.rs`
- `web-ui/src/App.vue`
- `web-ui/src/assets/main.css`
- `web-ui/src/main.ts`
- New: `web-ui/src/composables/useEventStream.ts`
- New: `web-ui/src/components/BroadcastTopBar.vue`
- New: `web-ui/src/components/IngestQueuePanel.vue`
- New: `web-ui/src/components/AssetRegistryGrid.vue`
- New: `web-ui/src/components/ProgressBar.vue`

## Out of scope
- Rewriting the watcher/profiling logic.
- Adding a config-save API.
- Adding Tailwind.
- Switching to WebSockets.
