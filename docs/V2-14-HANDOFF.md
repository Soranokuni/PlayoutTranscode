# Slice V2-14 Handoff: Crash, Reliability & Chaos Verification

## Overview

Slice V2-14 establishes comprehensive reliability and chaos boundary test suites verifying worker crash recovery, stale lease eviction, duplicate request dedup, and orphan staging cleanup.

## Invariants & Features Verified

1. **Duplicate Request Rejection**:
   - Verified active `request_hash` partial unique index rejects concurrent duplicate enqueue attempts while allowing re-transcode after completion.

2. **Worker Crash & Stale Lease Recovery**:
   - Verified background crash recovery queries evict expired leases (`heartbeat_at < now - 60s`), reset job state to `Pending`/`queued`, and increment retry attempts.

3. **Orphan Staging File Cleanup**:
   - Verified temporary `.tmp_*` files and `.tmp_json` sidecars left by crashed jobs are safely cleaned without affecting valid published media.

## Verification Results

- **Unit Tests**: 85/85 passed (`cargo test`).
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Chaos Reliability Tests**: 3/3 passed (`tests/reliability_chaos.rs`).
- **Wire Contract Tests**: 10/10 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` clean.
