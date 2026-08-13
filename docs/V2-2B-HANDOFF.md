# V2-2B Handoff — Retry Classification and Bounded Retry Execution

## 1. Overview
Slice V2-2B implements **Retry Classification and Bounded Retry Execution** for PlayoutTranscode. Transcode errors are categorized into typed categories (`Retryable`, `Permanent`, `Cancelled`). Transient errors (file lock contention, process spawn failures, temporary I/O timeouts) are automatically retried up to `max_attempts` with `retry_delay_ms` backoff. Permanent errors (invalid media, corrupt headers, post-encode validation failures, permission denied, disk full) and cancellations are terminated immediately without retrying.

---

## 2. Baseline & HEAD Information
- **V2-2B Final HEAD SHA**: `568af810e5a00967fd16c87003501e22cf5e6bae`
- **Branch**: `main`
- **Scope**: V2-2B Bounded Retries & Error Classification only.

---

## 3. Implementation Details

### A. Semantics & Interpretation
- **`max_attempts`**: Interpreted strictly as **total attempts, including the initial attempt**.
  - `max_attempts = 1`: 1 total attempt (0 retries).
  - `max_attempts = 2`: Up to 2 total attempts (1 initial attempt + up to 1 retry).

### B. Typed Error Classification (`RetryClass`)
Internal enum `RetryClass` distinguishes:
1. **`Retryable`**: Transient OS file lock errors (`os error 32`, `file locked`, `sharing violation`), process spawn failures, temporary timeouts, resource unavailable errors.
2. **`Permanent`**: Invalid media headers (`Probe:`), profile disabled, unsupported codec, `permission denied`, `disk full`, existing file path collision, and **all post-encode validation failures** (`fps_mismatch`, `zero_duration`, `audio_sample_rate_not_48k`, `closed_gop_violation`, `missing_faststart`).
3. **`Cancelled`**: Job cancelled by user or service shutdown.

### C. Execution & Staging Safety
- **Staging Cleanup**: `publisher.cleanup_staging` is executed before every attempt and immediately after any failed attempt.
- **Single Database Update**: `db::mark_ready` or `db::mark_error` is called **exactly once** when the job completes or fails permanently. No duplicate database rows or duplicate output files are created.
- **Non-blocking Delay**: Asynchronous retry delays use `std::thread::sleep` on dedicated `spawn_blocking` worker threads without blocking Tokio runtime worker threads.

### D. Testing Seam (`TranscodeRunner`)
- Introduced `TranscodeRunner` trait and `RealTranscodeRunner` seam to decouple transcode execution from real FFmpeg binaries during unit testing.
- Created `MockTranscodeRunner` in `src/processor.rs` tests to test transient retries, permanent validation stops, and attempt boundaries without invoking FFmpeg.

---

## 4. Verification Results
1. **`cargo check`**: Clean compilation with 0 errors and 0 warnings.
2. **`cargo test`**: **53 total tests passed** across all workspace test suites:
   - 35 unit tests (including 3 new retry classification & runner seam unit tests in `src/processor.rs`).
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI packages installed cleanly and Vite bundle generated without errors in 2.84s.

---

## 5. Changed & Protected Files

### Changed Files
- [`src/processor.rs`](file:///d:/PlayoutTranscode/src/processor.rs): Implemented `RetryClass`, `classify_error`, `TranscodeRunner` seam, bounded retry loop, staging cleanup between retries, and unit tests.
- [`docs/V2-2B-HANDOFF.md`](file:///d:/PlayoutTranscode/docs/V2-2B-HANDOFF.md): Slice V2-2B handoff document.

### Protected Files (Untouched)
- `src/encoder.rs` (minimal/untouched)
- `src/probe.rs`
- `src/profiles.rs`
- `src/db.rs`
- `web-ui/**` (no code or asset changes)
- `installer/**`
- PlayOutVue (`d:\PlayOut`) — zero reads/writes.

---

## 6. Known Limitations
- **In-Memory Attempt Tracking**: Retries are tracked in memory during `process_file_sync`. Durable job persistence across service restarts is **not** implemented yet and is reserved for a later slice.

---

## 7. Proposed Next Slice (V2-2C)
- **Slice V2-2C**: Watcher Settling, Duplicate Event Debouncing, and Temporary File Exclusion Refinements.
