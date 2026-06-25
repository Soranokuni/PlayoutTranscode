# Plan: Fix FFmpeg `-colorspace bt601` → exit code -22

## Problem
FFmpeg transcodes of **720×576 SD PAL** files fail with exit code `Some(-22)`.
Root cause (confirmed in `logs/logs from ingestor webuit.txt`): these files route to
**Profile C** (`probe.rs:43-45`, height ≤ 900), whose color constants are `bt601`
(`profiles.rs:67-69`). libx264 rejects `bt601`:

```
[libx264] Undefined constant or missing '(' in 'bt601'
[libx264] Error setting option colorspace to value bt601.
Error applying encoder options: Invalid argument
```

HD files use Profile A/B (`bt709`, valid) and succeed.

## Key Technical Finding
The `-colorspace`/`-color_trc`/`-color_primaries` FFmpeg AVOptions resolve against the
`AVColorSpace`/`AVColorTransferCharacteristic`/`AVColorPrimaries` enums. Valid names are
`bt709`, `smpte170m`, `bt470bg`, `smpte240m`, etc. **`bt601`, `bt601ntsc`, and `bt601pal`
are NOT valid** for these AVOptions. (`bt601ntsc`/`bt601pal` exist only as options to the
`colormatrix` *filter*, not the `-colorspace` output option.)

The error originates in **libx264's own private `colorspace` option**, which accepts only
`undef | bt709 | fcc | smpte170m | smpte240m | YCgCo`. The only BT.601 constant valid in
**both** the FFmpeg enum **and** libx264's private option is `smpte170m` (SMPTE 170M = the
BT.601 standard). This is the value to use.

## Files Affected
| File | Changes |
|---|---|
| `src/profiles.rs` | Fix PROFILE_C constants (`bt601`→`smpte170m`); add color-constant validator |
| `src/db.rs` | Boot-time cleanup of orphaned `processing` rows in `init_pool` |
| `src/main.rs` | Call the color validator at startup |

---

## Task 1: Fix Profile C color constants (`src/profiles.rs`)
Change `PROFILE_C` (lines 67-69):
```rust
colorspace: "smpte170m",
color_trc: "smpte170m",
color_primaries: "smpte170m",
```
Profiles A/B stay `bt709` (valid, no change). After this, no `bt601`-like raw tag remains.

## Task 2: Color-constant validator (recurrence guard) (`src/profiles.rs`)
Add a function that checks all three profiles against FFmpeg/libx264-valid whitelists and
fails fast if an unknown value (e.g. `bt601`, `bt601ntsc`, `bt601pal`) ever reappears:

```rust
const VALID_COLORSPACE: &[&str] = &["undef", "bt709", "smpte170m", "smpte240m"];
const VALID_COLOR_TRC: &[&str] = &["undef", "bt709", "smpte170m", "smpte240m", "bt470bg", "linear", "smpte2084", "bt2020-10", "bt2020-12", "iec61966-2-1", "arib-std-b67"];
const VALID_COLOR_PRIMARIES: &[&str] = &["undef", "bt709", "smpte170m", "smpte240m", "bt470bg", "film", "bt2020", "smpte431", "smpte432", "jedec-p22"];

pub fn validate_color_constants() -> Result<(), String> {
    for id in [ProfileId::ProfileA, ProfileId::ProfileB, ProfileId::ProfileC] {
        let p = EncodingProfile::by_id(id);
        if !VALID_COLORSPACE.contains(&p.colorspace) {
            return Err(format!("{}: invalid colorspace '{}'", id, p.colorspace));
        }
        if !VALID_COLOR_TRC.contains(&p.color_trc) {
            return Err(format!("{}: invalid color_trc '{}'", id, p.color_trc));
        }
        if !VALID_COLOR_PRIMARIES.contains(&p.color_primaries) {
            return Err(format!("{}: invalid color_primaries '{}'", id, p.color_primaries));
        }
    }
    Ok(())
}
```
Note: `VALID_COLORSPACE` is intentionally the intersection of FFmpeg enum names AND libx264's
private `colorspace` option values, since `-colorspace` may be routed to libx264. This is the
strict set that guarantees no -22 from either layer.

Wire into startup in `src/main.rs` `run_service`, right after config load (after line 94
`logging::init_logging`):
```rust
profiles::EncodingProfile::validate_color_constants()
    .map_err(|e| anyhow::anyhow!("Color constant misconfiguration: {}", e))?;
```
A bad const is a code bug (these are compile-time `&'static str`), so aborting startup is
correct — every transcode would otherwise fail.

## Task 3: Boot database cleanup (`src/db.rs` `init_pool`)
After table creation/index (and the existing display_name migration block), add an orphan
recovery UPDATE so the system never gets stuck on rows left `processing` by a crash:
```rust
let result = sqlx::query(
    "UPDATE media_assets SET status = 'error' WHERE status = 'processing'",
)
.execute(&pool)
.await?;
if result.rows_affected() > 0 {
    tracing::warn!(
        "Recovered {} orphaned asset row(s) left in 'processing' state (marked 'error')",
        result.rows_affected()
    );
}
```
Rationale: orphaned `processing` rows are never retried and never complete; marking them
`error` makes them visible without silently re-ingesting. Partial output files on disk from a
crash are out of scope (we don't track which are partial; deleting blindly risks valid files).

## Task 4: Safe failure transitions — VERIFIED, NO CHANGE
`src/processor.rs` already handles non-zero FFmpeg exit correctly:
- `else` branch (lines 173-184): `db::mark_error(pool, &metadata_uuid)` (atomic single
  `UPDATE media_assets SET status='error' WHERE uuid=?`) + `std::fs::remove_file(&result.output_path)`
  to delete the partial output.
- Duration-mismatch branch (lines 144-156): same `mark_error` + partial-file removal.
- Probe-failure (line 90) and profile-disabled (line 103): `mark_error`.

No gaps. The `mark_error` UPDATE is atomic by virtue of being a single SQL statement under
SQLite's serialized writer. (Minor note for implementer: the `let _ =` on `mark_error` swallows
DB errors; acceptable since WAL + short transactions make this rare, and the boot cleanup in
Task 3 is the safety net.)

---

## Audit Checklist
| # | Check | Status |
|---|-------|--------|
| 1 | No raw `bt601`/`bt601ntsc`/`bt601pal` tag in generated FFmpeg args | ✅ Profile C → `smpte170m` |
| 2 | Profiles A/B color tags valid (`bt709`) | ✅ Unchanged, valid |
| 3 | Validator rejects any future unmapped color tag at startup | ✅ Task 2 |
| 4 | Non-zero FFmpeg exit flips DB row to `error` (atomic) | ✅ processor.rs (verified) |
| 5 | Partial output file removed on failure | ✅ processor.rs (verified) |
| 6 | Boot recovers orphaned `processing` rows | ✅ Task 3 |
| 7 | `cargo check` passes | To verify after implementation |

## Validation
1. `cargo check` — no errors.
2. Grep `bt601` in `src/` — expect zero matches in profiles.rs.
3. Re-transcode one of the failing 720×576 files ("A Year In The Wild no1…") — expect success.
4. Confirm output MP4 color metadata reports `smpte170m` (via `ffprobe ... -show_streams`).
5. Kill the daemon mid-transcode, restart → confirm the orphaned row is now `error` in the DB.

## Open Questions
- None.
