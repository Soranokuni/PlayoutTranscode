# Slice V2-7 Handoff: Broadcast Video Profile Engine

## Overview

Slice V2-7 introduces the typed `BroadcastVideoProfile` engine in `PlayoutTranscode`. It provides explicit profile definitions and registry lookup for standard broadcast mezzanine profiles (`playoutvue-h264-1080p25`, `playoutvue-h264-1080i50`, `playoutvue-h264-720p50`, `playoutvue-prores-1080i50`, `playoutvue-h264-1080p25-sd-pal`), with exact container, codec, pixel format, field order, color metadata, GOP, rate control, and faststart parameters.

## Invariants & Features Implemented

1. **Typed `BroadcastVideoProfile` Model**:
   - Explicitly defines `name`, `container`, `video_codec`, `pix_fmt`, `width`, `height`, `fps_num`, `fps_den`, `interlaced`, `field_order`, `gop_size_secs`, `closed_gop`, `colorspace`, `color_trc`, `color_primaries`, `video_profile`, `video_level`, `crf`, `maxrate`, `bufsize`, and `faststart`.

2. **Standard Profile Registry**:
   - `playoutvue-h264-1080p25`: Standard 1080p25 H.264 closed GOP (Profile A equivalent).
   - `playoutvue-h264-1080i50`: Broadcast 1080i50 TFF H.264 closed GOP (Profile B equivalent).
   - `playoutvue-h264-720p50`: Broadcast 720p50 H.264 closed GOP.
   - `playoutvue-prores-1080i50`: Mezzanine ProRes 422 HQ 1080i50 QuickTime MOV.
   - `playoutvue-h264-1080p25-sd-pal`: SD PAL 4:3 pillarbox SMPTE-170M in 1080p25 (Profile C equivalent).

3. **Backward Compatible Alias Lookup**:
   - `find_broadcast_profile(name)` resolves both explicit canonical names and legacy aliases (`"ProfileA"`, `"ProfileB"`, `"ProfileC"`, `"a"`, `"b"`, `"c"`).

## Verification Results

- **Unit Tests**: 82/82 passed (`cargo test`) including tests for profile registry and legacy alias mapping.
- **Contract Boundary Tests**: 9/9 passed (`tests/contract_boundary.rs`).
- **Wire Contract Tests**: 9/9 passed (`tests/v1_wire_contract.rs`).
- **Formatting**: `cargo fmt --check` clean.
- **Web UI**: `npm run build` completed in 1.67s.
