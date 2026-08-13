# V2-2C Handoff — Watcher Settling and Temporary File Exclusion Refinements

## 1. Overview
Slice V2-2C implements **Watcher Settling, Duplicate Event Debouncing, and Temporary File Exclusion Refinements** for PlayoutTranscode. It hardens media candidate ingestion against partial transfers, active browser/FTP downloads, editor temporary files, hidden files, and sidecar metadata JSON files.

---

## 2. Baseline & HEAD Information
- **V2-2C Final HEAD SHA**: `045f232211ecc9ae5c897bae5f487369eb0acdaa`
- **Branch**: `main`
- **Scope**: V2-2C Watcher Settling & Temporary File Filtering only.

---

## 3. Implementation Details

### A. Extended Temporary & In-Flight File Filtering (`is_temp_file_name`)
Files meeting any of the following criteria are immediately ignored by both recursive directory scanning (`collect_candidates`) and filesystem notify event handlers (`watch_loop`):
1. **Hidden files and temporary prefixes**: Starting with `.`, `.tmp_`, or `~` (e.g., `~$video.mp4`, `.hidden.mp4`, `.DS_Store`).
2. **Sidecar metadata files**: Ending with `.uuid.json` or `.json` (e.g., `clip.mp4.uuid.json`, `metadata.json`).
3. **In-flight / partial download extensions**:
   - `.part`, `.partial`
   - `.crdownload`, `.download`
   - `.tmp`, `.temp`, `.filepart`
   - `.upload`, `.incomplete`

### B. Deterministic Settling & Debouncing
- **Candidate Size & Timestamp Stability**: Tracks `stable_polls` across polling intervals (`poll_secs`).
- **Settling Delay**: Verifies `now_secs - modified_epoch_secs >= settle_secs` before candidate admission.
- **In-Flight Identity Tracking**: Caches `(size, modified_epoch_secs)` in `queued` map to prevent duplicate dispatch of already-queued files.
- **Non-exclusive Read Locking**: Evaluates `is_file_available_for_reading` before sending candidates across the processing channel.

---

## 4. Verification Results
1. **`cargo check`**: Clean compilation with 0 errors and 0 warnings.
2. **`cargo test`**: **59 total tests passed** across all workspace test suites:
   - 41 unit tests (including 6 new watcher unit tests in `src/watcher.rs`).
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite frontend bundle compiled successfully in 3.59s.

---

## 5. Changed & Protected Files

### Changed Files
- [`src/watcher.rs`](file:///d:/PlayoutTranscode/src/watcher.rs): Extended `is_temp_file_name`, `TEMP_EXTENSIONS`, candidate collection filtering, and unit tests.
- [`docs/V2-2C-HANDOFF.md`](file:///d:/PlayoutTranscode/docs/V2-2C-HANDOFF.md): Slice V2-2C handoff document.

### Protected Files (Untouched)
- `src/encoder.rs`, `src/probe.rs`, `src/profiles.rs`, `src/db.rs`
- `web-ui/**` (no source changes)
- `installer/**`
- PlayOutVue (`d:\PlayOut`) — 0 modifications.

---

## 6. Proposed Next Slice (V2-2D)
- **Slice V2-2D**: Subclip-Safe Purge Policy (`purge_asset` with subclip preservation guarantees).
