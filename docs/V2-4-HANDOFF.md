# Slice V2-4 Handoff: Explicit Job State Machine and Lifecycle Tracking

## Overview

Slice V2-4 implements an explicit, typed job state machine (`JobPhase`), formal legal transition validation, rich diagnostic metadata tracking, and cancel/retry lifecycle management in `PlayoutTranscode`. It guarantees 100% backward compatibility with V1 REST contracts, SSE event streams, and downstream PlayOutVue hydration.

## Invariants & Features Implemented

1. **Explicit Phase Model (`JobPhase`)**:
   - Replaced generic string state transitions with 12 typed phases:
     - `queued`
     - `probing`
     - `planned`
     - `encoding`
     - `normalizing_audio`
     - `validating`
     - `publishing`
     - `completed`
     - `failed`
     - `cancel_requested`
     - `cancelled`
     - `recoverable`
   - Documented and enforced transition validity via `JobPhase::can_transition_to`.
   - Prevented illegal jumps (e.g. `Queued -> Completed`, `Completed -> Encoding`).

2. **V1 Wire Contract Compatibility**:
   - `JobRecord` maintains `state: JobState` (`Pending`, `Processing`, `Completed`, `Failed`, `Cancelled`), `current_stage: String`, `progress: f32`, `error: Option<String>`, and `stderr_log: Option<Vec<String>>`.
   - `JobPhase::as_v1_state` deterministically maps fine-grained V2 phases to V1 coarse states.
   - All `/api/jobs` sub-routes, `/api/stats`, and SSE event envelopes serialize with 100% golden fixture fidelity.

3. **Rich Lifecycle & Diagnostics Metadata**:
   - `fingerprint: Option<i64>` (FNV-1a 64-bit content hash).
   - `request_hash: Option<String>` (Deterministic execution hash).
   - `started_at: Option<String>` and `finished_at: Option<String>` (ISO 8601 timestamps automatically stamped on first non-queued phase and terminal states).
   - `attempt: u32` and `max_attempts: u32`.
   - `error_category: Option<String>` (Classified failure reason, e.g. `validation_failure`, `probe_failure`, `retryable_error`, `publish_failure`).

4. **Pipeline Orchestration**:
   - Process loop drives assets through clean, legal phase transitions:
     `Queued -> Probing -> Planned -> Encoding -> Validating -> Publishing -> Completed` (or `Recoverable` / `Failed`).
   - Zero SQLite schema migrations required.

## Verification Results

- **Unit Tests**: 72/72 passed (including 6 new dedicated phase transition, error rejection, retry, and cancellation tests).
- **Boundary Integration Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 9/9 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` passed cleanly.
- **Web UI**: `npm run build` in `web-ui/` completed cleanly in 2.26s.
