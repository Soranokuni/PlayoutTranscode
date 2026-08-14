# Slice V2-12 Handoff: Audio and QC UI Integration

## Overview

Slice V2-12 exposes audio normalization policy controls, loudness compliance targets, and quality control (QC) parameters directly in the operator web interface.

## Invariants & Features Implemented

1. **Audio Normalization Configuration UI**:
   - Exposed Mode selector: `Legacy (Pass-through)`, `EBU R128`, `ATSC A/85`, `Passthrough & Validate`, and `Analyze Only`.
   - Exposed granular target overrides: Target Integrated Loudness (LUFS), True Peak ceiling (dBTP), and Loudness Range target (LRA LU).
   - Added Mono to Dual-Mono stereo track expansion checkbox.

2. **Bidirectional Policy Persistence**:
   - `populateFromConfig` loads active `audio_policy` settings from backend `GET /api/v2/config` into interactive form refs.
   - `saveConfig` packages typed `AudioPolicyPayload` and persists it via `PUT /api/v2/config`.

3. **Frontend Compatibility**:
   - Fully typed in `useEventStream.ts` (`AudioPolicyPayload`).
   - Clean Vite/TypeScript build without regressions.

## Verification Results

- **Unit Tests**: 84/84 passed (`cargo test`).
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 10/10 passed (`tests/v1_wire_contract.rs`).
- **Frontend Build**: `npm run build` compiled in 1.65s with 0 errors.
