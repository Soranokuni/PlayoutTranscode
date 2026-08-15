# Database Viewer & Verified Source Cleanup Handoff

## Summary of Changes

### 1. Read-Only Database Viewer Backend & Frontend
- **Safe Introspection Endpoints**: Added fixed, parameterized, read-only endpoints on `/api/db/*` and `/api/v2/db/*`:
  - `GET /api/v2/db/overview`: High-level counts for active assets, master clips, virtual subclips, trashed assets, durable jobs, DB page size, and WAL mode.
  - `GET /api/v2/db/assets`: Paginated assets with search and filter (`all`, `master`, `subclip`, `ready`, `processing`, `error`, `trashed`).
  - `GET /api/v2/db/assets/{uuid}`: Detailed asset inspection with first 100 keyframe offsets and full QC warnings.
  - `GET /api/v2/db/jobs`: Paginated transcode jobs with state filter and full-text search.
  - `GET /api/v2/db/jobs/{id}`: Detailed job inspection with tail of stderr logs, worker lease, and attempt counts.
  - `GET /api/v2/db/folders`: Virtual folder summary with color tags, ready count, and trashed count.
  - `GET /api/v2/db/schema`: SQLite schema introspection using `PRAGMA table_info` for `media_assets`, `transcode_jobs`, and `virtual_folder_colors`.
- **Database Viewer Web UI (`DbViewer.vue`)**:
  - Embedded as a dedicated "Database" tab in `App.vue`.
  - Filter pills, live debounced search, responsive tables, formatted durations/timecodes, and inspect modals.

### 2. Verified Post-Publication Source Cleanup Policy
- **Fail-Closed Opt-in Safety**: Source deletion only runs if `clean_source_after_success: true` is configured in `IngestionConfig` / `StoragePolicy`.
- **Execution Order**: Runs strictly after mezzanine transcode succeeds, stream probing and QC checks pass, atomic file rename finishes, sidecar is written, and `db::mark_ready` succeeds.
- **Safety Checks**:
  1. Policy enabled check.
  2. Reject empty path, root directory, and parent directory traversal (`..`).
  3. Reject directories (files only).
  4. Ensure canonical source path is within canonical watch folder root.
  5. Reject if source path matches or resides inside target mezzanine folder.
  6. Pre-deletion verification: verify file size and modified timestamp match initial capture (detects in-flight replacement).
  7. In-memory queue collision check: reject deletion if another queued or processing job targets the same source file.
  8. Non-fatal failure handling: If deletion fails, warning is recorded (`source_cleanup_failed`), asset remains ready, and source file is retained.

## Verification
- Unit & Integration tests: 125 tests passing (`cargo test`).
- Web UI compilation: `vue-tsc --build && vite build` built with 0 errors.
