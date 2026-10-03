# Clarity, Texture, Dehaze and Grain: fitted to Lightroom

2 October 2026. The second wave of Lightroom calibration. Each slider was set
alone at -100, -50, +50 and +100 (Grain at 50 and 100) in Lightroom Classic
(`Adobe references #2`) on the six test photos and the chart, and compared
against Lightroom's unedited export of the same photo (`Adobe references/Adobe
no edits`). As with the lighting sliders, only *changes* are compared: each
engine's edited picture against its own unedited one.

Measured with `tools/adobe_detail.py`:

- **detail gain by scale**: lightness split into bands (difference of
  Gaussians, sigma 0.75 to 192 full-resolution pixels); per band, the
  regression slope of the edited band on the unedited one (1 = unchanged);
- **tone**: change in L* by unedited L*; **colour**: chroma ratio;
- **grain**: the added fine noise's L* and colour spread, and its size
  (the autocorrelation's half-width at full resolution).

Every result was also judged on zoomed crops, not just numbers.

## What Lightroom does, and what ours now does

### Clarity: local contrast over many sizes

Lightroom's Clarity is not one radius: at +100 it boosts the finest detail
1.55x and still the broadest (192 px) 1.13x, weighted to the midtones. Ours is
now four bands (sigma 4, 16, 64, 200 px), each with its own amount at every
slider value (`detail_table.rs`, fitted by `tools/fit_detail.py`).

| gain at sigma (px) | 0.75 | 6 | 24 | 96 | 192 |
|---|---|---|---|---|---|
| Lightroom +100 | 1.55 | 1.33 | 1.24 | 1.15 | 1.13 |
| ours +100 | 1.53 | 1.33 | 1.22 | 1.16 | 1.11 |
| Lightroom -100 | 0.69 | 0.69 | 0.74 | 0.85 | 0.94 |
| ours -100 | 0.68 | 0.72 | 0.73 | 0.86 | 0.93 |

Within 0.03 at every scale and value. On crops, Lightroom's +100 shades faces
and coats a little more deeply and darkens dark tones more; shape, bias and
edge-aware variants were tried and did not close that without breaking the
gains.

### Texture: the fine end only

Texture works from about 2 to 16 px and is gone by 48. Ours is three bands
(sigma 2, 6, 16 px).

| gain at sigma (px) | 0.75 | 3 | 6 | 12 | 24 |
|---|---|---|---|---|---|
| Lightroom +100 | 1.32 | 1.23 | 1.18 | 1.12 | 1.07 |
| ours +100 | 1.30 | 1.23 | 1.15 | 1.11 | 1.06 |

- **Negative Texture** is fitted too (2 October, wave 3): measured from the
  Texture -50 and -100 exports (made with Sharpening 40) against Lightroom's
  Sharpening 40 export, so the sharpening cancels; within 0.04 at every
  scale. See sharpening-noise-calibration.md.
- Lightroom's Texture also lifts colour speckle in skin; ours changes
  lightness only.

### Dehaze: a veil one way, contrast the other

**Negative Dehaze** is, on every photo, much the same three things, so it is
built as those (`dehaze_table.rs`, fitted by `tools/fit_dehaze.py`; applied in
`primary.wgsl`'s `haze_veil` and the detail stage's scattering):

1. a **veil curve** on the tonal key (lifting black by about 35 L* at -100);
2. **colour pulled toward the veil** (about 38% kept at -100, 70% at -50),
   and the veil tinted toward the photo's own haze colour (the brightest of
   its dark channel);
3. **scattering**: the picture mixed toward a fine and a broad blur, so fine
   detail softens first, as through real haze.

**Positive Dehaze** is adaptive per photo in Lightroom; ours keeps its
dark-channel haze removal, plus two contrast bands (sigma 16 and 200) and a
tone/colour curve fitted to Lightroom's.

| | dL* at L* 10 / 50 / 90 | chroma at L* 50 | detail fine / broad |
|---|---|---|---|
| Lightroom -100 | +34.6 / +28.1 / +3.9 | 0.38 | 0.40 / 0.55 |
| ours -100 | +33.9 / +29.0 / +4.1 | 0.39 | 0.40 / 0.51 |
| Lightroom -50 | +14.8 / +14.9 / +2.3 | 0.70 | 0.82 / 0.83 |
| ours -50 | +18.2 / +14.9 / +2.2 | 0.71 | 0.74 / 0.76 |
| Lightroom +100 | -9.3 / -13.4 / -3.8 | 1.33 | 1.20 / 1.14 |
| ours +100 | -10.1 / -10.8 / -2.9 | 1.35 | 1.14 / 1.06 |

Gaps seen on crops:

- -50 is a little softer than Lightroom's (detail kept 0.72-0.76 vs 0.81-0.83).
- At -100 Lightroom's veil glows more around backlit silhouettes, and on the
  portrait its veil is a touch greener.
- At +100 Lightroom shifts skin a little redder, and adds slightly more
  contrast at the broadest scale.

### Grain: sized by the photograph

Lightroom's grain is colourless, nearly even across tones (a little stronger
in the shadows), and sized relative to the frame, not the pixel: at Size 25
its grain pattern repeats at about 0.05% of the short edge, and at Amount 100 its
spread is about 424/sqrt(short edge) L*. Ours (`film_grain` in `main.wgsl`)
now follows the same rules. Earlier it was 8x too weak, too coarse and coloured.

- **Full size** (DSC08016, chart): spread 6.1 vs Lightroom 5.8 L*; size
  (autocorrelation half-width) 2 px vs 2 px; no colour.
- **Preview size** (3000 px): a full-size Lightroom export shrunk to preview
  size shows its grain averaged, at about scale^0.62 of its full strength, and
  ours does the same, so the preview matches the export.

| L* spread at L* 10 / 50 / 90 | +50 | +100 |
|---|---|---|
| Lightroom | 1.92 / 1.57 / 1.57 | 3.60 / 3.13 / 2.85 |
| ours | 1.80 / 1.47 / 1.39 | 3.47 / 2.94 / 2.77 |

Neither changes tone (under 1 L*). Lightroom's grain carries faint colour
speckle in deep shadows and near white (ab 1.0 vs our 0.6); ours stays
colourless.

## Re-running

```sh
# render ours (base renders at 3000 px, then the swept controls)
cargo run --release --example adobe_sweep -- "<Originals>" OUT 3000 clarity:-100,-50,50,100 texture:50,100
# measure against Lightroom
uv run --with numpy --with scipy --with tifffile --with imagecodecs --with pillow \
    tools/adobe_detail.py measure "<Adobe references #2>" "<Adobe references>/Adobe no edits" OUT measure.json
uv run --with numpy tools/adobe_detail.py report measure.json
# correct and regenerate the tables (never hand-edit them)
... tools/fit_detail.py correct clarity measure.json OUT
... tools/fit_dehaze.py correct measure.json
```

`detail_table.rs` and `dehaze_table.rs` are generated.
