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

## 2. Confirmation header on destructive operations — **not yet shipped**

**Status in PlayoutTranscode:** planned (T1-5). This section is advance notice
so it can be scheduled; the header is not enforced yet.

Destructive routes will require `X-Confirm-Destructive: yes` and will otherwise
return `428 Precondition Required` with `{"error":"confirmation_required"}`.
The affected calls are:

- `DELETE /api/assets/{uuid}/purge`
- `DELETE /api/folders/purge`
- `DELETE /api/recycle-bin/purge`
- `POST /api/recycle-bin/auto-purge`
- `POST /api/folders/trash`
- `POST /api/jobs/retry-failed`
- `PUT /api/config`
- `POST /api/service/stop`

PlayOut already prompts the operator natively before these, so this is a
one-line addition per call in `ingestor_api.rs`. Adding the header **now** is
safe: the service ignores unknown headers today.

---

## 3. Open question for the PlayOut team: what values does `tp` take?

**Blocking:** T1-3 (server-side validation of `PUT /api/assets/{uuid}/tp`).

`tp` is written by PlayOut's `ComplianceModule.vue` and stored verbatim. The
service currently accepts any string of any length, which means PlayOut can
read back whatever was written into it by anything else that can reach the API.

We intend to validate it server-side. Until we hear from you, the planned
provisional rule is:

```
^[A-Za-z0-9 _\-|:\[\]{}",.]{0,512}$
```

**Please confirm:**

1. Is `tp` a fixed enumeration (e.g. `TP`, `SHOW`, `NONE`), or free text?
2. If it is structured (the `|`-delimited form seen in `rating`), what is the
   grammar?
3. What is the realistic maximum length?

If it is an enumeration we will validate against the list instead, which is
strictly better. Reply before T1-3 lands or the provisional regex ships.

---

## 4. Already-fixed items from the handoff, for information

These needed no PlayOut change; they are listed so the client team knows the
behaviour is now enforced server-side.

| Handoff item | Now |
|---|---|
| §3.2 treat `folder_path` as untrusted | `folder_path` is validated and `LIKE` wildcards are escaped. `/%` returns 422 instead of matching the whole library. |
| §3.3 bind / CORS | `bind_address` must be loopback unless a token is set. CORS is a loopback allow-list. A non-loopback `Host` header returns 421. PlayOut's `reqwest` client sends a correct `Host`, so it is unaffected. |
| §3.5 health endpoint cheap | Unchanged and still side-effect free. |
| §3.6 error bodies must not leak paths | Partly done: the SPA 404 and the config-save error no longer contain paths. The remaining sites are T1-4. |

### One behaviour change worth noting

`POST /api/assets/{uuid}/restore` with an invalid `target_folder` now returns
**422** instead of silently restoring the asset to `/`. If PlayOut relied on
the silent fallback, it must handle the 422 — but sending a valid folder path
is the correct fix.

---

## 5. Still coming (no action yet, listed for planning)

- **Paginated listings (T2-7).** `GET /api/assets` will default to
  `limit=1000` with `X-Total-Count`, and will omit `keyframe_offsets` unless
  `?fields=full`. Single-asset and batch resolve keep the full array, which is
  what PlayOut's per-asset hydration uses. PlayOut's `list` currently expects
  the whole library in one response; it will need to page, or raise its own
  16 MiB cap. This is the fix for the handoff's §3.4 size concern.
- **SSE `resync` event (T2-10).** A new event type telling clients they missed
  messages and should refetch. PlayOut should treat an unknown SSE event as a
  no-op today so adding it is non-breaking.
