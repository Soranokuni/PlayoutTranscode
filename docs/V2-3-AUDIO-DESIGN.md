# V2-3 Audio Pipeline Audit and Normalization Design

## 1. Overview & Baseline
- **Slice**: V2-3A — Audio Pipeline Audit and Normalization Design (Design Document Only, No Code Changes)
- **Baseline HEAD SHA**: `df73c7508f20fcac69702a1959de0076df1ba297`
- **Branch**: `main`
- **Scope**: Comprehensive audit of the current audio pipeline, detailed technical design for ITU-R BS.1770-4 / EBU R128 / ATSC A/85 two-pass loudness normalization, multi-channel handling, and verification of zero code drift.

---

## 2. Current Audio Pipeline Audit

### 2.1 Audio Encoding Decision Points (`src/profiles.rs` & `src/encoder.rs`)
In the current implementation, all audio encoding arguments are generated in `src/profiles.rs:198-225`:
```rust
args.extend_from_slice(&[
    "-map".to_string(), "0:v:0".to_string(),
    "-map".to_string(), "0:a:0?".to_string(),
]);

let audio_codec = &config.encoding.audio_codec;
args.extend_from_slice(&[
    "-c:a".to_string(), audio_codec.clone(),
]);

if audio_codec == "pcm_s16le" {
    args.extend_from_slice(&[
        "-ar".to_string(), "48000".to_string(),
        "-ac".to_string(), "2".to_string(),
    ]);
} else if audio_codec == "libmp3lame" {
    args.extend_from_slice(&[
        "-b:a".to_string(), config.encoding.audio_bitrate.clone(),
        "-ar".to_string(), "48000".to_string(),
        "-ac".to_string(), "2".to_string(),
    ]);
} else {
    args.extend_from_slice(&[
        "-b:a".to_string(), config.encoding.audio_bitrate.clone(),
        "-ar".to_string(), "48000".to_string(),
        "-ac".to_string(), "2".to_string(),
        "-async".to_string(), "1".to_string(),
    ]);
}
```

#### Exact Emitted FFmpeg Audio Arguments by Codec:
1. **`aac` (Default)**:
   `-map 0:a:0? -c:a aac -b:a 320k -ar 48000 -ac 2 -async 1`
2. **`pcm_s16le`**:
   `-map 0:a:0? -c:a pcm_s16le -ar 48000 -ac 2`
3. **`libmp3lame`**:
   `-map 0:a:0? -c:a libmp3lame -b:a 320k -ar 48000 -ac 2`

### 2.2 Channel Layout & Downmix Handling
- **Mapping**: `-map 0:a:0?` maps the first audio stream if present. If the input contains no audio, encoding succeeds without an audio stream.
- **Stereo Enforcement (`-ac 2`)**:
  - **Mono (1 channel)**: FFmpeg's default `swresample` duplicates mono to left and right channels (`dual mono`).
  - **Stereo (2 channels)**: Preserved as discrete left and right channels at 48 kHz.
  - **Multi-channel (5.1 surround, 6 channels)**: FFmpeg applies its internal default downmixing coefficients:
    $$\text{FL}_{\text{out}} = \text{FL} + 0.7071 \cdot \text{FC} + 0.7071 \cdot \text{BL}$$
    $$\text{FR}_{\text{out}} = \text{FR} + 0.7071 \cdot \text{FC} + 0.7071 \cdot \text{BR}$$
    LFE (Low Frequency Effects, subwoofer) is discarded by default.
  - **7.1 surround (8 channels)**: Downmixed with default panning coefficients without dialogue gain protection.

### 2.3 Current Loudness Processing Status
- **Zero loudness processing**: There are no audio filters (`-af`), no `loudnorm`, `ebur128`, `volume`, or `dynaudnorm` filters anywhere in `src/profiles.rs`, `src/encoder.rs`, or `src/processor.rs`.
- Audio levels remain identical to source amplitude except for resampling/downmixing attenuation.

### 2.4 Policy Model Derivation (`src/config.rs`)
In `src/config.rs:228-272` & `717-738`:
- If `[policies.audio]` is missing in `config.toml`, `config.effective_audio_policy()` derives a default `AudioPolicy`:
  - `mode = AudioMode::LegacyV1Encode`
  - `codec = config.encoding.audio_codec` (default `"aac"`)
  - `bitrate = config.encoding.audio_bitrate` (default `"320k"`)
  - `sample_rate_hz = 48000`
  - `channels = 2`
  - `channel_layout = None`
  - `target_lufs = None`
  - `true_peak_dbtp = None`
  - `lra_target = None`
  - `dual_mono = false`
  - `preserve_original = false`
- When explicit `[policies.audio]` is configured with `mode = "ebu_r128"` or `mode = "atsc_a85"`, `effective_audio_policy()` surfaces the targeted broadcast parameters.

### 2.5 Probing & Analysis Seam (`src/probe.rs`)
- `probe::probe_media(tools, input_path)` executes `ffprobe -v quiet -print_format json -show_streams -show_format <input_path>`.
- It captures `audio_codec`, `audio_sample_rate`, and `audio_channels`.
- **Seam Location**: Loudness measurement requires running `ffmpeg` with `-af loudnorm=...:print_format=json -f null -`. This measurement belongs in `src/processor.rs` immediately after `probe::probe_media` during the **Probing / Analysis stage** and *before* entering the transcode attempt loop.

### 2.6 Downstream Playout & Contract Constraints (`PlayOutVue` / `CasparCG`)
- **CasparCG Media Player**: Expects 48 kHz audio. Non-48k audio can cause audio/video sync drift or buffer underflows on DeckLink SDI outputs.
- **Contract Boundary Test (`tests/contract_boundary.rs`)**:
  - Requires `mezzanine_ok = true` only when audio sample rate is exactly 48,000 Hz.
  - Requires `duration_ms` of output file to match video frame boundaries. Audio filter graphs must not alter video timeline duration or cause container truncation.

---

## 3. V2-3 Normalization Technical Design

```
[ Ingest Candidate ]
         │
         ▼
 ┌───────────────┐
 │ Probe Media   │ ── (ffprobe: codec, channels, sample rate)
 └───────┬───────┘
         │
         ▼
 ┌───────────────────────────────────────────────┐
 │ Audio Policy Check:                           │
 │   - LegacyV1Encode ──► Skip measurement       │
 │   - EbuR128/AtscA85 ──► Run Pass 1 Measure    │
 └───────┬───────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────┐
 │ Pass 1: Audio Measurement (FFmpeg -f null)    │
 │   - Extract input_i, input_tp, input_lra,     │
 │     input_thresh, target_offset               │
 └───────┬───────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────┐
 │ Transcode Execution Loop (with Retries)       │
 │   - Pass 2 Filter: loudnorm (linear=true)     │
 │   - Audio pan/downmix matrix if 5.1           │
 │   - Output: 48 kHz stereo (or discrete 5.1)   │
 └───────┬───────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────┐
 │ Output Validation & Publication               │
 │   - Verify 48 kHz sample rate                 │
 │   - Embed loudness metadata in sidecar JSON   │
 │   - Atomic publish & DB mark_ready            │
 └───────────────────────────────────────────────┘
```

### 3.1 Loudness Targets & Broadcast Standard Mapping

| Standard | Mode Enum | Target Integrated ($I$) | Max True Peak ($TP$) | Target LRA | Typical Application |
|---|---|---|---|---|---|
| **EBU R128** (Default) | `AudioMode::EbuR128` | **-23.0 LUFS** | **-1.0 dBTP** | **7.0 LU** | European broadcast, CasparCG playout |
| **ATSC A/85** | `AudioMode::AtscA85` | **-24.0 LKFS** | **-2.0 dBTP** | **11.0 LU** | North American broadcast (CALM Act) |
| **Custom / Config** | Any with overrides | `target_lufs` | `true_peak_dbtp` | `lra_target` | Custom studio delivery specifications |

### 3.2 Exact Two-Pass Filter Graphs

#### Pass 1: Loudness Measurement (Analysis Phase)
Executed once prior to transcoding. Reads audio packets only without decoding video:
```bash
ffmpeg -hide_banner -nostats \
  -analyzeduration 500M -probesize 500M \
  -i <input_path> \
  -map 0:a:0 -vn -sn -dn \
  -af "loudnorm=I={target_i}:TP={target_tp}:LRA={target_lra}:print_format=json" \
  -f null -
```
**Captured stderr JSON**:
```json
{
  "input_i": "-28.42",
  "input_tp": "-4.12",
  "input_lra": "10.20",
  "input_thresh": "-38.80",
  "output_i": "-23.05",
  "output_tp": "-1.00",
  "output_lra": "7.40",
  "output_thresh": "-33.40",
  "normalization_type": "dynamic",
  "target_offset": "0.05"
}
```

#### Pass 2: Transcode Application (Combined Audio Filter)
Injected into the FFmpeg transcode command via `-af`:
```text
loudnorm=I={target_i}:TP={target_tp}:LRA={target_lra}:measured_I={input_i}:measured_TP={input_tp}:measured_LRA={input_lra}:measured_thresh={input_thresh}:offset={target_offset}:linear=true:print_format=summary
```

### 3.3 Multi-Channel Downmix & Sample Rate Specifications

1. **Sample Rate Invariant**: Always `-ar 48000`.
   - *Rationale*: Broadcast SDI/HDMI embedding and CasparCG channel mixers require 48 kHz synchronous audio.
2. **5.1 Surround to Stereo Downmixing**:
   - To avoid center channel speech distortion and dialogue clipping, standard ITU-R BS.775 downmixing is combined ahead of `loudnorm`:
     ```text
     pan=stereo|FL=0.5*c0+0.707*c2+0.707*c4|FR=0.5*c1+0.707*c2+0.707*c5,loudnorm=...
     ```
3. **Mono Upmixing**:
   - Mono audio is upmixed to dual-mono stereo:
     ```text
     pan=stereo|c0=c0|c1=c0,loudnorm=...
     ```
4. **Passthrough / Multi-Channel Preservation**:
   - If `preserve_original = true` or `channels = 6`, audio is normalized per-channel without downmixing.

### 3.4 Fallback & Failure Behavior (V2-2B RetryClass Alignment)

1. **No Audio Stream in Input (`audio_channels == 0`)**:
   - Skip loudness measurement completely.
   - Omit `-af` filter graph; encode with `-map 0:a:0?` as silent video.
2. **Corrupted Audio Stream / Decoder Errors during Measurement**:
   - Classify error via `classify_error(...)` as `Permanent`.
   - Mark asset `status = "error"` with warning `corrupt_audio_stream`.
3. **Short Clips (< 3 seconds) or Extreme Dynamic Range**:
   - `loudnorm` in FFmpeg requires minimum sample history. For clips under 3s, pass `linear=false` (dynamic normalization fallback) or fallback to `AudioMode::LegacyV1Encode` with warning `short_clip_loudnorm_fallback`.

### 3.5 Hard Invariant: Legacy Bit-Identity
When `config.effective_audio_policy().mode == AudioMode::LegacyV1Encode`:
- Emitted FFmpeg arguments must be **bit-for-bit identical** to the V1 command line.
- Zero audio filters (`-af`) are emitted.
- Loudness measurement step is skipped entirely.

### 3.6 Performance Impact & Retry Architecture
- **Measurement Speed**: Pass 1 runs with `-vn -sn -dn -f null -`, reading only audio frames. Typical processing speed is 150x–350x realtime (e.g., a 10-minute video measures in ~1.8 seconds).
- **Retry Invariant**: Measurement occurs once in the `Probing` phase. The measured values (`MeasuredLoudness`) are passed into the retry loop. If FFmpeg fails due to transient video encoder issues or OS disk locking, Pass 1 is **not re-run**.

---

## 4. Design Decisions & Schema Proposal

### Open Design Decisions Requiring Human Approval:

1. **Sidecar vs Database Storage for Loudness Metrics**:
   - *Recommendation*: Store full measured metrics (`measured_i`, `measured_tp`, `measured_lra`, `target_i`) in the JSON sidecar (`SidecarPayload`) immediately in V2-3B without altering the SQLite schema.
   - *Future Schema Decision*: If the Web UI or API needs to query/filter by loudness, add additive columns to `media_assets` in a later schema slice:
     ```sql
     ALTER TABLE media_assets ADD COLUMN measured_i_lufs REAL DEFAULT NULL;
     ALTER TABLE media_assets ADD COLUMN measured_tp_dbtp REAL DEFAULT NULL;
     ALTER TABLE media_assets ADD COLUMN measured_lra REAL DEFAULT NULL;
     ```
2. **5.1 Downmix Default**:
   - *Recommendation*: Default to ITU-R BS.775 downmix to 48 kHz stereo for broadcast compatibility unless `preserve_original = true` is configured.

---

## 5. Test Plan for Slice V2-3B

1. **Filter Graph Generation Unit Tests (`src/profiles.rs`)**:
   - `test_build_args_legacy_v1_audio_bit_identical`: Confirms exact argument string matching for `LegacyV1Encode`.
   - `test_build_args_ebu_r128_two_pass_filter`: Confirms correct `-af loudnorm=...` injection with measurement parameters.
   - `test_build_args_atsc_a85_two_pass_filter`: Confirms -24 LUFS / -2 dBTP injection.
   - `test_build_args_51_downmix_pan_filter`: Confirms ITU-R BS.775 pan matrix prepended to `loudnorm`.
2. **Loudness JSON Parsing Unit Tests (`src/probe.rs` / `src/processor.rs`)**:
   - Parse valid FFmpeg `loudnorm` stderr JSON.
   - Handle invalid/truncated stderr output gracefully.
3. **Pipeline Integration Tests**:
   - Validate that video-only inputs skip measurement and encode cleanly.
   - Verify `contract_boundary` and `v1_wire_contract` tests remain 100% green.

---

## 6. Risk Register & Rollback Strategy

| Risk | Impact | Mitigation Strategy |
|---|---|---|
| FFmpeg binary missing `loudnorm` filter | Transcode job failure | Bootstrap checks filter availability during startup toolchain probe. |
| Very short audio (< 3s) | Measurement inaccurate | Linear loudnorm fallback with `short_clip_loudnorm_fallback` warning. |
| Zero-audio source file | Potential filter error | Probing checks `audio_channels > 0` before invoking Pass 1 measurement. |
| Unexpected loudness level shift on legacy setups | Downstream loudness change | Default mode remains `AudioMode::LegacyV1Encode` (strictly opt-in). |
| Immediate Rollback | N/A | Set `mode = "legacy_v1_encode"` in config or revert V2-3B commits cleanly. |

---

## 7. Baseline Verification Results

The test suite and frontend build were executed against baseline HEAD `df73c7508f20fcac69702a1959de0076df1ba297` with zero code modifications:

1. **`cargo check`**: Clean compilation with 0 errors and 0 warnings.
2. **`cargo test`**: **63 total tests passed** across all workspace test suites:
   - 45 unit tests in `src/main.rs`.
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite frontend bundle compiled successfully in 2.65s.
