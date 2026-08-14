# Slice V2-6 Handoff: Atomic Output Publication & Collision Hardening

## Overview

Slice V2-6 hardens the atomic output publication pipeline in `PlayoutTranscode`. It guarantees that partial, invalid, or unverified files are never exposed under their canonical destination name. Additionally, it computes SHA-256 integrity checksums and exact byte sizes on staged media before rename, attaches structured `ValidationReport` payloads to sidecar JSON metadata, automatically purges orphaned staging files on startup, and provides multi-candidate collision mitigation.

## Invariants & Enhancements Implemented

1. **SHA-256 Checksum & File Size Measurement**:
   - `compute_file_sha256`: Deterministically computes lowercase SHA-256 hash of the fully flushed staging mezzanine file.
   - Measures exact byte size via filesystem metadata.

2. **ValidationReport Sidecar Metadata**:
   - Added `ValidationReport` struct to `src/identity.rs` capturing `mezzanine_ok`, `duration_ms`, `fps`, `fps_num`, `fps_den`, `audio_sample_rate`, `audio_channels`, `closed_gop`, `faststart`, `warnings`, `sha256`, and `file_size_bytes`.
   - Embedded with `#[serde(default, skip_serializing_if = "Option::is_none")]` into `SidecarPayload`, guaranteeing 100% backward compatibility for downstream consumers.

3. **Orphan Staging File Cleanup Sweep**:
   - `cleanup_orphan_staging_files`: Safely scans output directory for abandoned `.tmp_*` media or `.tmp_json` files exceeding the max age threshold (e.g. 1800s / 30m).
   - Integrated into startup recovery sweep in `src/service_handle.rs`.

4. **Multi-Candidate Collision Mitigation**:
   - `build_unique_output_path`: Probes candidate file paths and automatically applies UUID iteration / timestamp suffixes if collisions are detected.

## Verification Results

- **Unit Tests**: 80/80 passed (`cargo test`) including tests for SHA-256 calculation, orphan cleanup, collision avoidance, and validation report sidecar serialization.
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 9/9 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` clean.
- **Web UI**: `npm run build` completed in 1.55s.
