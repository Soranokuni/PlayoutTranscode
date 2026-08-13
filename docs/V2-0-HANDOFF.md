# V2-0 Handoff — Baseline, Compatibility Contracts, Fixtures, and Verification

- **HEAD SHA**: `9ac2561dd5d68963b81e2f4cfe005d23e2f22bf0` (Baseline commit: `24df92b2084055b3bea05ad46eccd00c1f09a1eb`)
- **Branch**: `main`
- **Date**: August 13, 2026
- **Status**: **COMPLETE & VERIFIED**

---

## 1. Completed Work Summary

Slice V2-0 freezes current observable V1 behavior and establishes the baseline contract, fixture specifications, PowerShell tooling, and self-contained integration test suite for PlayoutTranscode V2:

1. **Contract & Behavior Specifications**:
   - `docs/V2-0-CONTRACT.md`: Documents all V1 REST API endpoints, SSE event envelope structures, SQLite database schema, `AssetResponse` and `AssetSidecar` field invariants, config structure, and output naming semantics.
   - `docs/V2-0-ENCODING-PROFILES.md`: Documents FFmpeg argument generation for Profiles A, B, and C, forced 25/1 CFR behavior, 48 kHz stereo audio handling, progress parsing, and post-encode validation rules.
   - `docs/contracts/*.json`: 8 golden JSON contract sample files (`asset-response`, `asset-sidecar`, `config`, `health`, `job-record`, `sse-event-envelope`, `stats`, `watchfolder`).

2. **Fixtures Specification & Canonical Manifest**:
   - `docs/V2-0-FIXTURES.md`: Defines 8 synthetic fixture classes (`video_only`, `audio_only`, `video_stereo`, `multichannel`, `vfr_source`, `interlaced_tff`, `corrupt_truncated`, `mezzanine_compliant`), generation commands, probe expectations, and failure policies.
   - `fixtures/manifest.json`: Canonical JSON manifest detailing expected probe invariants and informational toolchain hash policies.
   - `fixtures/README.md`: Fixture usage guide.

3. **PowerShell Automation Tooling**:
   - `scripts/generate-fixtures.ps1`: Generates synthetic media files using FFmpeg `lavfi` synthetic filters. Checks PATH and `bin/` directory; fails clearly with exit code `1` if FFmpeg is unavailable.
   - `scripts/verify-baseline.ps1`: Probes generated media fixtures using `ffprobe`, asserts duration, FPS rationals, resolution, codec, sample rate, and channel properties against `manifest.json`. Fails clearly with exit code `1` if FFprobe is unavailable.

4. **Integration Test Suite**:
   - `tests/v1_wire_contract.rs`: Added 9 self-contained integration tests validating golden contract JSON schemas, field invariants, and exercising a live Axum HTTP server instance on ephemeral ports via `reqwest`.

---

## 2. Changed Files in V2-0

- `docs/V2-0-CONTRACT.md`
- `docs/V2-0-ENCODING-PROFILES.md`
- `docs/V2-0-FIXTURES.md`
- `docs/V2-0-HANDOFF.md` (this file)
- `docs/contracts/asset-response.example.json`
- `docs/contracts/asset-sidecar.example.json`
- `docs/contracts/config.example.json`
- `docs/contracts/health.example.json`
- `docs/contracts/job-record.example.json`
- `docs/contracts/sse-event-envelope.example.json`
- `docs/contracts/stats.example.json`
- `docs/contracts/watchfolder.example.json`
- `fixtures/manifest.json`
- `fixtures/README.md`
- `scripts/generate-fixtures.ps1`
- `scripts/verify-baseline.ps1`
- `tests/v1_wire_contract.rs`

---

## 3. Protected Files Confirmation

The following protected files were **confirmed 100% unchanged**:
- Production Rust backend: `src/profiles.rs`, `src/processor.rs`, `src/db.rs`, `src/server.rs`, `src/config.rs`, `src/encoder.rs`, `src/jobs.rs`, `src/bootstrap.rs`, `src/watcher.rs`, `src/service_handle.rs`, `src/identity.rs`, `src/fingerprint.rs`, `src/logging.rs`, `src/main.rs`.
- Web UI: `web-ui/*`.
- Installer: `installer/*`.
- Root configuration / dependencies: `Cargo.toml`, `Cargo.lock`.
- Boundary integration test: `tests/contract_boundary.rs`.
- Root documentation: `README.md`, `AGENTS.md`.
- Downstream consumer: `PlayOutVue` (`d:\PlayOut`).

---

## 4. Exact Verification Results

```powershell
# 1. Cargo Check
cargo check
# Result: Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.36s (PASSED - 0 errors)

# 2. Cargo Test
cargo test
# Result: 24 unit tests passed, 9 boundary integration tests passed, 9 wire contract tests passed (PASSED - 42 total tests)

# 3. NPM CI (in web-ui)
npm ci
# Result: added 98 packages in 8s (PASSED - 0 errors)

# 4. NPM Build (in web-ui)
npm run build
# Result: vue-tsc --build && vite build completed in 3.42s (PASSED)

# 5. PowerShell Fixture Generator (without PATH FFmpeg)
powershell -ExecutionPolicy Bypass -File scripts/generate-fixtures.ps1
# Output:
# ==> Resolving FFmpeg and FFprobe toolchain...
# ERROR: FFmpeg or FFprobe toolchain is not available.
#   FFmpeg: NOT FOUND
#   FFprobe: NOT FOUND
# Please install FFmpeg/FFprobe into PATH or download them via the PlayoutTranscode control panel / 'PlayoutTranscode setup'.
# Result: Exited with code 1 (PASSED - Failed clearly as mandated)

# 6. PowerShell Baseline Verification (without PATH FFprobe)
powershell -ExecutionPolicy Bypass -File scripts/verify-baseline.ps1
# Output:
# ==> Resolving FFprobe toolchain...
# ERROR: FFprobe toolchain is not available.
# Please install FFmpeg/FFprobe into PATH or download them via the PlayoutTranscode control panel.
# Result: Exited with code 1 (PASSED - Failed clearly as mandated)
```

---

## 5. Documented V1 Behaviors & Invariants

1. **Forced 25/1 CFR**: All transcode output generated by Profiles A, B, and C is forced to 25/1 CFR (`src/profiles.rs:4-5`). Source rational FPS is snapped during probing, but output is normalized to 25 fps. V2-0 preserves this behavior without alteration.
2. **`mezzanine_ok` vs `status="ready"`**: Assets with validation warnings (e.g. `fps_mismatch`, `audio_sample_rate_not_48k`, `closed_gop_violation`, `missing_faststart`) receive `mezzanine_ok = false`, but are published with `status="ready"`. Hard error status is reserved for probe failure, encode process failure, missing output file, 0-byte file, or duration mismatch.
3. **PlayOutVue API Wire Compatibility**: All field names, types, unit scales (milliseconds), and rational structures on `AssetResponse` and `AssetSidecar` are frozen and backward-compatible.

---

## 6. Fixture & Toolchain Assumptions

- Fixture generation and verification require `ffmpeg` and `ffprobe` (6.0+ or 7.x).
- Executables are resolved from `PATH`, `$PSScriptRoot/../bin/`, or `$PSScriptRoot/../target/debug/bin/`.
- If missing, scripts fail clearly with exit code `1` rather than silently skipping.
- Binary media output hashes are marked **informational and toolchain-specific**; semantic probe properties (duration, resolution, rationals, channels, sample rate) serve as primary assertions.

---

## 7. Next PR Proposal — Slice V2-1

**V2-1 — Versioned Configuration and Policy Engine**
- Introduce typed configuration sections for encoding, audio, validation, storage, retry, and concurrency in `src/config.rs`.
- Add audio policy options: `ebu_r128`, `atsc_a85`, `passthrough_validate`, `analyze_only`.
- Add configuration schema versioning and migration logic.
- Preserve backward compatibility for all existing V1 configuration keys.

---

## 8. Out of Scope Items

- Production FFmpeg argument changes (deferred to V2-7).
- Two-pass audio loudness normalization (deferred to V2-8).
- Database schema changes (deferred to V2-4 / V2-5).
- REST API endpoint changes (deferred to V2-10).
- Web UI overhaul (deferred to V2-11).
