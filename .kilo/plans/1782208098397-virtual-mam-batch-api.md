# Plan: Virtual MAM — Schema Upgrade & Batch API

## Goal
Add `display_name` and `virtual_folder` columns to `media_assets`, expose rename/move REST endpoints, and build a high-performance batch-fetch endpoint for 2000+ item rundowns.

## Files Affected
| File | Changes |
|---|---|
| `src/db.rs` | Schema migration, new columns in CREATE TABLE, new query functions |
| `src/server.rs` | Three new route handlers + route registration |
| `src/processor.rs` | Pass `display_name` to `insert_processing` on initial ingestion |

---

## Task 1: Schema Migration (`src/db.rs`)

### 1a. Update CREATE TABLE
In `init_pool`, add new columns to the `CREATE TABLE IF NOT EXISTS` statement:
```sql
display_name   TEXT NOT NULL DEFAULT '',
virtual_folder TEXT NOT NULL DEFAULT '/'
```

### 1b. Idempotent ALTER TABLE for existing databases
After the CREATE TABLE, add ALTER TABLE statements that handle the "duplicate column" case:
```rust
let _ = sqlx::query("ALTER TABLE media_assets ADD COLUMN display_name TEXT NOT NULL DEFAULT ''")
    .execute(&pool).await;
let _ = sqlx::query("ALTER TABLE media_assets ADD COLUMN virtual_folder TEXT NOT NULL DEFAULT '/'")
    .execute(&pool).await;
```
Discard errors (column-already-exists). SQLite has no `IF NOT EXISTS` for `ALTER TABLE ADD COLUMN`.

### 1c. Populate `display_name` for existing rows
After the ALTER, run a one-time UPDATE that extracts the file stem from `current_path`. Approach: read all rows with empty `display_name`, use Rust to extract stems, then update in a loop. Use `std::path::Path::file_stem()` on `current_path`. For rows where `current_path` has no stem (edge case), fall back to the UUID.

```rust
let rows: Vec<(String, String)> = sqlx::query_as(
    "SELECT uuid, current_path FROM media_assets WHERE display_name = ''"
).fetch_all(&pool).await?;
for (uuid, path) in &rows {
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| uuid.clone());
    let display_name = stem.chars().take(255).collect::<String>();
    let _ = sqlx::query("UPDATE media_assets SET display_name = ?1 WHERE uuid = ?2")
        .bind(&display_name).bind(uuid).execute(&pool).await;
}
```

### 1d. Update `MediaAsset` struct
Add fields:
```rust
pub display_name: String,
pub virtual_folder: String,
```
Update all `sqlx::query_as` SELECT lists in `find_by_uuid`, `find_by_fingerprint`, and the new batch query to include both columns.

### 1e. Update `AssetResponse`
Add `display_name` and `virtual_folder` fields. Update the `From<MediaAsset>` impl.

### 1f. Update `insert_processing` signature
Add parameter `display_name: &str`:
```rust
pub async fn insert_processing(
    pool: &SqlitePool,
    uuid: &str,
    fingerprint: i64,
    path: &str,
    display_name: &str,
) -> Result<(), sqlx::Error>
```
SQL becomes:
```sql
INSERT INTO media_assets (uuid, fingerprint, current_path, display_name, status)
VALUES (?1, ?2, ?3, ?4, 'processing')
```

### 1g. New DB functions
```rust
pub async fn set_display_name(pool: &SqlitePool, uuid: &str, display_name: &str) -> Result<bool, sqlx::Error>
pub async fn set_virtual_folder(pool: &SqlitePool, uuid: &str, virtual_folder: &str) -> Result<bool, sqlx::Error>
pub async fn find_batch(pool: &SqlitePool, uuids: &[String]) -> Result<Vec<MediaAsset>, sqlx::Error>
```

### 1h. `find_batch` — dynamic IN clause
Build the query with the correct number of `?` placeholders:
```rust
let placeholders = uuids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
let sql = format!(
    "SELECT uuid, fingerprint, current_path, duration_ms, trim_in_ms, trim_out_ms, rating, status, display_name, virtual_folder FROM media_assets WHERE uuid IN ({})",
    placeholders
);
let mut query = sqlx::query_as::<_, MediaAsset>(&sql);
for uuid in uuids {
    query = query.bind(uuid);
}
query.fetch_all(pool).await
```

**Safety:** 500 UUIDs → 500 bind params. SQLite's `SQLITE_MAX_VARIABLE_NUMBER` default is 999. WAL mode already active. Well within limits.

---

## Task 2: API Handlers (`src/server.rs`)

### 2a. Validation helpers
```rust
const MAX_DISPLAY_NAME_LEN: usize = 255;

fn is_valid_virtual_folder(path: &str) -> bool {
    if path.is_empty() { return false; }
    if !path.starts_with('/') { return false; }
    if path.contains("..") { return false; }
    if path != "/" && path.ends_with('/') { return false; }
    true
}
```

### 2b. `PUT /api/assets/{uuid}/rename`
- Deserialize `RenameRequest { display_name: String }`
- Reject if empty or > 255 chars → 422
- Call `db::set_display_name()`, return updated asset or 404

### 2c. `PUT /api/assets/{uuid}/move`
- Deserialize `MoveRequest { virtual_folder: String }`
- Reject if invalid per `is_valid_virtual_folder` → 422
- Call `db::set_virtual_folder()`, return updated asset or 404

### 2d. `POST /api/assets/batch`
- Deserialize `BatchRequest(Vec<String>)` (a JSON array of UUIDs)
- Reject if > 500 UUIDs → 422 with error `"max 500 UUIDs per batch request"`
- Reject if duplicate UUIDs in array → 422 (dedup with `HashSet`)
- Call `db::find_batch()`
- Build response: `HashMap<String, AssetResponse>` — only found assets included
- If no assets match, return empty JSON object `{}`

### 2e. Route registration
Add to the API router:
```rust
.route("/assets/{uuid}/rename", put(put_rename))
.route("/assets/{uuid}/move", put(put_move))
.route("/assets/batch", post(post_batch))
```

---

## Task 3: Processor Update (`src/processor.rs`)

In `process_file_sync`, the `safe_stem` is already computed at lines 51-56. Pass it to `insert_processing`:

```rust
let _ = handle.block_on(db::insert_processing(
    pool,
    &metadata_uuid,
    fingerprint,
    &input_path.to_string_lossy(),
    &safe_stem,  // NEW: display_name
));
```

---

## Verification Checklist

| # | Check | Status |
|---|-------|--------|
| 1 | `/rename` handler only executes `UPDATE` SQL, zero `std::fs` | ✅ By design |
| 2 | `/move` handler only executes `UPDATE` SQL, zero `std::fs` | ✅ By design |
| 3 | `/batch` uses single `WHERE uuid IN (?,...,?)` query | ✅ `find_batch` builds dynamic query |
| 4 | 500 bind params well within SQLite limit (999 default) | ✅ |
| 5 | WAL mode prevents read/write contention | ✅ Already in `init_pool` |
| 6 | `display_name` ≤ 255 chars enforced server-side | ✅ |
| 7 | `virtual_folder` path validation blocks traversal | ✅ `..` rejected |
| 8 | Existing rows get `display_name` populated from `current_path` stem | ✅ One-time migration |
| 9 | `AssetResponse` gains fields additively (backward-compatible JSON) | ✅ |
| 10 | `cargo check` passes | To verify after implementation |

## Risks
- **Migration doesn't handle corrupt `current_path` values**: If `current_path` is not a valid path, `Path::file_stem()` returns `None`, fallback to UUID handles this.
- **Column-already-exists errors swallowed**: If ALTER TABLE fails for a reason other than "duplicate column", the error is silently discarded. Mitigation: log a warning with the error text.

## Open Questions
- None — all design decisions resolved.
