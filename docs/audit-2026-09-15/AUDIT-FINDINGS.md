# PlayoutTranscode — Reliability & Security Audit Findings (2026-09-15)

Auditor scope: full read of `src/*.rs`, `tests/*.rs`, `web-ui/src/composables/useEventStream.ts`, installer scripts, `Cargo.toml`/`Cargo.lock`, and the PlayOut client handoff `PLAYOUTTRANSCODE-INTERFACE-CHANGES.md`.
Baseline at audit time: `main` @ `63e5742`, `cargo test` → 136 passed / 0 failed (111 unit, 10 contract, 5 chaos, 10 wire).

Companion document: `REMEDIATION-PLAN.md` (step-by-step implementation plan keyed to the finding IDs below).

Severity scale
- **Critical** — remotely triggerable data loss / arbitrary file read / code execution path, or a defect that makes a shipped feature not work at all.
- **High** — exploitable from the operator's browser or LAN, or a reliability defect that silently corrupts state or loses media.
- **Medium** — needs unusual conditions, or degrades service under load / misconfiguration.
- **Low** — hygiene, information leak, or defence-in-depth gap.

Threat model used: the HTTP API is unauthenticated by design (PlayOut has no credential), `bind_address` is operator-configurable and the README documents `0.0.0.0`, CORS is `permissive()`, and the process is intended to run as a Windows service (LocalSystem by default via `sc create`). "Attacker" therefore includes (a) any process or user on the operator PC, (b) any web page the operator opens in a browser on that PC, and (c) any LAN host when bound to a non-loopback address.

---

## A. Security

### F-01 · Critical · Arbitrary file read through the SPA fallback
- **Where:** `src/server.rs:163-217` (`serve_spa`).
- **What:** `state.web_ui_dir.join(uri.path().trim_start_matches('/'))`. Axum does not normalise dot segments, so `GET /../config.toml` (sent with `curl --path-as-is`, Python `requests`, or a raw socket) resolves to `<exe_dir>/config.toml`, and `GET /../media_assets.db` returns the whole asset registry. On Windows, `PathBuf::join` with an absolute component *replaces* the base, so `GET /C:/Windows/win.ini` reads any file the service account can read. Running as LocalSystem, that is every file on the machine.
- **Impact:** Config exfiltration (watch/target paths), full DB download, arbitrary file disclosure. The 404 branch also leaks `web_ui_dir` on disk.

### F-02 · Critical · No authentication + `CorsLayer::permissive()` = cross-site control of the service
- **Where:** `src/server.rs:148`; every mutating route in the `api` and `api_v2` routers.
- **What:** Any web page open in the operator's browser can issue `PUT /api/config`, `DELETE /api/recycle-bin/purge`, `POST /api/service/install`, `POST /api/jobs/retry-failed`, etc. Permissive CORS answers the preflight for JSON `PUT`/`DELETE`, so the browser sends the request with no user interaction. When `bind_address` is not loopback, the same is true for every LAN host with no browser needed.
- **Impact:** Library purge, config takeover (see F-03), UAC prompt spam (F-04), service stop, with zero credentials. PlayOut's own handoff explicitly flags this as a server-side exposure it cannot mitigate.

### F-03 · High · `PUT /api/config` accepts arbitrary paths, persists before validating, and takes effect on the next start
- **Where:** `src/server.rs:532-694` (`put_config`); `src/config.rs:698-807` (`validate`).
- **What:** `watch_folder`/`target_folder` are stored verbatim and written to disk at line 674 *before* `validate()` runs at line 685, so an invalid config is persisted and simply disables auto-start at next boot. `validate()` itself has a side effect: `fs::create_dir_all(target_folder)` creates arbitrary directories. `POST /service/stop` + `POST /service/start` then applies the new paths immediately. Setting `watch_folder = C:\Users` with `clean_source_after_success = true` makes the service transcode and then delete every supported video file under user profiles (the cleanup guard only checks "inside watch root", which is now satisfied).
- **Impact:** Remote-driven mass media deletion and CPU exhaustion. Also no schema for `encoding.tune`, `maxrate`, `bufsize`, `probesize`, `analyzeduration`, `audio_bitrate` → one bad string breaks every subsequent encode.

### F-04 · High · Service install/uninstall and FFmpeg download are HTTP-triggerable privileged actions
- **Where:** `src/server.rs:950-1012` (`post_install_service`, `post_uninstall_service`); `src/bootstrap.rs:115-193` (`download_ffmpeg`).
- **What:** An unauthenticated `POST` spawns `powershell … Start-Process sc.exe -Verb RunAs`, popping a UAC prompt on the console session and, if approved, registering a **LocalSystem** service (no `obj=`) whose `binPath` points at whatever directory the exe currently lives in. `download_ffmpeg` fetches `ffmpeg-release-essentials.zip` over HTTPS with **no checksum or signature pin** and writes executables into `<exe_dir>/bin`, which the service later executes. `resolve_tool` (`bootstrap.rs:54-68`) also falls back to a `PATH` search, so a writable directory earlier in `PATH` allows binary planting.
- **Impact:** Privilege escalation path when the exe directory is user-writable (any local user can replace `bin/ffmpeg.exe` and get LocalSystem execution); supply-chain exposure; social-engineering UAC prompt from a web page (F-02).

### F-05 · High · SQL `LIKE` wildcard injection in virtual-folder operations
- **Where:** `src/db.rs:654, 729, 742, 917` (`trash_folder`, `restore_folder`, `purge_folder_with_context`); validator `src/db.rs:1133-1147` (`is_valid_virtual_folder`).
- **What:** `folder_path` is bound as `format!("{}/%", norm)` with no `ESCAPE`, and the validator does not reject `%` or `_`. `POST /api/folders/trash {"folder_path":"/%"}` matches every asset in any sub-folder; `DELETE /api/folders/purge {"folder_path":"/%"}` then deletes their media and sidecars. A legitimate folder named `/promo_2026` also trashes `/promoX2026`.
- **Impact:** Bulk data loss from a single request, and incorrect scope on normal folder names. This is exactly item 3.2 in the PlayOut handoff ("treat `folder_path` as untrusted").

### F-06 · Medium · Unbounded, expensive, process-spawning read endpoints
- **Where:** `src/server.rs:290-334` (`get_diagnostics` → `PRAGMA integrity_check` + `audit_toolchain()`), `src/server.rs:696-699` (`get_toolchain_status` → `audit_toolchain()` spawns `ffmpeg -version` and `ffprobe -version`), `src/server.rs:1049-1069` (`list_assets`, no limit, includes full `keyframe_offsets`), `src/db.rs:1788-1848` and `1977-2037` (`query_db_assets`/`query_db_jobs` load whole tables then filter in memory; `DbAssetSummary::from_asset` stats the sidecar path per row).
- **What:** The bundled web UI polls `/api/toolchain` every 2 s (`useEventStream.ts` `fetchAll`), i.e. two process spawns per second per open tab. `list_assets` grows without bound; a 2-hour programme at 2 s GOP is ~3 600 offsets, so a few thousand assets exceed PlayOut's new 16 MiB body cap and listing fails outright.
- **Impact:** Trivial DoS, wasted CPU on the encode host, and a foreseeable hard failure of PlayOut library listing at scale.

### F-07 · Medium · Blocking I/O and process execution inside async handlers
- **Where:** `src/server.rs` — `std::process::Command` (950-1012), `audit_toolchain()` (291, 697, 791), `path.exists()` (868, 918, 1635), `config.save_to` under a held mutex (674), `identity::build_sidecar_from_db_asset` (1644); `src/db.rs:1742-1747` (`exists()` per asset row); `src/watcher.rs:203` (`WalkDir` full tree walk inside an async task every `poll_secs`).
- **Impact:** Tokio worker starvation; a slow network share or hung `ffmpeg -version` stalls every request including `/api/health`, which PlayOut polls every 5 s and treats as the liveness signal.

### F-08 · Medium · Missing input validation on several mutating routes
- **Where:** `put_tp` (`server.rs:1233`, any string), `put_rating` (`is_valid_rating` accepts an unbounded payload after the first `|`), `put_folder_color` (`server.rs:1997`, any string, rendered into a `style` binding in `DbViewer.vue:308`), `post_retry_job` `input_path` (`server.rs:857`, any path — `exists()` on a UNC path triggers outbound SMB/NTLM auth), `{uuid}`/`{id}` path segments never checked for canonical form (requested by PlayOut handoff §3.1), `post_restore_asset` silently falls back to `/` on an invalid folder instead of 422.
- **Impact:** Stored garbage returned to PlayOut, CSS injection in the DB viewer, NTLM hash leakage, and no server-side guarantee behind PlayOut's client-side UUID validation.

### F-09 · Low · Error and diagnostic bodies leak internal paths
- **Where:** `serve_spa` 404 text (`server.rs:206`), `post_regenerate_sidecar` (`path`, `sidecar_path`), `put_config` save error, `post_install_service` raw PowerShell stderr, `get_diagnostics.config_summary`.
- **Impact:** PlayOut writes error bodies verbatim into its diagnostics log (handoff §3.6).

### F-10 · Low · No HTTP hardening layers
- **Where:** `src/server.rs:144-149`.
- **What:** No request timeout, no concurrency limit, no request tracing, default 2 MiB JSON body limit relied on implicitly, no graceful shutdown on `axum::serve`.

---

## B. Reliability

### F-11 · Critical · "Install as Windows Service" produces a service that cannot start
- **Where:** `src/main.rs` (no `StartServiceCtrlDispatcher`/`windows-service` integration anywhere in `src/`), `src/server.rs:950-989`, `scripts/build-installer.ps1:87-90`, `installer/PlayoutTranscode.nsi`.
- **What:** The binary is a plain console program. `sc create … binPath="PlayoutTranscode.exe run --config …"` registers it, but the Service Control Manager kills any process that does not report `SERVICE_RUNNING` within 30 s (error 1053). Both the HTTP installer and the shipped NSIS/`install.ps1` path do this.
- **Impact:** The documented production deployment mode does not work; operators fall back to running it interactively, where a logoff kills ingest.

### F-12 · High · Cancelling one job kills every running FFmpeg
- **Where:** `src/processor.rs:840-874` (heartbeat thread), `src/encoder.rs:156-161`.
- **What:** `active_pids` is one shared list for the whole service. The heartbeat thread for job A, on seeing `cancel_requested`, iterates `hb_pids` and `taskkill`s **all** PIDs, including jobs B and C. Those jobs then fail with "ffmpeg exited with code …", classified retryable, and restart from scratch.
- **Impact:** Operator cancels one clip, loses minutes of progress on every concurrent encode.

### F-13 · High · Durable job persistence is unordered, fire-and-forget, and creates ghost jobs
- **Where:** `src/jobs.rs:307-326` (`persist_job`), `src/jobs.rs:367-375` (`update`), `src/processor.rs:789-831` (progress thread calls `qc.update` on every stderr progress line), `src/server.rs:844-895` (`post_retry_job`), `src/db.rs:1505-1552` (`recover_stale_jobs`), `src/db.rs:1387` (`claim_next_job` is `#[allow(dead_code)]`).
- **What:**
  1. Every `update`/`transition` spawns an independent upsert. Two rapid writes can land out of order, so the DB can end with "Encoding 97 %" after the in-memory job is "Completed".
  2. From the std progress thread there is no Tokio handle, so each progress line spawns a **new OS thread and a new runtime** to write one row.
  3. Nothing consumes the durable queue: `recover_stale_jobs` re-queues `Processing` rows to `Pending`, but only the filesystem watcher feeds `dispatch_one`. Recovered rows stay `Pending` forever, and `post_retry_job` marks the old record `Queued` while `process_file_inner` always creates a brand-new `JobRecord` — so every retry leaves a permanent ghost.
- **Impact:** `/api/jobs`, `/api/stats`, and the DB viewer show phantom pending work; write amplification on SQLite during encodes; misleading crash-recovery logs.

### F-14 · High · Watch/target overlap is not validated → runaway re-ingest loop
- **Where:** `src/config.rs:698-807` (`validate`), `src/watcher.rs:61-91` (`collect_candidates` only skips `.tmp_`/hidden/json names).
- **What:** If `target_folder` is inside `watch_folder` (or equal), every published `name_<uuid>.mp4` is picked up as new source, transcoded again, published again, forever. Nothing in `validate()` or `start_processing_loop` prevents it.
- **Impact:** Disk fill and 100 % CPU from a one-line misconfiguration; recoverable only by stopping the service manually.

### F-15 · High · Sampled fingerprint dedup silently skips files and destroys subclips
- **Where:** `src/fingerprint.rs:8-53` (SHA-256 of size + first/middle/last 64 KiB, despite the `fnv1a64` name), `src/processor.rs:573-596`.
- **What:** Two different deliverables with identical size and identical head/mid/tail bytes (CBR promos from the same template, same duration, common slate/black) are treated as duplicates: the second is skipped with only an `info` log — no job record, no failed event, nothing in the UI. When the match is "not usable", `purge_rows_by_fingerprint` deletes **every** row with that fingerprint, including operator-created virtual subclips (which inherit the parent fingerprint in `create_subclip`).
- **Impact:** Silent loss of ingest requests; silent loss of operator trims/subclips.

### F-16 · High · No persistent logging; `logging.log_file` is dead config
- **Where:** `src/logging.rs:1-17` (stdout only), `src/config.rs:252-274` (`log_file` parsed, never read), `src/service_handle.rs` (`log_lines` ring of 500 in memory).
- **Impact:** When run headless/as a service, stdout goes nowhere. After an incident there is no log to read; `/api/logs` holds only the last 500 UI lines.

### F-17 · Medium · Service stop/start race allows overlapping processing loops
- **Where:** `src/service_handle.rs:117-255` (`start_processing_loop`), `:300-310` (`stop_processing`).
- **What:** `stop_processing` flips `running=false` immediately; the old worker thread keeps draining `spawn_blocking` tasks until its runtime drops. `start_processing_loop` only checks the flag, so a fast stop/start (or a crash in `Runtime::new().unwrap()` at line 169, which leaves `running=true` forever) yields two watchers on the same folder and duplicate jobs for the same source. The `Stop` command channel has capacity 1 and uses `try_send`.
- **Impact:** Duplicate assets, or a permanently "running" dead service that refuses restart.

### F-18 · Medium · Publish-path error handling ignores DB failures
- **Where:** `src/processor.rs:613-625` (`insert_processing` error logged then flow continues), `:1112-1126` (`let _ = handle.block_on(db::mark_ready(...))`).
- **What:** If the insert fails (locked DB, disk full) the file is still transcoded and published; `mark_ready` then updates zero rows and its own error is discarded. The mezzanine exists on disk with no `ready` row → PlayOut never sees it, next startup marks nothing, and the orphan file is never cleaned.
- **Impact:** Violates the AGENTS.md "Bold Rule" from the other direction: published media invisible to playout, no alert.

### F-19 · Medium · Purge file-deletion guard fails open when paths do not canonicalize
- **Where:** `src/db.rs:768-802` (`validate_purge_path`), `src/server.rs:1585-1596` (passes `None` when config path is empty).
- **What:** The "inside target dir" and "not inside watch dir" checks run only if **both** sides canonicalize. If `target_folder` is empty or temporarily unreachable, the check is skipped and `remove_file` proceeds on whatever `current_path` holds. For `processing`/`error` rows `current_path` **is the source file in the watch folder**.
- **Impact:** Source media deletion via purge of a failed asset under misconfiguration.

### F-20 · Medium · SSE lag and no resync signal
- **Where:** `src/main.rs:123` (broadcast capacity 256), `src/server.rs:707-721` (`msg.ok()?` swallows `Lagged`), `useEventStream.ts` listens for a `connected` event the server never emits.
- **Impact:** A slow browser tab misses `completed`/`failed` events silently; UI state drifts until the 2 s poll catches up. Server never tells clients they lagged.

### F-21 · Medium · Progress parsing depends on `\n`-terminated stats lines
- **Where:** `src/encoder.rs:189-207` (`BufRead::read_line`), `src/profiles.rs` (`-stats`, `-loglevel info`).
- **What:** FFmpeg terminates interim `frame=… time=…` reports with `\r` when stderr is not a TTY. `read_line` only returns on `\n`, so progress can arrive late or as one concatenated line where `captures()` matches the *first* occurrence. Needs runtime verification on the shipped FFmpeg build; the robust fix (`-progress pipe:N -nostats`) is cheap either way.
- **Impact:** Frozen or wrong progress in UI and in `transcode_jobs`.

### F-22 · Medium · Disk preflight uses a fixed 500 MB threshold
- **Where:** `src/processor.rs:740-759`.
- **What:** A 1-hour HD mezzanine at `maxrate 15M` is ~6.75 GB. The check passes with 501 MB free, the encode fails mid-way with a disk-full error that is classified `Permanent`, and the asset is marked `error`.
- **Impact:** Predictable failures on long-form content that the preflight was meant to catch.

### F-23 · Medium · Saved config vs running config divergence is invisible
- **Where:** `src/server.rs:358-440` (`get_config` shows saved state), `src/service_handle.rs:117` (worker uses a clone taken at start).
- **Impact:** Operator changes `max_concurrency`, sees it echoed by `GET /config`, but the running loop still uses the old value until stop/start. No "restart required" indicator.

### F-24 · Medium · FFmpeg downloader: 30 s default timeout, whole archive in RAM, panic leaves `downloading` stuck
- **Where:** `src/bootstrap.rs:129-136` (`reqwest::blocking::get` default client), `:168` (`.unwrap()` on fallback `File::create`), `src/service_handle.rs:312-331` (`trigger_download` refuses while status is `Some`).
- **Impact:** Download fails on slow links; a panic in the worker thread leaves the UI button disabled until process restart.

### F-25 · Medium · README config schema does not match the code
- **Where:** `README.md:174-195` documents `[server] port`, `[transcode]`, `[audio]`, `paths.database_path`; the code reads `[server] web_port`, `[ingestion]`, `[audio_policy]`, and puts the DB next to the exe. Unknown keys are silently ignored (`config.rs` test `test_unknown_future_fields_ignored`).
- **Impact:** Operators believe they configured `max_concurrency`/loudness/bind port and get defaults.

### F-26 · Low · Silent drops in the processor
- **Where:** `src/processor.rs:550-553` (path outside watch root → `warn!` and return), `:565-571` (fingerprint error → `error!` and return), `:573-586` (dedup skip).
- **Impact:** No job record and no SSE event for three failure classes; UI and PlayOut have nothing to show.

### F-27 · Low · Sidecar regeneration fabricates metadata
- **Where:** `src/identity.rs:388-411` (`build_sidecar_from_db_asset` hard-codes 1920×1080, `h264`, `aac`, 48 kHz stereo, `progressive`, `profile_used = "rebuilt_from_db"`).
- **Impact:** A regenerated sidecar for a Profile C (pillarboxed SD) or PCM asset lies about the media.

### F-28 · Low · Sidecar location resolution depends on filesystem state
- **Where:** `src/identity.rs:268-301` (`sidecar_path_for` picks `sidecars/` sibling vs legacy adjacent based on `exists()`).
- **Impact:** Two sidecars with different content can coexist; purge removes one and PlayOut may read the stale other.

### F-29 · Low · Dead / misleading configuration surface
- `toolchain_policy.ffmpeg_path` / `ffprobe_path` are parsed and returned by `GET /config` but `audit_toolchain` never reads them.
- `validation_policy.*` (`enforce_closed_gop`, `max_duration_delta_ms`, `strict_ready_blocking`) are parsed but `run_qc_evaluation`/`classify_probe_match` ignore them (hard-coded 1200 ms tolerance, always-enforced checks).
- `ingestion.retry_policy` string is never consulted.
- `storage_policy.atomic_publication` is never consulted (publication is always atomic).

### F-30 · Low · Process lifecycle gaps
- `run_headless` and the worker thread both `Runtime::new().unwrap()`.
- Ctrl-C path kills FFmpeg and returns immediately; in-flight `persist_job` writes and the SQLite pool are dropped mid-write; no `with_graceful_shutdown` on the listener.
- No single-instance guard; two processes on the same watch folder double-process.
- `[profile.release] strip = true` with no separate symbols → panics in the field have no usable backtrace.
- No CI, no `cargo audit`/`cargo deny`, no `clippy -D warnings` gate. Dependency versions are current as of the lockfile (axum 0.8.9, tokio 1.52.3, sqlx 0.8.6, reqwest 0.12.28, zip 2.4.2).

### F-31 · Low · Test coverage gaps that matter for the above
- `tests/v1_wire_contract.rs` "live" tests build a **stub** router with canned JSON; not one `server.rs` handler is exercised end-to-end.
- No test for `serve_spa`, CORS, folder LIKE scoping, cancel isolation, stop/start, persistence ordering, watch/target overlap, or config validation of ffmpeg strings.

---

## C. Things that are in good shape (keep as-is)
- Staging → validate → atomic rename → sidecar → `mark_ready` ordering (`processor.rs:1027-1126`) honours the AGENTS.md publication invariants.
- `validate_and_cleanup_source` (`processor.rs:113-387`) is genuinely fail-closed and well tested.
- Reference-counted purge keeps a mezzanine alive while subclips point at it (`db.rs:805-899`).
- All SQL uses bound parameters; the only `format!` SQL interpolates constant column lists/table names.
- Panic containment around each job (`catch_unwind`) and `panic = "unwind"` in the release profile.
- `/api/health` is cheap and side-effect free, satisfying PlayOut handoff §3.5.
- Filename sanitisation (Greek transliteration, ASCII-only stems) removes path-separator and shell-metachar risk from output names.

---

## D. Mapping to the PlayOut handoff (`PLAYOUTTRANSCODE-INTERFACE-CHANGES.md` §3)

| PlayOut ask | Finding | Plan step |
|---|---|---|
| 1. Server-side id validation on `{uuid}` routes and batch body | F-08 | T1-3 |
| 2. Treat `folder_path` as untrusted | F-05, F-08 | T0-3 |
| 3. Bind / CORS / auth | F-02 | T0-2, T1-1 |
| 4. Response size (16 MiB cap) | F-06 | T2-7 |
| 5. Health endpoint cheap | OK | — (protect with T1-6 from starvation) |
| 6. Error bodies must not leak paths | F-09 | T1-4 |
| 7. Purge has no second factor | F-02 | T1-5 |
