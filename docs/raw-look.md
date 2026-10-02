# How a RAW opens: Lightroom's default look

1 October 2026. A RAW now opens as Lightroom's default rendering shows it (its Adobe
Standard profile, sharpening and noise reduction at 0, as the reference
exports were made), measured on pairs of the same RAWs developed
by both. Rendered photographs (JPEGs and the like) are untouched: they
already opened alike in RapidRAW, Resolve and Lightroom (median L* 56.4 /
56.3 / 56.4 over the test set).

## Why

Unedited, RapidRAW's RAWs were darker, flatter and duller than Lightroom's
(and than Resolve's own default, which they were never calibrated against).
RapidRAW developed a RAW at the camera's metered exposure through Resolve's
gentle rendering; Lightroom adds a baseline exposure, a midtone-lifting
S-curve and a saturation boost (richest in warm browns and oranges). Every
slider then started from that dull picture.

Over the 7 test RAWs (Sony A7C II and A9, ISO 100–12800), in Oklab ×100
(about 2 is just visible), RapidRAW / Lightroom:

| unedited | before | now | Lightroom |
|---|---|---|---|
| median lightness | 21.3 | 36.0 | 36.2 |
| tonal spread (5–95%) | 46.4 | 68.3 | 69.6 |
| mean chroma | 2.20 | 3.46 | 3.42 |
| colour + tone difference from Lightroom | 16.0 | 1.4 | |

The mapping is consistent: across all seven RAWs and both cameras, a tone
of ours lands within about ±2 L* of the same Lightroom tone, so one curve
serves them all.

## What it does (color_engine/raw_look.rs)

Applied once to a RAW's calibrated scene-linear pixels when it is decoded
(the decoded source is cached), after highlight recovery, so everything
after it — the tone zones' keys, the photo's tones, every control,
thumbnails, export — sees the developed picture:

1. **Tone**: each pixel's tonal key (its brightness in DaVinci
   Intermediate, judged with positive weights like the tone zones) moves
   along a fitted curve, by one gain on all three channels; hues stay put.
   Smooth and never reversing, like the lighting sliders' tables, and it
   rolls the top off toward white rather than clipping, so Highlights can
   bring it back.
2. **Colour**: in Oklab, chroma scaled and hue turned by a table over 16
   hues and 9 bands of the developed tone, smoothed around the hue circle.

## How the table is made (tools/fit_raw_look.py)

1. `examples/adobe_sweep` renders the RAWs unedited without the look
   (`BASE`).
2. `fit_raw_look.py seed BASE` takes the tone curve from where each of our
   tones lands in Lightroom's, pooled over every RAW, pixel by pixel
   (Lightroom's export resized onto ours by area).
3. Render with the look (`CURRENT`); `fit_raw_look.py correct BASE CURRENT`
   moves the tone by what is still missing and the colour by Lightroom's
   chroma over ours and its hue turn, by hue and tone. Repeat.

Six rounds: tone within 0.4 L* rms; chroma per hue-and-tone cell within
about 19% rms (cells pool different photos, so some of that is the photos
disagreeing), hue within 5°. `raw_look_table.rs` is generated; never
hand-edit it.

## Limits

- Learnt on two Sony bodies. Adobe tunes its default per camera, so RAWs
  from other makes get this same look and may sit a little off until
  Lightroom exports from them are added.
- Skin in the portrait is about 9% less rich than Lightroom's at the same
  hue and lightness.
- A Resolve-matched RAW look could replace this later: it is one stage with
  its own table, separate from the lighting sliders' (re-run their check if
  it changes).

## Demosaic: RCD

Since 2 October a 2x2 RGB Bayer RAW is demosaiced with RCD (Ratio
Corrected Demosaicing, color_engine/rcd.rs), the default of darktable and
RawTherapee, instead of the dependency's PPG; X-Trans, four-colour sensors
and the fast thumbnail path keep the dependency's own. The frame, its size
and the sensor's crops are unchanged.

- On a photo-like test scene RCD reconstructs with about a third of PPG's
  error, on all four Bayer layouts (rcd.rs tests).
- At full size against Lightroom's export of the same RAW, real detail is
  level with Lightroom's (0.97-1.11 of its amplitude below 0.4 cycles per
  pixel) while the false grain at the finest scale fell from 1.45-1.68x
  Lightroom's to 1.19-1.36x; the coloured speckle in dark fur is gone.
- The unedited look still matches (colour + tone difference from Lightroom
  1.39, was 1.41), and a 24 MP RAW decodes in 0.40 s, a little faster than
  with PPG.
