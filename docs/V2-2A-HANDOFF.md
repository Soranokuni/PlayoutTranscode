# V2-2A Handoff — Atomic Output Staging and Publication

## 1. Overview
Slice V2-2A implements **Atomic Output Staging and Publication** for PlayoutTranscode. Transcode outputs and UUID sidecars are written to temporary staging files (`.tmp_...mp4` and `.tmp_...uuid.json`) inside the target directory. Staging files are probed and validated completely before being atomically renamed into final production output paths. `db::mark_ready` is invoked **only after** the final published file exists and is verified.

---

## 2. Baseline & HEAD Information
- **V2-2A Implementation SHA**: `06abb1c64eb3a48bfdbdeaa1b94d9ce6a21ec6bf`
- **Branch**: `main`
- **Scope**: V2-2A Atomic Staging and Publication PR only.

---

## 3. Implementation Details
1. **`Publisher` Trait & `LocalFilePublisher` Implementation** ([`src/processor.rs`](file:///d:/PlayoutTranscode/src/processor.rs)):
   - Defines a `Publisher` abstraction with `stage_path`, `publish`, and `cleanup_staging`.
   - `LocalFilePublisher` constructs `.tmp_{uuid}_{filename}` staging paths on the same volume as the final target video directory.
   - Enforces atomic file renaming (`std::fs::rename`) and prevents blind overwrites if the final file already exists.
2. **Staging -> Probe -> Validation -> Rename Sequence**:
   - Transcode output writes to `staged_output_path`.
   - Keyframe scan, GOP validation, audio sample rate checks, and faststart checks operate on `staged_output_path`.
   - On validation success: `publisher.publish(&staged_output_path, &final_output_path)` moves the file into `final_output_path`.
   - Sidecar JSON is written to `.tmp_...uuid.json` and atomically renamed to `.uuid.json`.
   - `db::mark_ready` is called **only after** `final_output_path` exists.
3. **Failure & Staging Cleanup**:
   - Staging files and temporary sidecars are cleaned up on all failure paths (transcode error, probe error, validation failure, publish failure, or early exit).
4. **Watcher Exclusion** ([`src/watcher.rs`](file:///d:/PlayoutTranscode/src/watcher.rs)):
   - Updated `collect_candidates` and notify event loops to ignore files starting with `.` or `.tmp_`.

---

## 4. Verification Results
1. **`cargo check`**: Clean compilation with 0 errors.
2. **`cargo test`**: **50 total tests passed** across the workspace:
   - 32 unit tests (including 3 new `Publisher` unit tests in `src/processor.rs`).
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite bundle generated without errors.

---

## 5. Changed Files & Protected File Verification
### Changed Files
- [`src/processor.rs`](file:///d:/PlayoutTranscode/src/processor.rs): Added `Publisher` trait, `LocalFilePublisher`, atomic staging/publishing sequence, and `Publisher` unit tests.
- [`src/identity.rs`](file:///d:/PlayoutTranscode/src/identity.rs): Atomic sidecar file writing (staging -> rename).
- [`src/watcher.rs`](file:///d:/PlayoutTranscode/src/watcher.rs): Exclude `.tmp_` files from candidate collection and notify events.
- [`docs/V2-2A-HANDOFF.md`](file:///d:/PlayoutTranscode/docs/V2-2A-HANDOFF.md): Slice V2-2A handoff document.

### Protected Files (Untouched)
- `src/encoder.rs`, `src/probe.rs`, `src/profiles.rs`, `src/db.rs`
- `web-ui/**` (no code or asset changes)
- `installer/**`
- PlayOutVue (`d:\PlayOut`) — zero reads/writes performed.

---

## 6. Proposed Next Slice (V2-2B)
- **Slice V2-2B**: Retry Classification and Bounded Retry Execution (retryable vs non-retryable error classification with bounded exponential/linear backoff and attempt tracking).
