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

**A RAW bracket → `<first>_HDR.dng`** (`hdr_dng`):
- **Pixels:** a floating-point LinearRaw DNG, the form Lightroom's HDR merges take. 16-bit half floats, deflate with the floating-point predictor.
- **Colour:** linear sRGB primaries (the colour matrix says so, so every DNG reader agrees), white balanced.
- **Brightness:** scaled under 1.0, with `BaselineExposure` restoring the middle frame's brightness.
- **In the app:** it opens like any RAW, with the RAW look and the full merged range under every control.
- **Size:** about 145 MB for a 33-megapixel merge.
- **Checked:** a RAW written as such a DNG renders within 0.02 levels of the RAW itself.

**A bracket of finished pictures → `<first>_HDR.tif`**, 16-bit sRGB:
- **Why not a DNG:** their look is baked in, so a DNG would lay the RAW look over it twice.
- **Why not a float TIFF:** the input transform treats a finished picture's white as its ceiling, so light above white has nowhere to go.
- **What it does instead:** light above white is folded under it (`fold_highlights`). The picture is unchanged up to 0.8 on the brightest channel, then a smooth shoulder reaches white at the merge's brightest light, easing toward neutral at the very top. The Highlights slider brings that range back.

**Reading float DNGs:** rawler reads neither floating-point nor LinearRaw DNGs. `hdr_dng::decode_float_dng` reads them for the colour engine's RAW path. That covers ours and Lightroom's (SubIFD layout, tiles, 16/24/32-bit floats, predictors 3, 34894 and 34895).

## The merge window

- **Preview:** a render of the merged file itself, through the editor's pipeline, at 1600 px. The full-size picture sent to the page as one image was too big to draw and showed black.
- **Saving:** moves the file beside the first frame.
- **Stack:** `versions::record_merge` makes the merge head a stack holding its frames ("HDR", "Bracket 1…").
