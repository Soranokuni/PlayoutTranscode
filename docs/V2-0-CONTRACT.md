# V2-0 Contract — Frozen V1 Wire, API, Database, Config, and Sidecar Contracts

- Baseline: commit `f1b86cc5428b02b3cb24762598b0a4909e5d599d` (`main`).
- Scope: this document freezes the observable V1 contracts. It is the reference for `tests/v1_wire_contract.rs` and the golden samples in `docs/contracts/`.
- V2 rule: **do not change any contract described here until a versioned V2 endpoint/config/schema is introduced.** Any deviation must be a deliberate, separate PR with its own migration story.

## 1. Conventions

- Base path: `/api`. JSON request/response bodies. Server binds `127.0.0.1:4353` by default (`src/config.rs:33-34`).
- CORS: permissive (`CorsLayer::permissive()`, `src/server.rs:89`).
- Non-API paths are served from `web-ui/dist` with SPA fallback to `index.html` (`src/server.rs:104-143`); on Windows the UI directory is `<exe_dir>/web-ui/dist` (`src/main.rs:136`).
- Primitive conventions across payloads: `duration_ms`, `trim_in_ms`, `trim_out_ms`, `keyframe_safe_start_ms`, `keyframe_offsets[]` are **milliseconds, absolute from the start of the media file**; `fps_num`/`fps_den` are the exact rational frame rate.

## 2. REST API surface

Status codes used: `200 OK`, `201 Created`, `404`, `409 Conflict`, `422 Unprocessable Entity`, `500` (DB failure → `{"error": "database error"}`).

| Route | Method | Request | Response |
|---|---|---|---|
| `/api/health` | GET | — | `HealthPayload` (see §3) |
| `/api/jobs` | GET | — | `JobRecord[]` (all states, newest first) |
| `/api/jobs/active` | GET | — | `JobRecord[]` (state `Processing`) |
| `/api/jobs/completed` | GET | — | `JobRecord[]` (state `Completed`, newest `finished_at` first) |
| `/api/jobs/failed` | GET | — | `JobRecord[]` (state `Failed`) |
| `/api/jobs/pending` | GET | — | `JobRecord[]` (state `Pending`, oldest first) |
| `/api/jobs/{id}/retry` | POST | `optional {"input_path": string}` | `{"success": true}`; `404` unknown job; `422` no input; `409` service not running/queue full |
| `/api/jobs/retry-failed` | POST | — | `{"submitted": n, "source_missing": n, "errors": n}` |
| `/api/config` | GET | — | `ConfigPayload` (§4) |
| `/api/config` | PUT | partial `ConfigPayload` sections | `{"success": true}`; `422` validation error; `500` save failure |
| `/api/toolchain` | GET | — | `ToolchainStatus` (§6) |
| `/api/events` | GET | — | SSE stream (§7) |
| `/api/stats` | GET | — | `{"pending","active","completed","failed","total": int}` |
| `/api/watchfolder` | GET | — | `{"watch_folder","target_folder","settle_secs","poll_secs","stable_polls_min","retry_policy","max_concurrency"}` |
| `/api/service/status` | GET | — | `{"running": bool}` |
| `/api/service/start` | POST | — | `{"success": true}` or `{"success": false, "error": "..."}` |
| `/api/service/stop` | POST | — | `{"success": true}` |
| `/api/download/start` | POST | — | `{"success": bool}` |
| `/api/download/status` | GET | — | `{"status": "idle"\|"downloading"\|"ok"\|"error: ..."}` |
| `/api/logs` | GET | — | `string[]` (in-memory ring, max 500 lines) |
| `/api/service/install` | POST | — | `{"success": true, "message"}` or `{"success": false, "error"}` |
| `/api/service/uninstall` | POST | — | `{"success": true, "message"}` or `{"success": false, "error"}` |
| `/api/assets?status=` | GET | optional `status` query | `AssetResponse[]` |
| `/api/assets/{uuid}/purge?delete_file=` | DELETE | `delete_file=true\|false` | `StructuredPurgeResult` |
| `/api/assets/{uuid}/clear-verdict` | POST | — | `{"cleared": bool, "detail": string}` |
| `/api/jobs/{id}` | DELETE | — | `{"dismissed": 1, "id": string}` |
| `/api/jobs/finished?state=` | DELETE | `failed\|completed\|cancelled`, comma-separated | `{"dismissed": n, "ids": string[]}` |
| `/api/assets/{uuid}` | GET | — | `AssetResponse`; `404` |
| `/api/assets/{uuid}/trim` | PUT | `{"trim_in_ms": int, "trim_out_ms": int}` | `AssetResponse`; `422` invalid (see §9) |
| `/api/assets/{uuid}/rating` | PUT | `{"rating": string}` | `AssetResponse`; `422` invalid rating |
| `/api/assets/{uuid}/tp` | PUT | `{"tp": string}` | `AssetResponse` |
| `/api/assets/{uuid}/rename` | PUT | `{"display_name": string}` | `AssetResponse`; `422` empty/>255 chars |
| `/api/assets/{uuid}/move` | PUT | `{"virtual_folder": string}` | `AssetResponse`; `422` invalid folder |
| `/api/assets/{uuid}/subclip` | POST | `{"display_name": string, "trim_in_ms": int, "trim_out_ms": int}` | `201` `AssetResponse` |
| `/api/assets/{uuid}/purge` | DELETE | — | `{"success": true, "purged_records": int, "file_removed": bool}`; `404` |
| `/api/assets/batch` | POST | `string[]` (uuids, max 500, no duplicates) | `{uuid: AssetResponse}` |
| `/api/folders/colors` | GET | — | `[{virtual_folder, color}]` |
| `/api/folders/colors` | PUT | `{"virtual_folder": string, "color": string}` | `200` |

Route definitions: `src/server.rs:52-84`. `PUT /trim` and `PUT /rating` (correct as implemented; README §API Surface shows POST — this document follows the code).

## 3. HealthPayload (`src/server.rs:145-155`)

```json
{ "status": "ok", "service": "PlayoutTranscode", "version": "1.0.0",
  "toolchain_ready": true, "service_running": false, "uptime_ms": 12345 }
```

## 4. Config payload — `GET/PUT /api/config` (`src/server.rs:177-244`)

```json
{
  "paths":    { "watch_folder": "D:/media/in", "target_folder": "D:/media/out" },
  "server":   { "web_port": 4353, "bind_address": "127.0.0.1" },
  "encoding": { "preset": "medium", "ffmpeg_threads": 0, "cpu_cores": 0,
                "audio_codec": "aac", "audio_bitrate": "320k", "tune": "film",
                "probesize": "500M", "analyzeduration": "500M",
                "effective_threads_per_encode": 1, "effective_total_threads": 2 },
  "profiles": { "a": { "enabled": true, "crf": 24, "maxrate": "15M", "bufsize": "16M" },
                "b": { "enabled": true, "crf": 23, "maxrate": "15M", "bufsize": "16M" },
                "c": { "enabled": true, "crf": 20, "maxrate": "5M",  "bufsize": "6M" } },
  "ingestion": { "settle_secs": 5, "poll_secs": 10, "max_concurrency": 2,
                 "stable_polls_min": 2, "retry_policy": "once",
                 "auto_retry_on_start": true, "max_attempts": 2,
                 "retry_delay_ms": 2000, "clean_source_after_success": false },
  "logging":  { "level": "info" },
  "system":   { "available_logical_cores": 8 },
  "initialized": true
}
```

- `PUT` accepts partial sections; missing fields are preserved. `initialized` is forced to `true` on any successful PUT (`src/server.rs:374`).
- `effective_threads_per_encode` / `effective_total_threads` are read-only derived values (`src/server.rs:179-181`); `available_logical_cores` is read-only (`src/server.rs:182`).
- Validation (`src/config.rs:351-401`): watch/target non-empty and watch exists; preset ∈ {ultrafast, veryfast, faster, fast, medium, slow, slower, veryslow}; audio_codec ∈ {aac, pcm_s16le, libmp3lame}; profile CRF ≤ 51; `max_concurrency ≥ 1`; `max_attempts ≥ 1`; `cpu_cores ≤ available logical cores` (oversubscription of the derived thread budget is a warning only).

## 5. AssetResponse (`src/db.rs:29-51`, `From<MediaAsset>` at `src/db.rs:53-80`)

```json
{ "uuid": "…", "playoutvue_id": "…", "current_path": "D:/media/out/videos/clip_<uuid>.mp4",
  "duration_ms": 10000, "trim_in_ms": 0, "trim_out_ms": 10000,
  "rating": "K", "tp": "None", "status": "ready",
  "display_name": "clip", "virtual_folder": "/",
  "mezzanine_ok": true, "fps": 25.0, "fps_num": 25, "fps_den": 1,
  "total_frames": 250, "gop_frames": 50, "keyframe_safe_start_ms": 0,
  "warnings": [], "keyframe_offsets": [0, 2000, 4000, 6000, 8000] }
```

Field invariants (PlayOutVue boundary — enforced by `tests/contract_boundary.rs`):

| Field | Invariant |
|---|---|
| `uuid` / `playoutvue_id` | identical strings; stable identity of the asset |
| `current_path` | final Caspar-playable file path on `ready` |
| `duration_ms` | exact, > 0 on `ready` |
| `trim_in_ms` / `trim_out_ms` | absolute ms from file start; on first publish `0` / `duration_ms` (`src/db.rs:312-313`); `trim_out > trim_in`, `trim_out ≤ duration_ms` |
| `fps_num` / `fps_den` | exact rational, both > 0 on `ready`; never float approximations (e.g. `30000/1001`, **never** `29970/1000`) |
| `mezzanine_ok` | false unless frame-accurate-safe. **A `ready` asset always has `mezzanine_ok=true`** — see §9. Earlier versions could publish `ready` with `mezzanine_ok=false`, which three consumers each resolved differently; that state is now refused by the schema. |
| `warnings` | `string[]` of documented warning codes (see encoding-profiles doc) |
| `keyframe_offsets` | `i64[]`, keyframe positions in ms |
| `retry_suppressed` | `bool`. The service has recorded that this media already failed under the current settings and will skip it rather than encode it again. Additive; see §9. |
| `status` | `processing` → `ready` \| `error` \| `missing` (see §9) |

## 6. ToolchainStatus (`src/bootstrap.rs:18-26`)

```json
{ "ffmpeg_found": true, "ffprobe_found": true,
  "ffmpeg_version": "ffmpeg version 7.1.1 …", "ffprobe_version": "ffprobe version 7.1.1 …",
  "bundled": false, "bin_dir": "D:/app/bin" }
```

## 7. SSE event stream (`GET /api/events`, `src/server.rs:412-424`)

Wire format is a JSON envelope string broadcast on a tokio broadcast channel (`src/jobs.rs:167-170`), re-emitted as an SSE `event`/`data` pair:

```json
{ "event": "<type>", "data": { "...": "..." } }
```

Event types actually emitted by the server:

| type | data payload |
|---|---|
| `job_update` | `{ "id": string, "stage": string }` — emitted on queue push (`src/processor.rs:117`) |
| `progress` | `{ "id", "percent": f64, "current_time_ms": i64, "duration_ms": i64, "determinate": bool, "fps": f64, "bitrate": string, "speed": string, "stage": string }` (`src/processor.rs:178-188`) |
| `completed` | `{ "id": string, "uuid": string }` (`src/processor.rs:353`) |
| `failed` | `{ "id": string, "error": string }` (`src/processor.rs:129,143,365`; panic path `src/processor.rs:31-34`) |

Observed delivery characteristics (frozen as behavior, not guarantees):

- Broadcast channel capacity 256 (`src/main.rs:119`); a slow consumer that falls behind is dropped with **no replay** (`msg.ok()?` at `src/server.rs:414`).
- No event IDs, no `Last-Event-ID`, no replay endpoint.
- Default axum SSE keep-alive.
- **The event name `connected` is never emitted by the server.** (Both `src/web/index.html:150` and `web-ui/src/composables/useEventStream.ts:309` listen for it; the UI relies on its 2 s polling for state recovery.)
- On connection, no state is sent; clients must fetch `/api/jobs`, `/api/assets`, `/api/stats` etc.
- Server does not send job state for `pending` transitions except via `job_update`.

## 8. JobRecord (`src/jobs.rs:16-45`)

```json
{ "id": "…", "input_path": "D:/media/in/clip.mov", "output_path": null,
  "profile": "ProfileA", "uuid": null, "state": "Pending",
  "progress": 0.0, "current_stage": "Queued", "duration_secs": 0.0,
  "error": null, "stderr_log": null, "attempt": 0,
  "created_at": "2026-08-12T10:00:00Z", "finished_at": null,
  "source_frame_count": 0, "current_frame": 0, "encode_fps": 0.0,
  "encode_bitrate": "", "encode_speed": "", "current_time_ms": 0, "duration_ms": 0 }
```

- `state` ∈ `"Pending" | "Processing" | "Completed" | "Failed"` (`src/jobs.rs:8-14`).
- `stderr_log` is present only on failure (max 50 lines, `src/encoder.rs:284`).
- `attempt` increments per manual retry, in-memory only (`src/server.rs:551,578`).
- Jobs are **in-memory only**; they do not survive a restart (see §10).

## 9. Database schema and status semantics

DB file: `<exe_dir>/media_assets.db`, WAL, pool max 5 (`src/db.rs:148-155`, `src/main.rs:110`).

`media_assets` (DDL as executed, `src/db.rs:156-178`, plus additive ALTERs at 192-216):

```sql
CREATE TABLE IF NOT EXISTS media_assets (
    uuid         TEXT PRIMARY KEY,
    fingerprint  INTEGER NOT NULL,
    current_path TEXT NOT NULL,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    trim_in_ms   INTEGER NOT NULL DEFAULT 0,
    trim_out_ms  INTEGER NOT NULL DEFAULT 0,
    rating       TEXT NOT NULL DEFAULT 'NONE',
    tp           TEXT NOT NULL DEFAULT 'None',
    status       TEXT NOT NULL DEFAULT 'processing',
    display_name TEXT NOT NULL DEFAULT '',
    virtual_folder TEXT NOT NULL DEFAULT '/',
    mezzanine_ok BOOLEAN NOT NULL DEFAULT 0,
    fps REAL NOT NULL DEFAULT 0.0,
    fps_num INTEGER NOT NULL DEFAULT 0,
    fps_den INTEGER NOT NULL DEFAULT 0,
    total_frames INTEGER NOT NULL DEFAULT 0,
    gop_frames INTEGER NOT NULL DEFAULT 0,
    keyframe_safe_start_ms INTEGER NOT NULL DEFAULT 0,
    warnings TEXT NOT NULL DEFAULT '[]',          -- JSON string[]
    keyframe_offsets_json TEXT NOT NULL DEFAULT '[]', -- JSON i64[]
    deleted_at TEXT DEFAULT NULL,
    original_virtual_folder TEXT DEFAULT NULL,
    parent_uuid TEXT DEFAULT NULL   -- the full-length asset a sub-clip was cut from
);
-- `ready` is a promise the asset can go to air now, so it may not be written
-- against a mezzanine that failed QC. Enforced for every writer, not just the
-- publish path, because the contradiction was reachable by hand.
CREATE TRIGGER IF NOT EXISTS trg_media_assets_ready_requires_mezzanine_insert
  BEFORE INSERT ON media_assets FOR EACH ROW
  WHEN NEW.status = 'ready' AND NEW.mezzanine_ok = 0
  BEGIN SELECT RAISE(ABORT, 'status ready requires mezzanine_ok'); END;
CREATE TRIGGER IF NOT EXISTS trg_media_assets_ready_requires_mezzanine_update
  BEFORE UPDATE ON media_assets FOR EACH ROW
  WHEN NEW.status = 'ready' AND NEW.mezzanine_ok = 0
  BEGIN SELECT RAISE(ABORT, 'status ready requires mezzanine_ok'); END;
CREATE TABLE IF NOT EXISTS registry_meta (   -- this registry's stable identity
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS virtual_folder_colors (
    virtual_folder TEXT PRIMARY KEY,
    color          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_assets_fingerprint ON media_assets(fingerprint);
```

- `warnings` / `keyframe_offsets_json` are JSON arrays stored as TEXT; `AssetResponse` unpacks them (`src/db.rs:55-56`).
- Status semantics:
  - `processing` — inserted before encode (`src/db.rs:273-290`); on crash **flipped to `error` at next startup** (`src/db.rs:247-257`).
  - `ready` — a successful encode that also **passed QC**. `mezzanine_ok` is
    always true here; the schema refuses any other combination.
  - `error` — an encode/probe failure (`mark_error`), **or** a mezzanine that
    encoded cleanly and then failed QC. The two are distinguishable: a QC
    failure has a real `duration_ms`, geometry, keyframes and a populated
    `warnings` array naming what it failed on, and its `current_path` points at
    a published mezzanine that is still on disk. A failed ingest has
    `duration_ms = 0` and a `current_path` that is still the source file.

    Earlier versions published a QC-failed mezzanine as `ready` with
    `mezzanine_ok=false`. The contract never defined that combination and its
    three readers each chose differently — PlayOut's v2 library mapping said
    `error`, its v1 and batch paths said `ready`, and this service's own dedupe
    read it as "not usable" and re-transcoded the source on every start. One
    asset was red in the library and green in the rundown at the same time.
  - `missing` — a `ready` asset whose mezzanine is no longer on disk. Set and
    cleared by a reconcile pass at startup and every 120 s, in both directions:
    a file that comes back (a remounted share) returns to `ready`. Counted on
    `/api/v2/diagnostics` as `metrics.missing_assets`. Alongside it,
    `metrics.unverified_keyframe_assets` counts `ready` assets published before
    T-7 whose keyframe list is still empty; they stay `ready` (W-5).

    Clients must treat `missing` as not airable. It exists because the registry
    could previously say `ready` about a file that was not there, and the first
    thing to notice was the client's pre-flight check at TAKE.
- Startup recovery (`src/db.rs:110-142`, driven from `src/service_handle.rs:165`): with `auto_retry_on_start=true`, rows in `error`/`processing` whose `current_path` (the stored source path on those states) still exists inside the watch folder are **purged** so the watcher re-queues the file; rows whose source no longer exists are purged; otherwise kept.
- Trim/rating/tp/rename/move/subclip/purge all key on `uuid`. Subclips copy
  parent metadata and share the parent's `current_path` file, and record the
  parent in `parent_uuid`.
- **Deduplication only ever considers full-length rows with a known source
  hash** (`parent_uuid IS NULL AND source_sha256 IS NOT NULL`), ordered
  `mezzanine_ok DESC, (status='ready') DESC, rowid DESC LIMIT 1`. A sub-clip
  carries its parent's fingerprint and path with a NULL `source_sha256`, so
  without the exclusion the dedupe read it as an unconfirmable legacy row and
  re-ingested the programme it was cut from.
- **Known-bad media is not re-encoded.** A QC verdict is reproducible: the
  pipeline probes a finished encode against its source under a fixed policy, so
  identical bytes under identical settings reach an identical verdict. Each
  failed row therefore stores a `qc_verdict_key` — a hash of the source SHA-256
  together with the encoding configuration and the validation policy — and a new
  ingest whose key matches is recorded as `Skipped` with the reason instead of
  being encoded again.

  This holds across two separate paths, which used to loop independently:

  - **a QC failure** (encoded cleanly, failed validation): caught by the dedupe
    before any work is done.
  - **an ingest failure** (ffmpeg or ffprobe refused the media), which the retry
    classifier judged `Permanent`: caught by the startup recovery sweep, which
    no longer purges such a row for retry. Previously it deleted the row, the
    watcher re-offered the file, and a full encode attempt was spent reaching
    the same conclusion on every restart.

  The key is what makes the skip safe to make permanent rather than a blacklist.
  It is re-examined whenever anything that could change the answer changes:

  | Change | Effect |
  |---|---|
  | Replace or re-copy the file | different source hash → judged afresh |
  | Change the encoding config or validation policy | no stored key matches → every failed asset gets another hearing |
  | Delete the asset | the verdict goes with it |
  | Nothing | skipped, with the reason on the job |

  A failure whose findings are **all** environmental (`keyframe_scan_failed` —
  ffprobe could not be run) is never treated as permanent, and neither is a
  failure with no recorded findings. A failure that never reached QC -- the
  source would not probe, or the output did not match it -- records one code
  in `warnings` beside its key, so it is recognised on re-ingest too:
  `output_duration_mismatch`, `output_missing_video`, `no_video_stream`,
  `no_audio_stream`, `unsupported_audio_channel_layout`,
  `audio_measurement_failed`, `unsupported_codec`, `source_unreadable`,
  `profile_disabled`, `source_probe_failed`, `output_validation_failed`,
  `ingest_failed_permanently`. Rows published before `qc_verdict_key`
  existed have no record of the settings that judged them, so a much narrower
  test applies to them (`duration_delta_exceeded` only) until a skip stamps the
  current key onto them.

  Held-back assets are counted on `/api/v2/diagnostics` as
  `metrics.permanently_failed_assets` (operator-cleared verdicts are not
  counted: they are no longer held back), and the startup log says how many were
  not queued and what to change to have them re-examined.
- Purge (`purge_asset_completely`, `src/db.rs:487-514`): deletes the row; deletes the physical file **only when no remaining row references the path** — subclips sharing the mezzanine are preserved.
- Folder color upsert: `ON CONFLICT(virtual_folder) DO UPDATE` (`src/db.rs:625-639`).

Validation rules at the API layer:
- `trim`: `trim_in_ms ≥ 0`; `trim_out_ms` ≤ 0 means "full duration"; `trim_out > trim_in`; `trim_out ≤ duration_ms` (`src/server.rs:716-776`).
- `rating`: `K, 8, 12, 16, 18` with optional `+`; also `NONE`/empty accepted (`src/db.rs:522-527`).
  A newly ingested asset is written as `NONE` (unrated): ingest cannot know a
  programme's suitability mark, so it does not assert one. PlayOut is where an
  operator sets it, via `PUT /api/assets/{uuid}/rating`. Subclips inherit the
  parent's rating unchanged.
- `virtual_folder`: starts with `/`, no `..`, no trailing `/` except root (`src/db.rs:557-571`).
- `display_name`: 1–255 chars (`src/server.rs:978-996`).
- subclip: same trim rules vs parent + non-empty name. If the parent is
  `mezzanine_ok` **and its parsed keyframe list is non-empty**, `trim_in_ms`
  must land within half a frame of a keyframe, else the warning
  `trim_in_not_keyframe_aligned`. A parent with no keyframes yields no warning:
  it knows nothing about alignment and must not pretend to.

  The IN point is **not** snapped. CasparCG's FFmpeg producer seeks to the
  preceding keyframe and decodes forward, so a non-keyframe IN is frame-accurate
  anyway; raising it would silently cut programme. The warning is advisory.

## 9b. Housekeeping: dismissing jobs and deleting assets

Four operations an operator reaches for, and what each one leaves behind:

| Operation | Endpoint | job record | registry row | media file |
|---|---|---|---|---|
| Dismiss a job | `DELETE /api/jobs/{id}` | **gone** | kept | kept |
| Clear finished jobs | `DELETE /api/jobs/finished?state=failed` | **gone** | kept | kept |
| Remove from library | `POST /api/assets/{uuid}/trash` | kept | recycle bin | kept |
| Delete permanently | `DELETE /api/assets/{uuid}/purge?delete_file=false` | kept | **gone** | kept |
| Delete permanently + media | `DELETE /api/assets/{uuid}/purge?delete_file=true` | kept | **gone** | **gone** |

- **Dismissing a job clears a record, never media.** It is refused with `409` for
  a job that is still pending or running — cancel it first, so an encoder is
  never left with nothing tracking it. Not behind `X-Confirm-Destructive`: it
  destroys nothing an operator could miss. Removal is broadcast as a
  `job_removed` SSE event carrying `{ "ids": [...] }`, so other clients drop the
  rows without refetching.
- **`delete_file` is per call.** Omitted, it keeps the historical behaviour, so
  existing callers are unaffected. `purge` stays behind
  `X-Confirm-Destructive` whatever the parameter says.
- **The reference count is not negotiable.** A media file that another row still
  plays — a sub-clip — is kept even under `delete_file=true`, and the response
  says so in `warnings` with `media_removed: false`. Reporting a deletion that
  did not happen would be worse than refusing.
- **`preserve_subclips_on_purge` never did anything.** It chose between two
  `PurgeMode` variants that evaluated to the identical rule. Sub-clips were
  always protected, by the reference count. The enum now means something
  (`KeepMedia` / `DeleteMediaIfUnreferenced`) and the setting is redundant.

### Overriding a skip

`POST /api/assets/{uuid}/clear-verdict` releases one asset from the
"known-bad media" rule in §9, so its source is examined again on the next
offer. It writes a sentinel rather than `NULL`, because a `NULL` key drops the
row into the *legacy* branch of the lookup — which matches on the source hash
alone and would have re-caught exactly the rows an operator is most likely to be
overruling. The override would have appeared to work and changed nothing.

`retry_suppressed` on the asset payload is the single source of truth for
"the service will skip this": it is true exactly when a live verdict is
recorded. A client must not infer it from `status` and `warnings` — neither
changes when a verdict is cleared.

## 10. Persistence boundaries (frozen)

- Jobs: in-memory only (`src/jobs.rs`). Asset rows: SQLite. Sidecar: `.<stem>_<uuid>.uuid.json` next to the mezzanine (`src/identity.rs:59-61`), written before `mark_ready` (`src/processor.rs:309-326`).
- Logs endpoint: in-memory 500-line ring (`src/service_handle.rs:53-60`).
- `config.toml`: plain TOML next to the executable (`src/config.rs:6-12`), created with defaults when absent (`src/config.rs:261-266`). `initialized: false` until wizard or first PUT.

## 11. Sidecar schema (`src/identity.rs:7-57`)

```json
{ "playoutvue_id": "…", "id": "…", "path": "D:/media/out/videos/clip_<uuid>.mp4",
  "duration_ms": 10000, "trim_in_ms": 0, "trim_out_ms": 10000,
  "fps_num": 25, "fps_den": 1, "mezzanine_ok": true,
  "filename": "clip_<uuid>.mp4", "filepath": "D:/media/out/videos/clip_<uuid>.mp4",
  "transcoded_at": "2026-08-12T10:00:00Z", "profile_used": "ProfileA",
  "original_source": { "path": "D:/media/in/clip.mov", "codec": "h264",
    "duration_secs": 10.0, "frame_count": 250, "width": 1920, "height": 1080,
    "fps": 25.0, "fps_num": 25, "fps_den": 1, "field_order": "progressive" },
  "output_media": { "duration_secs": 10.0, "frame_count": 250, "width": 1920,
    "height": 1080, "codec": "h264", "audio_codec": "aac",
    "audio_sample_rate": 48000, "audio_channels": 2, "fps_num": 25, "fps_den": 1 },
  "fps": 25.0, "total_frames": 250, "gop_frames": 50,
  "keyframe_safe_start_ms": 0, "warnings": [] }
```

## 12. Output naming (`src/processor.rs:375-404`, `src/identity.rs:144-157`)

- Layout: `<target_folder>/videos/<safe_stem>_<uuid>.mp4`.
- `safe_stem` = source file stem passed through `sanitize_filename`: ASCII alphanumerics, `-`, `_`, `.` kept; all other characters (incl. spaces and non-ASCII) become `_`; leading/trailing `_` trimmed; lowercased (`src/identity.rs:144-157`).
- UUID is v4, generated at job start (`src/processor.rs:90`). Collision avoidance is existence-check plus re-roll with a fresh UUID (up to 3 tries, then timestamp suffix) (`src/processor.rs:384-403`).

## 13. Out of scope for this document (future PRs)

Durable jobs, per-job cancellation, versioned `/api/v2` routes, event IDs/replay, loudness/audio policy, atomic output publication (`.tmp` → rename), schema versioning, and toolchain integrity — all deferred per the V2 roadmap; none may silently change the contracts above.