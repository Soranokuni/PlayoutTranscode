# Slice V2-10 Handoff: Versioned API and Event Contract

## Overview

Slice V2-10 establishes the `/api/v2` versioned REST API namespace and event endpoints in `PlayoutTranscode`. It provides clean, versioned interfaces for health, toolchain audits, profiles, jobs, assets, events, and metrics while maintaining 100% backward compatibility for all legacy `/api/*` V1 endpoints.

## Invariants & Features Implemented

1. **Versioned `/api/v2` Route Surface**:
   - `GET /api/v2/health`: Returns API version (`2.0.0`), service uptime in seconds, toolchain readiness, and running status.
   - `GET /api/v2/toolchain`: Returns FFmpeg/FFprobe versions and discovery status.
   - `GET /api/v2/config` & `PUT /api/v2/config`: Reads and updates typed V2 configuration.
   - `GET /api/v2/profiles`: Returns the registry of typed standard broadcast profiles.
   - `GET /api/v2/jobs`: Lists all transcode jobs with complete phase and error classification.
   - `GET /api/v2/jobs/{id}`: Returns specific job record by ID or 404.
   - `POST /api/v2/jobs/{id}/cancel`: Cooperatively terminates active FFmpeg processes and cancels the job.
   - `POST /api/v2/jobs/{id}/retry`: Re-queues a failed or cancelled job.
   - `GET /api/v2/assets`: Lists database assets with QC and validation status.
   - `GET /api/v2/assets/{uuid}`: Returns hydrated asset record with validation report.
   - `GET /api/v2/events`: Server-Sent Events (SSE) stream for real-time progress, completion, and failure events.
   - `GET /api/v2/metrics`: Aggregate operational metrics (pending, active, completed, failed, active PID counts, and uptime).

2. **Full V1 Backward Compatibility**:
   - All existing `/api/*` routes remain fully intact and verified against golden contract snapshots.

## Verification Results

- **Unit Tests**: 84/84 passed (`cargo test`).
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 10/10 passed (`tests/v1_wire_contract.rs`, including new live Axum V2 contract test).
- **Formatting**: `cargo fmt --check` clean.
- **Web UI**: `npm run build` completed in 1.62s.
