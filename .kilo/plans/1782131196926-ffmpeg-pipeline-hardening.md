# Plan: Robust FFmpeg Encoding Pipeline — 7 Fixes

## Problem
Longer files error with FFmpeg exit code -22 (EINVAL). Some outputs are corrupt, others play fine. Stderr diagnostics are silently discarded. Root causes span probe size limits, frame-rate handling, deprecated x264 flags, and missing output validation.

## Files Affected
| File | Changes |
|---|---|
| `src/encoder.rs` | Stderr ring-buffer capture, enriched error messages |
| `src/profiles.rs` | 5 fixes: probesize, genpts, vsync/fps, remove ilme/cgop flags, muxing queue |
| `src/processor.rs` | Output duration validation |
| `src/config.rs` | New `probesize` + `analyzeduration` config fields |

---

## Fix 1: Capture full FFmpeg stderr for diagnostics
**File:** `src/encoder.rs`

**Problem:** stderr is consumed line-by-line for progress parsing, every line is dropped. On failure, only `"ffmpeg exited with code Some(-22)"` is reported.

**Change:**
- Replace `BufReader::new(stderr).lines().flatten()` loop with manual read using `BufReader::read_line`
- Maintain a `Vec<String>` ring buffer (last 200 lines)
- On exit code != 0: include last 50 stderr lines in `EncodeResult.error`
- On exit code == 0: log all captured lines at `tracing::debug!` (not info, to avoid noise)

New struct fields or local variables in `transcode_file`:
```rust
let mut stderr_lines: Vec<String> = Vec::new();
const STDERR_RING_SIZE: usize = 200;
// In the read loop, push each line; if > STDERR_RING_SIZE, remove oldest
```

Error formatting:
```rust
let mut error_msg = format!("ffmpeg exited with code {:?}", status.code());
if !stderr_lines.is_empty() {
    let tail = if stderr_lines.len() > 50 { &stderr_lines[stderr_lines.len()-50..] } else { &stderr_lines };
    error_msg.push_str("\n--- FFmpeg stderr (last lines) ---\n");
    for line in tail { error_msg.push_str(line); error_msg.push('\n'); }
}
```

---

## Fix 2: Increase probe sizes for long files (configurable)
**Files:** `src/profiles.rs`, `src/config.rs`

**Problem:** `-analyzeduration 100M -probesize 100M` insufficient for 2+ hour files. Partial stream analysis produces corrupt frame timing or -22 on muxer inconsistency.

**Change in `config.rs` (`EncodingConfig`):**
```rust
#[serde(default = "default_probesize")]
pub probesize: String,          // e.g. "500M"
#[serde(default = "default_analyzeduration")]
pub analyzeduration: String,    // e.g. "500M"

fn default_probesize() -> String { "500M".into() }
fn default_analyzeduration() -> String { "500M".into() }
```

**Change in `profiles.rs` (`build_ffmpeg_args`):**
Replace hardcoded `"100M"` with `config.encoding.probesize` and `config.encoding.analyzeduration`.

Also update `Default` impl, `run_wizard`, `get_config` handler, and `validate` in `config.rs` to include the new fields.

---

## Fix 3: Add `-fflags +genpts` for broken source timestamps
**File:** `src/profiles.rs`

**Problem:** Files from broadcast servers, NLEs, or capture cards often have missing/broken PTS. FFmpeg needs `+genpts` to generate stable timestamps before encoding begins.

**Change in `build_ffmpeg_args`:** Insert before `-i`:
```rust
"-fflags".to_string(), "+genpts".to_string(),
"-i".to_string(), input_path.to_string(),
```

Remove the standalone `-i` and `input_path` from the initial vec, move them to the extended slice with `+genpts`.

---

## Fix 4: Use `-vsync cfr` + `fps=25` filter instead of bare `-r 25`
**File:** `src/profiles.rs`

**Problem:** `-r 25` without explicit vsync defaults to `-vsync vfr`, causing unpredictable frame drop/duplication. On long non-25fps source files (29.97, 23.976, VFR), this produces corrupt cadence that CasparCG can't handle.

**Change in `build_ffmpeg_args`:**
1. Remove `"-r".to_string(), "25".to_string()` from the initial `args` vec
2. Add `fps=25,` prefix to the vf chain in `build_vf()` (before `scale=...`)
3. Add `"-vsync".to_string(), "cfr".to_string()` before the encoder args (e.g., before `-c:v`)

`build_vf()` change:
```rust
fn build_vf(&self) -> String {
    let mut vf = String::from("fps=25,");
    // ... rest of existing filter chain appended ...
}
```

---

## Fix 5: Remove deprecated `ilme` flag, consolidate x264 params
**File:** `src/profiles.rs`

**Problem:** `ilme` was removed from x264 in 2024. Using it causes -22 (invalid argument) on modern FFmpeg. `cgop` and `ildct` should use `-x264-params` syntax.

**Change in `build_ffmpeg_args`:**
Replace the entire `-flags` block (lines 128-136):

For **interlaced** (`ProfileB`): Remove `-flags +ilme+ildct+cgop`. Keep `-top 1 -field_order tt`. Add `interlaced=1` to x264-params:
```rust
// Instead of -flags +ilme+ildct+cgop:
args.extend_from_slice(&[
    "-top".to_string(), "1".to_string(),
    "-field_order".to_string(), "tt".to_string(),
]);
```

For **progressive** (`ProfileA`, `ProfileC`): Remove `-flags +cgop` block entirely (already enforced by `-x264-params open-gop=0` and `-sc_threshold 0`).

Update the `-x264-params` string (line 145) to include interlaced hint when profile is interlaced:
```rust
let x264_params = if self.interlaced {
    "open-gop=0:interlaced=1:pic-struct=1"
} else {
    "open-gop=0"
};
args.extend_from_slice(&["-x264-params".to_string(), x264_params.to_string()]);
```

---

## Fix 6: Validate output duration vs source after transcode
**File:** `src/processor.rs`

**Problem:** FFmpeg can exit code 0 with a truncated output (disk full, incomplete stream). No validation checks that the output duration matches the input.

**Change in `process_file_sync`** (after the `result.success` block, before marking ready):

```rust
// After output_probe
let output_duration = output_probe.duration_secs;
let source_duration = probe_data.duration_secs;
let tolerance = (source_duration * 0.05).max(2.0); // 5% or 2 sec minimum

if (output_duration - source_duration).abs() > tolerance {
    tracing::error!(
        "Duration mismatch: source={}s output={}s (tolerance={}s)",
        source_duration, output_duration, tolerance
    );
    // Mark as failed instead of ready
    queue.update(&job.id, |j| {
        j.state = jobs::JobState::Failed;
        j.error = Some(format!(
            "Duration mismatch: source {}s vs output {}s",
            source_duration, output_duration
        ));
        j.finished_at = Some(Utc::now().to_rfc3339());
    });
    let _ = handle.block_on(db::mark_error(pool, &metadata_uuid));
    let _ = std::fs::remove_file(&result.output_path);
    queue.prune_old(500);
    return;
}
```

Place this check right after the `output_probe` assignment, before `write_sidecar_next_to_video`.

---

## Fix 7: Add `-max_muxing_queue_size 4096` for muxer stability
**File:** `src/profiles.rs`

**Problem:** On long files with audio/video sync drift, FFmpeg's mp4 muxer accumulates packets and eventually errors with "Too many packets buffered for output stream". Increasing the queue prevents this.

**Change in `build_ffmpeg_args`:** Insert before the output path:
```rust
"-max_muxing_queue_size".to_string(), "4096".to_string(),
```

Add this after the audio args, before the output path is pushed.

---

## Final FFmpeg Argument Order (after all fixes)
```
-y -hide_banner -loglevel info -stats
-analyzeduration <config> -probesize <config>
-fflags +genpts -i <input>
-vf fps=25,scale=...,pad=...,setsar=...,format=yuv420p
-vsync cfr
-c:v libx264 -preset <preset> [-tune <tune>]
-crf <crf> -maxrate <maxrate> -bufsize <bufsize>
-profile:v <profile> -level:v <level>
-pix_fmt yuv420p
-colorspace <cs> -color_trc <trc> -color_primaries <prim>
[-top 1 -field_order tt]  (interlaced only)
-g 50 -keyint_min 50 -sc_threshold 0
-x264-params open-gop=0[:interlaced=1:pic-struct=1]
-movflags +faststart
[-threads <N>]
-map 0:v:0 -map 0:a:0?
-c:a <codec> [-b:a <bitrate>] -ar 48000 -ac 2
-max_muxing_queue_size 4096
<output_path>
```

## Validation
1. `cargo check` — no errors
2. Test with a known-long file that previously produced -22
3. Verify stderr is logged when FFmpeg fails (check tracing output)
4. Verify new config fields appear in `GET /api/config` response
5. Verify output duration mismatch triggers a `Failed` state in job records

## Open Questions
- None — all 7 fixes are scoped and the approach is clear
