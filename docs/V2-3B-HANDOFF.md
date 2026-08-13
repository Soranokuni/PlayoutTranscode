# Slice V2-3B Handoff: Opt-In Audio Normalization

## Overview

Slice V2-3B implements opt-in two-pass audio loudness normalization adhering strictly to the approved design in `docs/V2-3-AUDIO-DESIGN.md` while guaranteeing 100% bit-for-bit backward compatibility when `AudioMode::LegacyV1Encode` is active.

## Invariants & Features Implemented

1. **Legacy Compatibility**:
   - `AudioMode::LegacyV1Encode` emits the exact legacy FFmpeg argument order and filter configuration (`-map 0:a:0? -c:a <codec> [-b:a 320k] -ar 48000 -ac 2 [-async 1]`).
   - Loudness measurement is completely skipped.
   - Sidecar JSON omits the optional `loudness` field (`skip_serializing_if = "Option::is_none"`).
   - Video-only files (`audio_channels == 0`) remain fully supported without audio filter injection.

2. **Pass 1 Measurement**:
   - `LoudnessMeasurer` trait and `RealLoudnessMeasurer` run Pass 1 analysis (`ffmpeg -af loudnorm=...:print_format=json -f null -`) once during the `Probing` phase before entering the transcode retry loop.
   - `parse_loudnorm_json` parses `input_i`, `input_tp`, `input_lra`, `input_thresh`, and `target_offset`.
   - Rejects non-finite values, missing values, or malformed JSON as `RetryClass::Permanent`.
   - Measurement results are held in memory and passed immutably to each transcode attempt without SQLite persistence.

3. **Audio Routing & Downmixing**:
   - **Mono (1 ch)**: Dual-mono routing (`pan=stereo|c0=c0|c1=c0`).
   - **Stereo (2 ch)**: Direct 2-channel normalization.
   - **5.1 Surround (6 ch, `preserve_original=false`)**: BS.775 downmix pan matrix dropping LFE (`pan=stereo|FL=0.4142*c0+0.2929*c2+0.2929*c4|FR=0.4142*c1+0.2929*c2+0.2929*c5`).
   - **5.1 Surround (6 ch, `preserve_original=true`)**: Discrete 6-channel normalization (`-ac 6`).
   - **Unsupported Layouts (4 ch, 8 ch without preserve)**: Rejects with `Permanent` error `unsupported_audio_channel_layout`.

4. **Deterministic Linear Normalization**:
   - `is_linear = true` only when:
     - Non-silent ($> -70\text{ LUFS}$),
     - Projected true peak $\le \text{target\_tp}$,
     - Input LRA $\le \text{target\_lra} \times 1.5$, and
     - Clip duration $\ge 3\text{ seconds}$.
   - Silent clips ($\le -70\text{ LUFS}$ or `-inf`) bypass the `loudnorm` filter (unity gain) and append `silent_audio_loudness_skipped`.
   - Short clips ($< 3\text{s}$) force dynamic limiting (`linear=false`) and append `short_clip_loudnorm_dynamic`.

5. **Publication & Sidecar Metadata**:
   - Output sample rate is strictly verified at 48,000 Hz when an audio stream exists.
   - Staging output validation occurs prior to atomic rename.
   - Optional `loudness` metadata is written to the sidecar JSON upon successful transcode.
   - Zero SQLite migrations or schema modifications.

## Verification Results

- Unit Tests: 66/66 passed (includes 15 audio-specific unit tests covering downmixing, linear eligibility, argument generation, error classification, and sidecar serialization).
- Boundary Integration Tests: 9/9 passed (`tests/contract_boundary.rs`).
- Wire Contract Tests: 9/9 passed (`tests/v1_wire_contract.rs`).
- Web UI Build: `npm run build` completed cleanly in 2.18s.
