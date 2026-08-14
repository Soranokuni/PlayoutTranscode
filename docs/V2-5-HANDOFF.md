# Slice V2-5 Handoff: Durable Queue and Restart Recovery

## Overview

Slice V2-5 establishes SQLite-backed durable queue persistence, atomic worker leasing, periodic worker heartbeats, cooperative cancellation, automatic startup recovery of stale/crashed jobs, request-hash deduplication, and staging file cleanup in `PlayoutTranscode`. It guarantees that a crash or restart never leaves in-flight jobs stuck or produces duplicate encodes while preserving 100% V1 wire contract compatibility.

## Invariants & Features Implemented

1. **Durable Job Store (`transcode_jobs` table)**:
   - Persists all `JobRecord` fields, state, phase, attempt counters, diagnostic logs, and frame/bitrate metrics to SQLite.
   - Includes worker leasing timestamps (`worker_id`, `leased_until`, `heartbeat_at`, `cancel_requested`).
   - Populates and synchronizes `JobQueue` seamlessly across process restarts.

2. **Atomic Worker Lease & Heartbeat Protocol**:
   - `claim_next_job`: Atomically acquires pending jobs or expired-lease jobs using SQLite transactions.
   - `heartbeat_job`: Active workers renew their lease every 2 seconds during encoding and detect cooperative cancellation.
   - Graceful cancellation: When cancel is requested via `POST /api/jobs/{id}/cancel` or API, the active FFmpeg subprocess tree is terminated, the job transitions to `Cancelled`, and temporary staging files are cleaned up immediately.

3. **Crash & Stale-Job Recovery on Startup**:
   - `recover_stale_jobs`: Automatically sweeps `transcode_jobs` on startup:
     - Re-queues jobs whose attempt count is within `max_attempts` with `attempt += 1` and stage `"Re-queued (stale crash recovery)"`.
     - Transitions jobs to `Failed` if retry attempts are exhausted.
   - Prevents orphaned in-flight jobs from being stuck in `Processing` indefinitely.

4. **Request-Hash Deduplication**:
   - Computes deterministic request hashes (`fingerprint ^ frame_count`) and indexes them to avoid duplicate active encoding passes.

## Verification Results

- **Unit Tests**: 76/76 passed (including 4 new dedicated durable job persistence, atomic leasing, crash recovery, and cancellation tests).
- **Boundary Integration Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 9/9 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` passed cleanly.
- **Web UI**: `npm run build` in `web-ui/` completed cleanly in 2.67s.
