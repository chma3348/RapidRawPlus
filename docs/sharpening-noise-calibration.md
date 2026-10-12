# Sharpening and noise reduction: Lightroom's, slider for slider

2 October 2026. The third wave of Lightroom calibration. Darkroom Index's Detail
panel now has Lightroom's controls and behaves like them:

- **Sharpening**: Amount (0–150; negative still softens), Radius, Detail,
  Masking. Our Threshold stays as well.
- **Noise Reduction**: Noise Reduction (luminance), Detail, Contrast.
- **Color Noise Reduction**: Color Noise Reduction, Detail, Smoothness.

**A RAW opens as Lightroom opens it**, with Sharpening 40 and Color Noise
Reduction 25. A JPEG opens with neither, as in Lightroom.

- The engine applies these defaults wherever a RAW's saved settings leave them
  unset (`Detail::with_raw_defaults`), so the editor, thumbnails and exports
  agree.
- The sliders start and reset there (`defaultV3Detail(isRaw)`).
- A Lightroom import now carries Sharpening and noise reduction across
  one-to-one, sub-sliders included.

## How it was measured

Lightroom exports of the 16 test photos, each changing one setting from "no
edits" (Sharpening, Luminance NR and Color NR all 0):

- Sharpening 40 and 80;
- Detail 75 and Masking 50 (at Sharpening 40);
- Color NR 25.

Sharpening works on single pixels, so everything was compared at full
resolution, on the central 2400 px (`tools/adobe_sharpen.py`). The engine's
renders came from `examples/adobe_sweep` with `ADOBE_SWEEP_CROP=2400`. As
before, each engine's change was taken against its own unedited picture. It
was measured:

- by detail scale (sigma 0.5 to 8 px);
- by edge strength (thirds of the gradient: what Masking and Detail act on);
- by tone (L* bands);
- as halo (overshoot past an edge's own range, in L*);
- as colour gain by scale (for Color NR).

Every round was also judged on 100% crops: the leaves, the ISO 12800 portrait,
the night building and the dog's fur (`compare_sheet.py` in the session
scratchpad).

## What Lightroom does

- **Sharpening 40** boosts the finest detail 1.62x, fading to nothing by about
  6 px. Strong edges get a little less than texture: Detail 25 holds their
  halos back.
- **Amount isn't linear.** At 80, fine texture gets 2.7x and edges 2.1x.
- **Detail 75** sharpens fine texture much harder (2.3x on flat texture) and
  lets the halos grow.
- **Masking 50** almost spares flat areas (1.15x) and sharpens edges in full
  (1.5x).
- **Shadows are sharpened far less than midtones.** Below L* 12 the finest
  detail gains 1.10x, at L* 45–70 it gains 1.68x, and in the highlights 1.51x.
  This is what keeps Lightroom's shadows free of speckle.
- **Color NR 25 adapts to the photo's noise.** On the ISO 12800 portrait it
  removes about 95% of colour variation out to 8 px. On clean photos it
  removes mostly single-pixel speckle and keeps real colour detail. It never
  changes lightness, and average colour barely moves.

## What ours does (color_engine/detail.rs, sharpen_table.rs)

**Sharpening** works on the log luminance:

1. Three bands, the luminance less its Gaussian blur at 0.5, 1 and 2 px times
   Radius, each with its own amount.
2. Their sum is soft-limited as a whole; the limit is what holds a strong
   edge's halo back.
3. A tone weighting on the tonal key: 0 at L* 6, 0.28 at L* 18, 0.73 at L* 35,
   1 at L* 57 and 0.93 at L* 85.
4. Masking applies an edge mask: a smoothstep of the local gradient.

Amount scales the bands by (Amount / 40)^1.49. Detail moves the kernel's shape
between the Detail 25 and Detail 75 fits.

**At preview size** the bands would shrink below a pixel and do almost nothing.
Instead, the full-size sharpening's response at the frequencies the preview can
show is matched by bands the preview can hold (`preview_bands`). The preview
then looks as the full-size result does when shrunk.

**Color NR** smooths opponent colour in cube-root light, as Lab's a* and b* see
it, and rebuilds each pixel at its exact luminance:

1. A fine stage: a guided filter steered by luminance.
2. An adaptive stage: a guided filter steered by the colour itself, over 24 px.
   It smooths colour variation within a threshold set by the photo's own colour
   noise, estimated once for the whole frame.
3. The threshold, and how much of the noise is left, grow with how noisy the
   photo is, as Lightroom's does.

## Results, ours vs Lightroom

Full resolution, median of 16 photos:

| | finest-detail gain | gain at L* <12 / 45–70 | halo (L*) |
|---|---|---|---|
| Sharpening 40, LR | 1.62 | 1.10 / 1.68 | 0.49 |
| Sharpening 40, ours | 1.61 | 1.11 / 1.67 | 0.43 |
| Sharpening 80, LR | 2.16 | 1.19 / 2.38 | 1.23 |
| Sharpening 80, ours | 2.04 | 1.16 / 2.32 | 1.03 |
| Detail 75, LR | 1.99 | 1.16 / 2.03 | 0.81 |
| Detail 75, ours | 2.03 | 1.14 / 2.16 | 0.90 |
| Masking 50, LR (flat areas) | 1.15 | | 0.39 |
| Masking 50, ours (flat areas) | 1.07 | | 0.43 |

**At preview size** (3000 px), against Lightroom's full-size export shrunk to
the same size, finest preview detail:

- Sharpening 40: 1.37 vs 1.41.
- Sharpening 80: 1.67 vs 1.71.
- Before this wave, our preview showed 1.15.

**Color NR 25**, colour left at 0.5 / 2 / 8 px:

| Photo | Lightroom | Ours |
|---|---|---|
| ISO 12800 portrait, full size | 0.14 / 0.03 / 0.08 | −0.02 / 0.08 / 0.19 |
| ISO 12800 portrait, preview size | 0.04 / 0.02 / 0.48 | 0.04 / 0.06 / 0.48 |
| Median of all 16, full size | 0.15 / 0.55 / 0.86 | 0.24 / 0.54 / 0.88 |

**Negative Texture** is now fitted. It was a mirror of positive. It was
measured from Lightroom's Texture −50 and −100 exports, which have Sharpening
40, against its Sharpening 40 export, so the sharpening cancels out. We match
within 0.04 at every scale (−100, finest: 0.73 vs 0.69).

## Limits

- **Color NR adapts to an estimate of the noise taken from the picture.**
  Lightroom knows the camera's noise profile. A scene full of real coloured
  points (the night city, DSC03545) reads to ours as noisier than it is, and
  gets more smoothing than Lightroom gives it.
- **Still unmeasured:** Sharpening Detail and Masking away from 25/75 and
  50, Luminance NR's Detail and Contrast, and Smoothness (its Lightroom
  export changed too little to fit; ours is unchanged).
- **Very high ISO.** On the ISO 12800 portrait Lightroom's Noise Reduction
  50 smooths skin much more than ours (finest detail kept 0.34 vs 0.50).
  Making ours follow each photo's noise was tried and barely helped. Our
  noise there starts out different too (the demosaic: finer, more
  maze-like than Lightroom's).
- **Sharpened noise colour.** On that portrait, sharpened noise reads as
  pale yellow and dark red speckles in ours, orange grain in Lightroom's:
  ours sharpens before the colour rendering, Lightroom's after.
- **Sharpening 80 is slightly under** Lightroom on the brightest fine strands
  (fur sparkle).
- **Cost.** A 33 MP RAW's full-resolution render goes from 0.7 s to 1.8 s with
  the defaults on (colour NR is most of it). The result is cached, so other
  sliders don't pay it again.

## Wave 4 (11 October): Radius, Color NR and Noise Reduction

Lightroom exports of Radius 0.5 and 2 (Sharpening 40, Masking 50), Color NR
50 and 100, Color NR 25 with Detail 100 and with Smoothness 100, Noise
Reduction 50, and its RAW defaults (Sharpening 40 + Color NR 25), in
`Adobe references #2`. Most carry Color NR 25 (Lightroom's RAW default),
so each was compared against Lightroom's Color NR 25 export, and ours
against our Color NR 25 render, so it cancels.

- **Radius** changes which size of detail is sharpened far more than how
  hard. Ours only moved its bands, so 0.5 barely sharpened and 2 drew twice
  Lightroom's halo. Now Radius 0.5 and 2 each have their own fitted band
  amounts and limit (`R05`, `R2`), blended by log radius.
- **Color NR above 25** removes about the same share of the colour noise in
  Lightroom and counts more as noise; ours closed in on all of it. Now
  the mixes stay near 25's and what counts as noise grows (`COLOUR_A50`,
  `COLOUR_A100`).
- **Color NR Detail** is how much is removed: at 100 Lightroom removes a
  fifth as much, and colour detail stays (`COLOUR_D100`). Ours barely
  changed with it.
- **Noise Reduction (luminance)** was far too gentle on texture and
  edges: now a smaller, stronger guided filter, mixed in (`LUMINANCE_50`).

Full resolution, central 2400 px, median of 16 photos, finest-detail gain
(sharpening) or what is kept (noise reduction):

| | Lightroom | before | now |
|---|---|---|---|
| Radius 0.5 | 1.35 | 1.12 | 1.35 |
| Radius 2 (halo, L*) | 1.34 (0.33) | 1.58 (0.69) | 1.35 (0.24) |
| Color NR 50, noisy photos | 0.13 | 0.05 | 0.11 |
| Color NR 100, noisy photos | 0.11 | 0.02 | 0.11 |
| Color NR 25 Detail 100 | 0.65 | 0.24 | 0.76 |
| Noise Reduction 50 | 0.71 | 0.89 | 0.73 |
| Noise Reduction 50, flat areas | 0.42 | 0.55 | 0.44 |
| Sharpening 40 + Color NR 25 | 1.58 | 1.64 | 1.64 |

Judged on 100% and 300% crops (fur, leaves, the night street, the ISO
12800 portrait): Radius 2's halos and Noise Reduction's smoothing now look
like Lightroom's, except on the ISO 12800 portrait (see Limits).

    ADOBE_SWEEP_CROP=2400 adobe_sweep "<Originals>" OUT 16384 NAME=detail.KEY:V+...
    ... tools/fit_sharpen.py radius LR.json BASE
    ... tools/fit_sharpen.py colour-more LR.json BASE
    ... tools/fit_sharpen.py luminance LR.json BASE

## Re-running

```sh
ADOBE_SWEEP_CROP=2400 cargo run --release --example adobe_sweep -- "<Originals>" OUT 16384 \
    adobe_sharpening_40=detail.sharpening:40 ...
uv run ... tools/adobe_sharpen.py measure "<Adobe references #2>" "<Adobe references>/Adobe no edits" OUT m.json
uv run ... tools/adobe_sharpen.py report m.json
uv run ... tools/fit_sharpen.py fit m.json BASE      # sharpening (simulation)
uv run ... tools/fit_sharpen.py colour m.json BASE   # colour NR (per photo)
```

`sharpen_table.rs` is generated; never hand-edit it.
