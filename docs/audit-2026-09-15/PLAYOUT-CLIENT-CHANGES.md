# PlayOut client changes required by the PlayoutTranscode hardening

Audience: the PlayOut (Tauri/Vue) client team.
Source: `docs/audit-2026-09-15/AUDIT-FINDINGS.md` and the remediation work on
`PlayoutTranscode` `main`.

Nothing in this document has been implemented in the PlayOut repository — the
ingest service team does not touch it. Each section states what PlayOut must
send, why, and what happens if it does not.

---

## 1. Send an API token — **required when the operator configures one**

**Status in PlayoutTranscode:** shipped (T1-1).

### What changed

`PlayoutTranscode` now supports `server.api_token` in `config.toml`. When it is
set, every request under `/api/**` must carry the token. When it is empty the
service behaves exactly as before, but it may then only bind to loopback —
`bind_address = "0.0.0.0"` is refused at startup unless a token is configured.

The operator generates one with:

```
PlayoutTranscode gen-token
```

which writes it to `config.toml` and prints it once.

### What PlayOut must do

1. Add `settings.ingestorApiToken: string` (default empty) to the settings
   model and a field for it in the ingest settings UI. Treat it as a secret:
   mask it in the UI and never write it to the diagnostics log.
2. In `src-tauri/src/ingestor_api.rs`, on the shared `reqwest` client, send the
   token on every request:

   ```rust
   // Either header is accepted; pick one.
   req = req.header("X-Api-Token", &settings.ingestor_api_token);
   // or: req.bearer_auth(&settings.ingestor_api_token)
   ```

   The handoff (`PLAYOUTTRANSCODE-INTERFACE-CHANGES.md` §1) already names that
   file as the single place the client talks to the service, so this is one
   change, not one per call site.
3. **Do not** send the token to the health endpoints — it is harmless if you
   do, but they are deliberately exempt so the status light works before the
   operator has entered a token.

### Endpoints exempt from the token

| Endpoint | Why |
|---|---|
| `GET /api/health` | PlayOut's 5 s liveness poll must work before configuration |
| `GET /api/v2/health` | same |

Static files (the bundled web UI) are also exempt.

### Failure mode if PlayOut does not implement this

Any request other than health returns:

```
401 Unauthorized
{"error":"unauthorized"}
```

This only happens on installations where the operator has set a token. An
installation left on the default (loopback, no token) is unaffected.

---

## 2. Confirmation header on destructive operations — **required**

**Status in PlayoutTranscode:** shipped (T1-5).

Destructive routes require `X-Confirm-Destructive: yes` and otherwise return
`428 Precondition Required` with `{"error":"confirmation_required"}`. The
header value is matched case-insensitively and surrounding whitespace is
ignored; any other value (including `no` or an empty string) does not arm the
operation. The affected calls are:

- `DELETE /api/assets/{uuid}/purge`
- `DELETE /api/folders/purge`
- `DELETE /api/recycle-bin/purge`
- `POST /api/recycle-bin/auto-purge`
- `POST /api/folders/trash`
- `POST /api/jobs/retry-failed`
- `PUT /api/config`
- `POST /api/service/stop`

Both the v1 (`/api/...`) and v2 (`/api/v2/...`) forms of these paths are
gated.

PlayOut already prompts the operator natively before these, so this is a
one-line addition per call in `ingestor_api.rs`:

```rust
req = req.header("X-Confirm-Destructive", "yes");
```

Each destructive request is also written to the service log on the `audit`
target with the method, path, caller address and resulting status.

**Order of checks:** authentication runs first. A confirmed request without a
valid token returns 401, not 428, and is not executed.

### Failure mode if PlayOut does not implement this

Purge, trash, empty-recycle-bin, retry-all, config write and service stop all
fail with 428 and do nothing. Reads and reversible operations (restore,
single-asset trash, rename, move, service start) are unaffected.

---

## 3. Open question for the PlayOut team: what values does `tp` take?

**Status: the provisional rule has shipped** (T1-3). It can still be tightened
once you answer, and tightening it may reject values PlayOut sends today, so
please read this.

`tp` is written by PlayOut's `ComplianceModule.vue`. The service used to accept
any string of any length; it now enforces:

```
^[A-Za-z0-9 _\-|:\[\]{}",.]{0,512}$
```

A value outside that set returns `422 {"error":"invalid tp"}`. Note this
excludes newlines, tabs, NUL and non-ASCII characters (including Greek), so if
PlayOut writes any of those the call will now fail.

**Please confirm:**

1. Is `tp` a fixed enumeration (e.g. `TP`, `SHOW`, `NONE`), or free text?
2. If it is structured (the `|`-delimited form seen in `rating`), what is the
   grammar?
3. What is the realistic maximum length?
4. Does it ever carry non-ASCII text?

If it is an enumeration we will validate against the list instead, which is
strictly better.

### Related: `rating` is now bounded

`PUT /api/assets/{uuid}/rating` caps the whole value at 4 KiB and rejects
control characters. The broadcast-metadata tail after the first `|` is still
free text, **except** that a tail beginning with `[` or `{` must be valid JSON.
PlayOut already sends JSON there, so this should be a no-op.

### Related: `folder_color` is now an allow-list

`PUT /api/folders/colors` accepts `#rrggbb` or one of: `default`, `red`,
`orange`, `yellow`, `green`, `teal`, `blue`, `purple`, `pink`, `grey`, `gray`.
Anything else is 422. This closes a CSS-injection path in the DB viewer.

### Related: asset and job ids are validated server-side

Every `{uuid}`/`{id}` path segment, and every element of the
`POST /api/assets/batch` body, must be a canonical hyphenated UUID. Anything
else returns `422 {"error":"invalid asset id"}`. This is the server-side
guarantee behind PlayOut's client-side validation (handoff §3.1).

---

## 4. Already-fixed items from the handoff, for information

These needed no PlayOut change; they are listed so the client team knows the
behaviour is now enforced server-side.

| Handoff item | Now |
|---|---|
| §3.2 treat `folder_path` as untrusted | `folder_path` is validated and `LIKE` wildcards are escaped. `/%` returns 422 instead of matching the whole library. |
| §3.3 bind / CORS | `bind_address` must be loopback unless a token is set. CORS is a loopback allow-list. A non-loopback `Host` header returns 421. PlayOut's `reqwest` client sends a correct `Host`, so it is unaffected. |
| §3.5 health endpoint cheap | Unchanged and still side-effect free. |
| §3.6 error bodies must not leak paths | Done. No response body carries a filesystem path or OS error string; sidecar-regen failures return `mezzanine_missing` / `sidecar_write_failed`, config-save failures return `config_save_failed`, and the SPA 404 is a bare message. Details go to the service log only. |
| §3.7 purge has no second factor | Done - see section 2 above. |

### One behaviour change worth noting

`POST /api/assets/{uuid}/restore` with an invalid `target_folder` now returns
**422** instead of silently restoring the asset to `/`. If PlayOut relied on
the silent fallback, it must handle the 422 — but sending a valid folder path
is the correct fix.

### One more, about purge

Purging an asset whose status is not `ready` now deletes the registry row but
**retains the file on disk**, with a warning in the result. For a `processing`
or `error` row the stored path is still the *source* file in the watch folder,
and deleting it was a real data-loss path. If PlayOut showed "media removed"
based on the row disappearing, read `media_removed` from the response instead.

---

## 5. Deployment changes — **no client code change, but read this**

**Status in PlayoutTranscode:** shipped (T2-2, T2-1, T2-3).

Nothing here changes the wire contract. It changes where the service lives and
how it is started, which matters for support calls and for the upgrade.

### 5.1 The service now works as a Windows service

The documented production deployment mode did not work before. The installer
registered `PlayoutTranscode run`, which is a console program: the Service
Control Manager waited for a status report that never arrived and failed the
start with **error 1053** after 30 seconds. The only way to keep ingest running
was to leave someone logged in with a console window open, and a logoff killed
it.

The installer now registers a real SCM entry point (`service-run`) running as
`NT AUTHORITY\LocalService`, with auto-restart on crash. A service stop drains
in-flight HTTP requests, stops the watcher, kills any running FFmpeg child and
closes the database before reporting `STOPPED`.

**What this means for PlayOut:** the ingest service is now expected to be up
whether or not anyone is logged in. If PlayOut's status light was previously
red after a server reboot until someone logged in and started the app, that
should stop happening. No client change.

> **Verified on a real host**, 2026-09-16, via `scripts\verify-service.ps1`:
> the SCM reports `RUNNING`, `/api/health` answers 200 while under the SCM,
> `sc stop` reaches `STOPPED`, no `PlayoutTranscode.exe` or `ffmpeg.exe`
> survives the stop, and `LocalService` successfully wrote the registry into
> its data directory.

### 5.2 Config, database and logs moved out of the install directory

`LocalService` cannot write under `Program Files`, so all mutable state moved
to a **data directory**, resolved at startup as:

| Order | Source | Result |
|---|---|---|
| 1 | `--data-dir <PATH>` | that path |
| 2 | `PLAYOUT_TRANSCODE_DATA` | that path |
| 3 | exe under a `Program Files` tree | `%ProgramData%\PlayoutTranscode` |
| 4 | anything else | next to the exe (unchanged, portable/dev builds) |

`config.toml`, `media_assets.db`, `logs\` and any downloaded FFmpeg all live
there.

**Upgrade note worth telling operators:** on an installed build, a `config.toml`
that was previously edited next to the executable is **no longer read**. It must
be moved to `%ProgramData%\PlayoutTranscode\config.toml`. A portable build is
unaffected.

`GET /api/diagnostics` gained an additive field so support can ask for the right
files without guessing:

```json
{ "system": { "os": "windows", "arch": "x86_64", "logical_cores": 16,
              "data_dir": "C:\\ProgramData\\PlayoutTranscode" } }
```

No existing field changed. If PlayOut surfaces a diagnostics panel, showing
`system.data_dir` there would save a support round-trip.

### 5.3 There are now real logs after an incident

Previously a headless install discarded every log line. There are now four
sinks:

| Sink | Format | Contents |
|---|---|---|
| stdout | pretty | everything at `logging.level` (interactive runs only) |
| `<data_dir>\logs\transcode.log.<date>` | JSON, rotated daily | everything at `logging.level` |
| `<data_dir>\logs\audit.log.<date>` | JSON, rotated daily | destructive operations only |
| the web UI log panel | plain text | `WARN`, `ERROR` and every audit record |

`logging.retain_days` (default 14) prunes older files at startup.

The audit log records every destructive call PlayOut makes — the ones listed in
section 2 — with the method, path, **caller address** and resulting status:

```json
{"timestamp":"2026-09-16T18:21:41.252824Z","level":"WARN",
 "fields":{"message":"destructive operation","op":"DELETE",
 "path":"/api/recycle-bin/purge","remote_addr":"127.0.0.1:58051","status":200},
 "target":"audit"}
```

Two consequences for PlayOut:

1. **Purges are now attributable.** "Who emptied the recycle bin?" has an
   answer. If several clients share one service, the address in that line is
   PlayOut's.
2. The web UI log panel now shows service-wide `WARN`/`ERROR`, not just the
   handful of hand-written lines it used to. An operator looking at the UI will
   see failures PlayOut reported as generic errors, which may change what they
   report to you.

---

## 6. Job records: retries no longer leave ghosts — **check your job handling**

**Status in PlayoutTranscode:** shipped (T2-4).

Nothing renamed, nothing removed. But what `GET /api/jobs`, `/api/jobs/active`,
`/api/jobs/pending` and `/api/stats` return has changed in ways a client can
notice.

### 6.1 A retry reuses the job id instead of creating a second job

Before: `POST /api/jobs/{id}/retry` marked the old record re-queued and the
service then created a **brand-new** job for the same file. The old record
stayed `Pending` forever. Every retry permanently added one phantom pending job
to the list, and `/api/stats` counted it.

Now: the retry reuses the same record. The `id` you retried is the `id` that
runs, and `created_at` is preserved.

**What PlayOut should check:**

- If anything keys off "a retry produces a new job id", it will now see the
  same id transition `Pending → Processing → …` again. This is the intended
  behaviour and simpler to follow, but it is a change.
- If PlayOut has been filtering or de-duplicating the job list to hide the
  phantom pending entries, that workaround can go — and if it de-duplicates by
  `input_path`, it may now be hiding the real job.
- Pending counts will drop on upgrade for any installation that has been
  retrying jobs. That is the phantom work disappearing, not lost work.

### 6.2 Startup can now move a Pending job to Failed

At startup, a job left `Pending` whose source file no longer exists is failed
with:

```json
{ "state": "Failed", "phase": "failed",
  "error_category": "source_missing_on_recovery",
  "error": "Source file no longer exists" }
```

Previously those sat `Pending` forever. A job whose source still exists is left
`Pending` and picked up normally.

### 6.3 A retry that hits an already-ingested asset reports Failed

If a retry turns out to be a duplicate of an asset already ingested and valid,
the job ends as:

```json
{ "state": "Failed", "phase": "failed",
  "error_category": "duplicate_skipped",
  "error": "Skipped: an identical asset is already ingested" }
```

This is **not** an error condition — nothing went wrong and no work was needed.
It is reported as `Failed` because the job phase machine has no non-error
terminal state reachable from a queued job.

**Please key on `error_category`, not on `state`,** if you surface this to an
operator. Showing "duplicate_skipped" as a red failure would be misleading. Say
"already ingested" or similar. Tell us if you would prefer a dedicated phase and
we will look at widening the state machine.

### 6.4 Two new `error_category` values

Add these to whatever mapping PlayOut uses for failure reasons:

| `error_category` | Meaning | Operator-facing wording |
|---|---|---|
| `source_missing_on_recovery` | Job was pending across a restart; source file is gone | "Source file no longer available" |
| `duplicate_skipped` | Retry matched an asset already ingested | "Already ingested — nothing to do" |
| `fingerprint_failure` | Source could not be read | "Could not read the source file" |
| `path_outside_watch_folder` | Input resolved outside the watch folder | "File is not in the watch folder" |

The last two existed as behaviour but previously produced no job record at all
on a retry; now they close the job out visibly.

### 6.5 Progress updates are unchanged for you

SSE `progress` events still arrive at the same 250 ms throttle and carry the
same fields. What changed is only how often the *database* is written, which
PlayOut never sees. `GET /api/jobs` still serves the live in-memory record, so
percentages are as current as they ever were.

---

## 7. Service lifecycle: `POST /api/service/start` now uses status codes

**Status in PlayoutTranscode:** shipped (T2-5).

PlayOut does not call these endpoints today — `src-tauri/src/ingestor_api.rs`
has no `/api/service/*` call site, and the operator starts and stops the
service from the PlayoutTranscode web UI or the Windows Services console. This
section is here because that may change, and because the deployment note in
7.4 affects anyone running both.

### 7.1 Why it changed

`running` was a boolean, so "stopped" and "still stopping" looked the same. A
stop followed immediately by a start spawned a second watcher over the same
folder while the first was still killing its FFmpeg children — two dispatchers
on one concurrency semaphore, and whichever encode lost failed on a locked
output file. The service now has an explicit state machine.

### 7.2 New fields, additive

`GET /api/service/status`:

```json
{ "running": false, "state": "stopped", "generation": 3 }
```

| Field | Meaning |
|---|---|
| `running` | **unchanged.** `true` only in `running` |
| `state` | `stopped` \| `starting` \| `running` \| `stopping` |
| `generation` | incremented once per start; identifies the current run |

`GET /api/diagnostics` gains the same string at `service.state`. No existing
field changed, so a client that only reads `running` keeps working.

**The one that matters is `stopping`.** `POST /api/service/stop` returns as soon
as the stop is *requested*; the processing thread then unwinds, which takes as
long as the in-flight FFmpeg children take to die. During that window `running`
is already `false` but a start will be refused. If PlayOut ever drives a
restart, poll `GET /api/service/status` until `state == "stopped"` rather than
sleeping a fixed interval.

### 7.3 `POST /api/service/start` answers with a status code

It used to return `200` with `{"success": false, "error": "..."}` for every
refusal. Now:

| Status | Body `error` | Meaning |
|---|---|---|
| `200` | — | started; `{"success": true, "state": "running"}` |
| `409` | `Service already running` / `Service is starting` / `Service is stopping` | retry later; the body also carries `state` |
| `503` | `FFmpeg toolchain: …` | the toolchain is missing or unusable — operator action |
| `400` | `Watch and target folders must be configured first` | configuration, not timing |

`success` and `error` are still in every body, so a client that ignores the
status code and reads the body behaves exactly as before. The distinction is
worth honouring though: `409` is worth retrying, `400` and `503` are not.

`POST /api/service/stop` is unchanged apart from an additive `state` in its
body. It still requires `X-Confirm-Destructive: yes` (section 2).

### 7.4 One instance per data directory — **tell your operators**

The service now takes an advisory lock, `playout-transcode.lock`, in its data
directory (section 5.2) and refuses to start if a live process already holds
it. Startup fails with:

```
another PlayoutTranscode instance (pid 1234) is already using this data
directory; its lock is C:\ProgramData\PlayoutTranscode\playout-transcode.lock.
Stop that instance, or start this one with a different --data-dir.
```

This is deliberate. Two processes on one data directory meant two watchers on
one folder and two writers on one SQLite registry, and the symptom was a stream
of unexplained ingest failures with no obvious cause. The way operators reached
it was starting the portable build while the Windows service was already
running — which, since 5.1 made the service actually work, is now easier to do
by accident, not harder.

Two installs with **separate** data directories are unaffected and remain
supported.

A lock left behind by a crash is taken over by the next start, with a warning
in the log. No one has to delete a file by hand.

### 7.5 Nothing else for PlayOut here

Ingest, job, asset and SSE behaviour are untouched by this step.

---

## 8. Still coming (no action yet, listed for planning)

- **Paginated listings (T2-7).** `GET /api/assets` will default to
  `limit=1000` with `X-Total-Count`, and will omit `keyframe_offsets` unless
  `?fields=full`. Single-asset and batch resolve keep the full array, which is
  what PlayOut's per-asset hydration uses. PlayOut's `list` currently expects
  the whole library in one response; it will need to page, or raise its own
  16 MiB cap. This is the fix for the handoff's §3.4 size concern.
- **SSE `resync` event (T2-10).** A new event type telling clients they missed
  messages and should refetch. PlayOut should treat an unknown SSE event as a
  no-op today so adding it is non-breaking.
