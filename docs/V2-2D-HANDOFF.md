# V2-2D Handoff — Subclip-Safe Purge Policy

## 1. Overview
Slice V2-2D implements **Subclip-Safe Purge Policy** for PlayoutTranscode. Deleting an asset row via API (`DELETE /api/assets/{uuid}/purge`) or cleanup sweep checks for remaining database references (e.g., virtual subclips, sibling records, or parent assets) pointing to the same physical mezzanine path before removing any physical media or sidecar files.

---

## 2. Baseline & HEAD Information
- **V2-2D Final HEAD SHA**: `929503240d07cdec507edf3351e54ea27d775f56`
- **Branch**: `main`
- **Scope**: V2-2D Subclip-Safe Purge Policy only.

---

## 3. Implementation Details

### A. Purge Decision Model (`PurgeMode`)
- `PurgeMode::PreserveReferencedMezzanine`: Safe default mode. Purges the specific asset row by UUID and only deletes the physical mezzanine file and sidecar JSON if `count_rows_by_path(pool, &path) == 0`.
- `PurgeMode::DeleteUnreferencedMezzanine`: Configured when storage policy `preserve_subclips_on_purge = false`.

### B. Reference Protection & Sidecar Invariants
1. **Physical File Protection**:
   - `count_rows_by_path` verifies whether any other asset or subclip in `media_assets` shares the same `current_path`.
   - If other references exist, the physical media file (`.mp4`) and sidecar file (`.uuid.json`) are preserved untouched on disk.
2. **Staging File Safety**:
   - `is_temp_file_name` ensures temporary staging files (`.tmp_*.mp4`) are never accidentally deleted by asset purge sweeps.
3. **Atomic Sidecar Removal**:
   - The sidecar file is deleted only when the final database reference to that mezzanine is removed.

---

## 4. Verification Results
1. **`cargo check`**: Clean compilation with 0 errors and 0 warnings.
2. **`cargo test`**: **63 total tests passed** across all workspace test suites:
   - 45 unit tests (including 4 new subclip-safe purge unit tests in `src/db.rs`).
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite frontend bundle compiled successfully in 3.02s.

---

## 5. Changed & Protected Files

### Changed Files
- [`src/db.rs`](file:///d:/PlayoutTranscode/src/db.rs): Added `PurgeMode`, updated `purge_asset_with_mode`, sidecar handling, and subclip preservation unit tests.
- [`src/server.rs`](file:///d:/PlayoutTranscode/src/server.rs): Connected `delete_purge_asset` to `effective_storage_policy().preserve_subclips_on_purge`.
- [`docs/V2-2D-HANDOFF.md`](file:///d:/PlayoutTranscode/docs/V2-2D-HANDOFF.md): Slice V2-2D handoff document.

### Protected Files (Untouched)
- `src/encoder.rs`, `src/probe.rs`, `src/profiles.rs`, `src/processor.rs`, `src/watcher.rs`
- `web-ui/**` (no source changes)
- `installer/**`
- PlayOutVue (`d:\PlayOut`) — 0 modifications.

---

## 6. Proposed Next Slice (V2-3)
- **Slice V2-3**: Audio Normalization and Multi-Channel Audio Policies (Loudness EBU R128 / BS.1770-4 normalization, channel layout mapping, and audio validation).
