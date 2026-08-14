# Slice V2-16 Handoff: PlayOutVue Integration & End-to-End Contract Verification

## Overview

Slice V2-16 verifies the end-to-end ingest-to-playout contract boundary between `PlayoutTranscode` (upstream media-preparation service) and `PlayOutVue` (downstream broadcast playout orchestrator). It ensures that all V2 assets (with QC reports, rational FPS, closed GOP, faststart, loudness normalization, and stable paths) hydrate perfectly in PlayOutVue without needing repair and produce frame-accurate CasparCG play commands.

## Invariants & Features Verified

1. **Hydration & Frame Trim Calculation**:
   - Upstream V2 metadata (`fps_num`, `fps_den`, `duration_ms`, `trim_in_ms`, `trim_out_ms`, `current_path`) hydrates seamlessly into PlayOutVue's `RundownItem` domain model.
   - `compute_frame_trim` converts millisecond trims into exact frames (`in_frame`, `duration_frames`, `fps_rational`) without float rounding errors.

2. **CasparCG Play Command Generation**:
   - Produces deterministic `PLAY {channel}-{layer} "{path}" SEEK {in_frame} LENGTH {duration_frames}` commands matching broadcast automation specifications.

3. **Subclip Isolation & Purge Safety**:
   - Verified that parent assets and virtual subclips operate independently with immutable source files.

## Verification Results

- **Unit Tests**: 85/85 passed (`cargo test`).
- **Contract Boundary Tests**: 10/10 passed (`tests/contract_boundary.rs`, including V2 E2E integration test).
- **Chaos Reliability Tests**: 3/3 passed (`tests/reliability_chaos.rs`).
- **Wire Contract Tests**: 10/10 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` clean.
- **Frontend Build**: `npm run build` compiled in 1.60s.
