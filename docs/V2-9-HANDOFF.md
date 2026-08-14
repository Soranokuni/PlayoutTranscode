# Slice V2-9 Handoff: Structured Validation and QC Engine

## Overview

Slice V2-9 introduces the structured quality control (`QcEngine`) finding model and validation evaluation in `PlayoutTranscode`. It replaces ad-hoc boolean checking with typed findings (`ValidationFinding`) with severities (`Info`, `Warning`, `Error`), diagnostic codes, operator-facing messages, and explicit measured vs. expected parameters. Blocking errors prevent mezzanine publication while non-blocking warnings remain preserved in sidecars and API responses.

## Invariants & Features Implemented

1. **Structured QC Findings (`ValidationFinding` and `QcReport`)**:
   - `Severity`: `Info`, `Warning`, `Error`.
   - `ValidationFinding`: contains `code`, `message`, `measured`, `expected`.
   - `QcReport`: contains `passed`, `blocking_errors`, `warnings_count`, and `findings`.

2. **Automated QC Checks (`run_qc_evaluation`)**:
   - **Duration**: verified > 0 (Blocking error on zero/negative).
   - **FPS Rational & Standard**: verified against target broadcast rational (25/1 or 50/1).
   - **Audio Sample Rate**: verified exactly 48000 Hz (Blocking error on mismatch).
   - **Closed GOP Cadence**: verified uniform 2s closed GOP structure without open GOP leaks.
   - **Faststart Optimization**: verified MP4 `moov` atom positioned in the first 64KB.
   - **Audio Loudness**: verified silent audio pass & short clip dynamic mode detection.

3. **Additive Sidecar Metadata**:
   - Embedded `qc_report` and `findings` in `ValidationReport` and `SidecarPayload` with `#[serde(default, skip_serializing_if = "Option::is_none")]` preserving full V1 compatibility.

## Verification Results

- **Unit Tests**: 84/84 passed (`cargo test`) including compliant QC pass and multi-error blocking failure suites.
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 9/9 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` clean.
- **Web UI**: `npm run build` completed in 1.65s.
