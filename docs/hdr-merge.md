# HDR merge

19 October 2026. What a merge does, and what it writes.

## Merging

- **Decoding:** each frame is decoded by the colour engine (`hdr_output::load_frame`):
  - RAWs come out as linear light, with highlight recovery and as-shot white balance;
  - finished pictures (camera JPEGs, TIFFs) are linearised through their profile.

  The previous engine's RAW development compressed highlights, so its frames disagreed exactly where they overlap.
- **Weights** (`hdr_merge::merge_frames`):
  - one weight per frame for the whole pixel, set by its brightest channel;
  - ghost rejection on luminance against the frames' consensus, fading in over a stop.

  Weighing channels apart turned smooth skies into magenta, green and cyan arcs.
- **RAW and JPEG twins:** a RAW and its JPEG twin both selected merge as the RAW alone. RAW and JPEG frames of different shots don't mix: the merge says so.
- **Brightness:** set by the middle frame (`place_at`). Its midtones open as they did, not scaled down to fit the brightest light, which made merges dark and flat.

## What it writes

Merges (HDR and panoramas) are saved **compact** by default, as Lightroom
13 saves its merges (lossy JPEG XL); Settings → General → "Merged photos"
can choose **lossless**. Measured on a 33-megapixel frame:

| | Compact | Lossless |
|---|---|---|
| RAW bracket → DNG | 11 MB | 146 MB |
| JPEG bracket → .jxl / .tif | 5 MB | 32 MB |
| 131 MP panorama (JPEG) | 22 MB | 130 MB |

(The panorama this replaced, an uncompressed 32-bit float TIFF, was
1.65 GB.) Lightroom's own: 3 × 76 MB RAWs → an 18 MB panorama DNG, a 5-frame
HDR → 14 MB, since version 13; 200–460 MB before.

**A RAW bracket → `<first>_HDR.dng`** (`hdr_dng`):
- **Pixels:** a floating-point LinearRaw DNG in linear sRGB primaries (the colour matrix says so), white balanced, in the middle frame's units with highlights past 1.0 kept.
- **Compact:** JPEG XL in 512-pixel tiles (DNG 1.7: Compression 52546, JXLDistance 52553, JXLEffort 52554, JXLDecodeSpeed 52555), distance 0.3. Chosen on real photos with Exposure +3, zoomed in, where it matched lossless (1.0 smoothed grain).
- **Lossless:** half floats, deflate with the floating-point predictor.
- **Layout:** a rendered JPEG preview heads the file, with the pixels in a SubIFD, as Lightroom lays a DNG out.
- **No scaling:** the pixels are not scaled under 1.0. That made JPEG XL take a sunset for deep shadow (28% of pixels off by more than 4 levels pushed), and declaring an HDR intensity target instead made decoders tone-map the highlights.
- **In the app:** it opens like any RAW. It renders within 0.09 levels of the RAW itself (compact).
- **Elsewhere:** darktable reads the lossless one. macOS (Finder, Quick Look) shows both black, apparently not developing floating-point LinearRaw. Untested in Lightroom.

**A bracket of finished pictures → `<first>_HDR.jxl`** (compact; JPEG XL at 16 bits, distance 0.3, effort 3) **or `.tif`** (lossless; 16-bit, deflate + horizontal predictor):
- **Why not a DNG:** their look is baked in, so a DNG would lay the RAW look over it twice.
- **Highlights:** light above white is folded under it (`fold_highlights`), unchanged up to 0.8, so Highlights brings it back.
- **Memory:** a JPEG XL of a whole picture needs its memory at once. Effort 3 measured within 0.04 levels of the default 7 at half the memory (8 GB rather than 19 GB for 131 MP). Above 150 megapixels a compact merge is saved as the lossless TIFF instead. A DNG's JPEG XL is tiled, so it has no such limit.

**Reading:**
- **Float DNGs:** rawler reads neither floating-point, LinearRaw nor JPEG XL DNGs. `hdr_dng::decode_float_dng` reads them for the colour engine's RAW path: ours and Lightroom's (SubIFDs, tiles, 16/24/32-bit floats, predictors 3/34894/34895, JPEG XL tiles).
- **`.jxl` files:** opened by the colour engine through jxl-oxide and their colour profile.

**Panoramas** (`panorama_stitching`) are saved the same way, as finished pictures, until stitching moves to linear RAW frames.

## The merge window

- **Preview:** a render of the merged file itself, through the editor's pipeline, at 1600 px. The full-size picture sent to the page as one image was too big to draw and showed black.
- **Saving:** moves the file beside the first frame (`_HDR` or `_Pano`).
- **Stack:** `versions::record_merge` makes the merge head a stack holding its frames ("HDR" or "Pano", then "Bracket 1…").
- **Panoramas:** go through the same finishing (`hdr_output::finish`): written, previewed by rendering the file, stacked.
