# V2-3 Audio Pipeline Audit and Normalization Design

## 1. Overview & Baseline
- **Slice**: V2-3A — Audio Pipeline Audit and Normalization Design (Design Document Only, No Code Changes)
- **Baseline HEAD SHA**: `df73c7508f20fcac69702a1959de0076df1ba297`
- **Branch**: `main`
- **Scope**: Comprehensive audit of the current audio pipeline, technical specification for ITU-R BS.1770-4 / EBU R128 / ATSC A/85 two-pass loudness normalization, deterministic multi-channel routing, edge case handling, and verification of zero code drift.

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

### 2.2 Channel Layout & Downmix Handling in V1
- **Mapping**: `-map 0:a:0?` selects the first audio stream if present. If the input contains no audio stream, FFmpeg completes the transcode without audio.
- **Stereo Enforcement (`-ac 2`)**:
  - **Mono (1 channel)**: FFmpeg `swresample` duplicates mono to left and right channels (`dual mono`).
  - **Stereo (2 channels)**: Preserved as discrete left and right channels at 48 kHz.
  - **Multi-channel (5.1 surround, 6 channels)**: FFmpeg applies internal default downmixing coefficients:
    $$\text{FL}_{\text{out}} = \text{FL} + 0.7071 \cdot \text{FC} + 0.7071 \cdot \text{BL}$$
    $$\text{FR}_{\text{out}} = \text{FR} + 0.7071 \cdot \text{FC} + 0.7071 \cdot \text{BR}$$
    LFE (Low Frequency Effects) is dropped by default.
  - **7.1 surround (8 channels)**: Downmixed with default panning without dialogue gain protection.

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

### 2.6 Downstream Playout & Contract Verification (`PlayOutVue` / `CasparCG`)
- **CasparCG Media Player**: Expects 48 kHz audio. Non-48k audio causes audio/video sync drift or buffer underflows on DeckLink SDI outputs.
- **Contract Boundary Analysis (`tests/contract_boundary.rs` & `src/processor.rs:278-283`)**:
  - In `src/processor.rs:278-283`:
    ```rust
    if output_probe.audio_channels > 0 && output_probe.audio_sample_rate != 48000 {
        warnings.push(format!(
            "audio_sample_rate_not_48k: got {} Hz",
            output_probe.audio_sample_rate
        ));
        mezzanine_ok = false;
    }
    ```
  - **No-Audio vs 48 kHz Audio Distinction**:
    - If `audio_channels == 0` (video-only asset): `audio_sample_rate_not_48k` is **not** raised; the asset is valid and can be marked `mezzanine_ok = true`.
    - If `audio_channels > 0` (has audio): `audio_sample_rate == 48000` is strictly enforced. Any other rate (e.g. 44.1 kHz, 96 kHz) sets `mezzanine_ok = false`.

---

## 3. Channel Layout & Multi-Channel Routing Policy

To eliminate ambiguity between channel layout preservation and stereo playout requirements, the channel routing policy is defined per `AudioMode`:

```
Input Channels
      │
      ├─► LegacyV1Encode ──► -ac 2 (FFmpeg default swresample)
      │
      └─► EbuR128 / AtscA85
            │
            ├─► audio_channels == 0 ──► Skip audio filter; pass -map 0:a:0?
            ├─► audio_channels == 1 ──► Explicit dual-mono pan matrix (c0|c0)
            ├─► audio_channels == 2 ──► Direct stereo loudnorm filter
            ├─► audio_channels == 6 (5.1)
            │     ├─► preserve_original == true  ──► 6-channel discrete loudnorm (-ac 6)
            │     └─► preserve_original == false ──► Explicit ITU-R BS.775 pan downmix to stereo
            └─► Other (>2 ch, 7.1, 4 ch, etc.)
                  ├─► preserve_original == true  ──► Preserved if layout supported
                  └─► preserve_original == false ──► Rejected with Permanent error (unsupported_channel_layout)
```

### 3.1 Policy Matrix by AudioMode

| Channel Count | `LegacyV1Encode` | `EbuR128` / `AtscA85` (`preserve_original=false`) | `EbuR128` / `AtscA85` (`preserve_original=true`) |
|---|---|---|---|
| **0 (Silent/Video-only)** | Omit audio track | Omit audio filter; pass `-map 0:a:0?` | Omit audio filter; pass `-map 0:a:0?` |
| **1 (Mono)** | `-ac 2` (FFmpeg default) | `pan=stereo\|c0=c0\|c1=c0,loudnorm=...` | `-ac 1,loudnorm=...` |
| **2 (Stereo)** | `-ac 2` | `loudnorm=...` (stereo) | `loudnorm=...` (stereo) |
| **6 (5.1 Surround)** | `-ac 2` (FFmpeg default) | Explicit BS.775 downmix + `loudnorm=...` | `-ac 6,loudnorm=...` (discrete 5.1) |
| **8 (7.1 Surround)** | `-ac 2` (FFmpeg default) | **Rejected** (`unsupported_channel_layout`) | `-ac 8,loudnorm=...` (discrete 7.1) |
| **Other (3, 4, 16 ch)**| `-ac 2` (FFmpeg default) | **Rejected** (`unsupported_channel_layout`) | **Rejected** (`unsupported_channel_layout`) |

### 3.2 Exact FFmpeg 5.1 Channel Order and LFE Treatment

In FFmpeg `libavutil/channel_layout.h`, standard 5.1 layout (`FL, FR, FC, LFE, BL, BR`) defines channel indices:
- `c0` = Front Left (`FL`)
- `c1` = Front Right (`FR`)
- `c2` = Front Center (`FC`)
- `c3` = Low Frequency Effects (`LFE`)
- `c4` = Back Left / Side Left (`BL` / `SL`)
- `c5` = Back Right / Side Right (`BR` / `SR`)

#### LFE Handling Rationale:
In broadcast delivery (ITU-R BS.775 §2.2), **LFE is omitted (0 gain)** from the stereo downmix. Folding sub-bass LFE into standard stereo television speakers causes severe intermodulation distortion, phase cancellation with front channels, and amplifier saturation.

#### Downmix Matrix Equation & Headroom Normalization:
$$\text{FL}_{\text{mix}} = \frac{1}{\sqrt{2}} \cdot \text{FL} + \frac{1}{2} \cdot \text{FC} + \frac{1}{2} \cdot \text{BL} \approx 0.7071 \cdot c_0 + 0.5 \cdot c_2 + 0.5 \cdot c_4$$
$$\text{FR}_{\text{mix}} = \frac{1}{\sqrt{2}} \cdot \text{FR} + \frac{1}{2} \cdot \text{FC} + \frac{1}{2} \cdot \text{BR} \approx 0.7071 \cdot c_1 + 0.5 \cdot c_2 + 0.5 \cdot c_5$$

To prevent pre-loudnorm digital clipping when all 5 channels sum simultaneously, the coefficients are normalized by factor $1 / (0.7071 + 0.5 + 0.5) \approx 0.5858$:
- Front Left / Right: $0.7071 \times 0.5858 = \mathbf{0.4142}$
- Center: $0.5 \times 0.5858 = \mathbf{0.2929}$
- Surround Left / Right: $0.5 \times 0.5858 = \mathbf{0.2929}$

#### Exact FFmpeg Filter Chain for 5.1 Downmix:
```text
pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5,loudnorm=...
```

---

## 4. Two-Pass Normalization Technical Specification

### 4.1 Loudness Standards Mapping

| Standard | Mode Enum | Target Integrated ($I$) | Max True Peak ($TP$) | Target LRA |
|---|---|---|---|---|
| **EBU R128** (Default) | `AudioMode::EbuR128` | **-23.0 LUFS** | **-1.0 dBTP** | **7.0 LU** |
| **ATSC A/85** | `AudioMode::AtscA85` | **-24.0 LKFS** | **-2.0 dBTP** | **11.0 LU** |
| **Custom Overrides** | Any | `target_lufs.unwrap_or(...)` | `true_peak_dbtp.unwrap_or(...)` | `lra_target.unwrap_or(...)` |

### 4.2 Exact Filter Graphs

#### Pass 1: Audio Measurement (Analysis Phase)
Executed once in `src/processor.rs` before transcoding. Only decodes audio packets:
```bash
ffmpeg -hide_banner -nostats \
  -analyzeduration 500M -probesize 500M \
  -i <input_path> \
  -map 0:a:0 -vn -sn -dn \
  -af "{downmix_filter}loudnorm=I={target_i}:TP={target_tp}:LRA={target_lra}:print_format=json" \
  -f null -
```
*Note: If 5.1 downmixing is active, the pan filter is prepended to Pass 1 so loudness is measured on the downmixed audio stream.*

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

#### Pass 2: Transcode Application
Injected into the transcode command via `-af`:
```text
{downmix_filter}loudnorm=I={target_i}:TP={target_tp}:LRA={target_lra}:measured_I={input_i}:measured_TP={input_tp}:measured_LRA={input_lra}:measured_thresh={input_thresh}:offset={target_offset}:linear={is_linear}:print_format=summary
```

### 4.3 Deterministic `linear` Mode Eligibility Rules
`linear=true` applies static gain scaling ($\Delta G = I_{\text{target}} - I_{\text{measured}}$) without dynamic compression, preserving dynamic range. It is **not** unconditional.

**`linear = true` is eligible ONLY IF ALL of the following criteria are met:**
1. **Measured Integrated Loudness is non-silent**:
   $$\text{input\_i} > -70.0\text{ LUFS} \quad \text{and is finite}$$
2. **Projected True Peak does not exceed target True Peak**:
   $$\text{projected\_tp} = \text{input\_tp} + (\text{target\_i} - \text{input\_i}) \le \text{target\_tp}$$
3. **Measured LRA does not exceed Target LRA by more than 1.5x**:
   $$\text{input\_lra} \le \text{target\_lra} \times 1.5$$

**If any condition fails**: Pass 2 automatically sets `linear=false`. FFmpeg will apply its dynamic limiter to prevent True Peak overshoots and compress excess LRA while meeting the integrated loudness target.

---

## 5. Deterministic Edge Case Handling

When an explicit normalization policy (`EbuR128` or `AtscA85`) is selected, silent fallback to `LegacyV1Encode` is forbidden. The pipeline behaves as follows:

| Edge Case | Detection Condition | Pipeline Action | Warning / Error Code |
|---|---|---|---|
| **Complete Silence** | `input_i <= -70.0` or `input_i == "-inf"` | Unity gain (no amplification), omit loudnorm filter | Warning: `silent_audio_loudness_skipped` |
| **Short Clip (< 3s)** | Source duration $< 3000\text{ ms}$ | Execute single-pass dynamic loudnorm (`linear=false`) | Warning: `short_clip_loudnorm_dynamic` |
| **Malformed JSON in Pass 1** | Stderr JSON parse fails or missing keys | Terminate job with `RetryClass::Permanent` | Error: `audio_measurement_malformed_json` |
| **Missing `target_offset`** | Key absent in Pass 1 JSON | Compute $\text{target\_offset} = \text{target\_i} - \text{input\_i}$; if invalid, fail | None (if computed) or Error: `missing_target_offset` |
| **Non-Zero Exit in Pass 1** | FFmpeg measurement process exits $\ne 0$ | Terminate job with `RetryClass::Permanent` | Error: `audio_measurement_process_failed` |
| **Zero Audio Stream** | `audio_channels == 0` | Skip measurement, omit `-af`, pass `-map 0:a:0?` | None (valid silent video) |
| **Unsupported Channel Layout** | Channels $\notin \{0, 1, 2, 6\}$ (when not preserved) | Terminate job with `RetryClass::Permanent` | Error: `unsupported_audio_channel_layout` |

---

## 6. Sidecar & Wire Contract Compatibility Strategy

### 6.1 `SidecarPayload` Additive Extension (`src/identity.rs`)
The current `SidecarPayload` struct in `src/identity.rs` is extended with an optional, additive `loudness` field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LoudnessInfo {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub lra: f64,
    pub threshold: f64,
    pub target_lufs: f64,
    pub target_true_peak_dbtp: f64,
    pub normalization_mode: String,
    pub linear_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarPayload {
    pub playoutvue_id: String,
    pub id: String,
    pub path: String,
    pub duration_ms: i64,
    pub trim_in_ms: i64,
    pub trim_out_ms: i64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub mezzanine_ok: bool,
    pub filename: String,
    pub filepath: String,
    pub transcoded_at: String,
    pub profile_used: String,
    pub original_source: SourceInfo,
    pub output_media: OutputInfo,
    pub fps: f64,
    pub total_frames: i64,
    pub gop_frames: i64,
    pub keyframe_safe_start_ms: i64,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<LoudnessInfo>,
}
```

### 6.2 Compatibility Invariants
1. **`LegacyV1Encode` Zero Drift**: When `mode = AudioMode::LegacyV1Encode`, `loudness` is `None` and omitted from the JSON sidecar. The sidecar output is **100% byte-identical** to V1.
2. **Backward Compatibility for Downstream Consumers**: Existing readers (PlayOutVue, older tools) deserialize `SidecarPayload` without error.
3. **Database Independence**: No changes are made to `media_assets` table or `media_assets.db` schema in V2-3B. All normalization metadata resides in the JSON sidecar.

---

## 7. Performance & Retry Architecture

- **Measurement Cost**: Pass 1 runs with `-vn -sn -dn -f null -`, reading only audio stream packets. Benchmarks indicate measurement speeds of 180x–350x realtime (< 1.5 seconds for a 5-minute file).
- **Measurement-Once Invariant**:
  - Loudness measurement occurs once in `src/processor.rs` during the `Probing` stage.
  - The resulting `MeasuredLoudness` struct is passed into the retry loop.
  - If transcoding fails due to transient video encoder issues or OS file locks, Pass 1 measurement is **never re-run**.

---

## 8. Acceptance Criteria for Slice V2-3B

To be approved and merged, Slice V2-3B must meet the following criteria:

1. **`LegacyV1Encode` Argument Identity**:
   - Unit tests must verify that `AudioMode::LegacyV1Encode` generates the exact, bit-identical FFmpeg argument list as V1.
2. **Deterministic Filter Graph Construction**:
   - Exact filter string assertions for mono upmix, stereo loudnorm, 5.1 downmix pan matrix, and `linear=true/false` selection.
3. **No Implicit Multichannel Downmix**:
   - 5.1 downmix uses the explicit BS.775 pan matrix; unsupported multichannel layouts (>6 ch or non-standard) fail with `Permanent` error.
4. **Measurement-Once Before Retries**:
   - Unit tests using `MockTranscodeRunner` verify that Pass 1 measurement runs exactly once across retries.
5. **Deterministic Edge Case Handling**:
   - Tests covering silence (`-inf`), short clips (< 3s), malformed JSON, and video-only assets.
6. **Zero Database Migrations**:
   - `media_assets` table DDL remains 100% unchanged.
7. **Zero Wire Contract Drift**:
   - `tests/contract_boundary.rs` and `tests/v1_wire_contract.rs` remain completely green.

---

## 9. Baseline Verification Results

The test suite and frontend build were executed against baseline HEAD `df73c7508f20fcac69702a1959de0076df1ba297` with zero code modifications:

1. **`cargo check`**: Clean compilation with 0 errors and 0 warnings.
2. **`cargo test`**: **63 total tests passed** across all workspace test suites:
   - 45 unit tests in `src/main.rs`.
   - 9 integration tests in `tests/contract_boundary.rs`.
   - 9 wire contract tests in `tests/v1_wire_contract.rs`.
3. **`npm ci` & `npm run build`**: Web UI dependencies installed cleanly and Vite frontend bundle compiled successfully in 2.65s.
