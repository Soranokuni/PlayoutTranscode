# PlayoutTranscode Recycle Bin & Storage Purge Handoff

## 1. Overview
PlayoutTranscode now provides authoritative soft-delete (Recycle Bin), directory-boundary-safe folder deletion, restoration with root fallback, reference-protected physical file purging, and scheduled auto-purge endpoints.

## 2. Schema Additions
In SQLite table `media_assets`:
- `deleted_at TEXT DEFAULT NULL`: ISO 8601 UTC timestamp when soft-deleted, or `NULL` if active.
- `original_virtual_folder TEXT DEFAULT NULL`: Preserves the pre-trash virtual folder path for reliable restoration.
- Index: `CREATE INDEX IF NOT EXISTS idx_media_assets_deleted_at ON media_assets(deleted_at)`.

## 3. Endpoints Added

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/recycle-bin` | List all trashed assets (`deleted_at IS NOT NULL`) sorted by `deleted_at DESC`. |
| `POST/PUT` | `/api/assets/{uuid}/trash` | Soft delete active asset (`deleted_at = now()`, saves `original_virtual_folder`). |
| `POST/PUT` | `/api/folders/trash` | Soft delete all active assets under virtual folder & subfolders. Boundary protected (e.g. `/Shows/Drama` will not match `/Shows/Dramatic`). |
| `POST/PUT` | `/api/assets/{uuid}/restore` | Restore asset from Recycle Bin. Optional `{ "target_folder": "/..." }` payload. |
| `POST/PUT` | `/api/folders/restore` | Restore all assets originating from folder. Optional `{ "fallback_to_root": true }` if parent folder is missing. |
| `DELETE` | `/api/assets/{uuid}/purge` | Permanently delete DB row, physical mezzanine, and `.uuid.json` sidecar (if reference count == 0). Path validated. |
| `DELETE` | `/api/folders/purge` | Permanently purge all trashed assets matching virtual folder. |
| `DELETE` | `/api/recycle-bin/purge` | Empty Recycle Bin (purges all trashed items and unreferenced files). |
| `POST` | `/api/recycle-bin/auto-purge` | Execute scheduled auto-purge for retention policies (`1week`, `2weeks`, `3weeks`, `1month`). |

*(Mirrored on `/api/v2/*`)*

## 4. Verification
- 91 unit tests passing (`cargo test`)
- 10 contract boundary tests passing (`cargo test --test contract_boundary`)
- 5 reliability chaos tests passing (`cargo test --test reliability_chaos`)
- 10 wire contract tests passing (`cargo test --test v1_wire_contract`)
