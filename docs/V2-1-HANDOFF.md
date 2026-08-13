# V2-1 Handoff — Versioned Configuration and Policy Model

## 1. Overview
Slice V2-1 introduces the V2 Versioned Configuration and Policy Model for PlayoutTranscode. It defines typed, additive Rust structures for audio, validation, storage, retry, and toolchain policies without altering any legacy V1 fields, FFmpeg execution parameters, database schemas, frontend code, or runtime behaviors.

---

## 2. Baseline & HEAD Information
- **Parent Baseline SHA**: `791629f9f46a1cdf4c2e8209537e4437fd68ee9a` (V2-0 Approved Baseline)
- **V2-1 Final HEAD SHA**: `91f1b6d28fdcfe8d4caa78d015b97fff143f6b8e`
- **Branch**: `main`
- **Scope**: Versioned Configuration & Policy Model PR only.

---

## 3. Policy Structures Added
- **`AudioPolicy`**: `mode` (`legacy_v1_encode`, `ebu_r128`, `atsc_a85`, `passthrough_validate`, `analyze_only`), `codec`, `bitrate`, `sample_rate_hz`, `channels`, `channel_layout`, `target_lufs`, `true_peak_dbtp`, `lra_target`, `dual_mono`, `preserve_original`.
- **`ValidationPolicy`**: `enforce_closed_gop`, `enforce_faststart`, `enforce_48k_audio`, `max_duration_delta_ms`, `strict_ready_blocking`.
- **`StoragePolicy`**: `atomic_publication`, `preserve_subclips_on_purge`, `clean_source_after_success`.
- **`RetryPolicyV2`**: `max_attempts`, `retry_delay_ms`, `auto_retry_on_start`.
- **`ToolchainPolicy`**: `ffmpeg_path`, `ffprobe_path`, `verify_on_startup`.

---

## 4. Migration & Compatibility Behavior
1. **In-Memory Policy Derivation**:
   - Missing `version` field in `config.toml` defaults to `version = 1`.
   - Existing V1 `config.toml` files on disk are **never rewritten silently on startup**.
   - Effective V2 policies are derived in memory (`config.effective_audio_policy()`, etc.).
   - Legacy V1 configuration fields remain present in TOML and REST API endpoints (`GET /api/config`, `PUT /api/config`).
2. **Explicit V2 Priority & Disagreement Warnings**:
   - Explicit V2 policy settings override derived legacy values when present.
   - If an explicit V2 policy setting disagrees with a legacy V1 field (e.g. `audio_policy.codec` vs `encoding.audio_codec`), explicit V2 wins and a `tracing::warn!` message is emitted.
   - Explicit saving via `PUT /api/config` sets `version = 2` when writing to disk.
3. **Loudness Validation Rules**:
   - Loudness parameters (`target_lufs`, `true_peak_dbtp`, `lra_target`) are optional and evaluated only when `AudioMode` is `ebu_r128` or `atsc_a85`. Valid ranges (`-70.0 <= target_lufs <= 0.0`, `-10.0 <= true_peak_dbtp <= 0.0`) are checked during `AppConfig::validate()`.

---

## 5. Verification Results
1. **`cargo check`**: Clean compilation with 0 errors/warnings.
2. **`cargo test`**: 47 total tests passed across the workspace:
   - 29 unit tests in `src/*.rs` (including 5 new `#[cfg(test)]` config migration & policy tests in `src/config.rs`).
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite production bundle generated without errors.

---

## 6. Changed Files & Protected File Verification
### Changed Files
- [`src/config.rs`](file:///d:/PlayoutTranscode/src/config.rs): Added V2 policy structs, `version: u32` default, effective policy helpers, validation rules, and unit tests under `#[cfg(test)]`.
- [`src/server.rs`](file:///d:/PlayoutTranscode/src/server.rs): Updated `get_config` and `put_config` REST handlers to expose and update V2 policy sections.
- [`docs/V2-1-HANDOFF.md`](file:///d:/PlayoutTranscode/docs/V2-1-HANDOFF.md): Slice V2-1 documentation and verification handoff.

### Protected Files (Untouched)
- `src/encoder.rs`, `src/processor.rs`, `src/probe.rs`, `src/profiles.rs`, `src/db.rs`
- `web-ui/**` (no code or asset changes)
- `installer/**`
- PlayOutVue (`d:\PlayOut`) — zero reads/writes performed.

---

## 7. Known Limitations
- V2-1 defines the policy model only. Encoder execution (`src/encoder.rs`) currently continues using V1 profile parameters until subsequent V2 slices integrate `AudioPolicy` into FFmpeg command builders.

---

## 8. Proposed Next PR (V2-2)
- **V2-2 Scope**: Robust Ingestion and Watcher Pipeline.
- Introduce atomic publication semantics, settled-file verification, polling resilience, and structured job retries backed by `RetryPolicyV2` and `StoragePolicy`.
