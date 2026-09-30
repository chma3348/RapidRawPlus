# Shadows, fitted to Resolve — 29 September 2026

The first control calibrated against DaVinci Resolve 21's Photo page, from
Chris's `Davinci Test` exports (chart-all, five sRGB JPEGs; ±50 and ±100).

## What Resolve's Shadows does

Read off `chart-all` in DaVinci Intermediate (the exports inverted through
the captured output transform, the source taken through the captured input
transform):

- **One gain in linear light on all three channels.** Dark channels of a
  saturated colour stay dark: pure green at 3/47/3 goes to 10/121/10 at
  +100. A lift added in log would have raised the 3s to about 44 through the
  Intermediate toe; Resolve does not.
- **Keyed on a blurred luminance.** Flat interiors are pointwise: the same
  grey lands on the same value everywhere on the chart within a level.
  Across a hard edge the dark side loses up to 12 levels within ~40 px of
  the boundary (1450 px short edge) and the bright side gains 1–2: a soft
  halo from a blurred key. Blur radius mattered little between σ 8 and 32 px
  at that size; the app uses its existing 40 px structure radius (σ 20 at a
  1080 short edge), scaled with the picture.
- **Near-linear in the slider, slightly asymmetric.** ±50 is 0.5 ± 0.03 of
  ±100 across the range; the −100 curve peaks at a brighter key than +100.
  Zero is exactly the identity.
- **In stops against the key's Intermediate value** (0 = black, 0.336 = 18%
  grey, 0.74 = display white):

| key | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| ≤ 0.1 | −2.24 | −1.21 | +1.14 | +2.31 |
| 0.2 | −2.25 | −1.20 | +1.20 | +2.40 |
| 0.4 | −2.13 | −1.07 | +1.06 | +2.05 |
| 0.6 | −1.74 | −0.85 | +0.77 | +1.44 |
| 0.8 | −1.30 | −0.62 | +0.51 | +0.98 |
| 1.0 (extrapolated) | −0.99 | −0.42 | +0.34 | +0.71 |

The measured range ends at key 0.85–0.95 (display white and a little
above); beyond it the curve continues at its last slope. Scene values that
bright come only from RAW highlights, which are not part of this fit.

## What the app does now

`resolve_shadows` in the v3 shader: the slider picks a gain in stops from a
65-knot table (`src-tauri/src/color_engine/resolve_shadows_table.rs`, written
by `tools/fit_resolve_tone.py shadows`), linear between the measured stops and
zero at 0, using the neighbourhood buffer's blurred Intermediate luminance as
the key (the pixel's own when no neighbourhood is bound). It runs in the
working space before the previous engine's remaining Basic controls, whose
own Shadows no longer runs in v3.

## Result, as the app renders, full resolution (mean 8-bit levels)

| class | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| chart, before | 47.2 | 26.6 | 18.1 | 31.5 |
| chart, after | 2.1 | 1.7 | 4.5 | 7.7 |
| sRGB JPEGs (5), before | 33.7 | 19.2 | 14.7 | 27.0 |
| sRGB JPEGs (5), after | 1.1 | 0.8 | 2.4 | 4.7 |

The sweep now finds the best app value at −99.5, −49.8, 49.4 and 98.6 for
Resolve's −100, −50, 50 and 100: the scale is right.

## What remains

The darkening side is within 1–2 levels everywhere, greys within 2 at
either extreme. The lift side's residual is colour: on saturated colours
Resolve's +100 lifts the dominant channel more than the others (skin comes
out warmer and more saturated than ours by 3–4 levels of red and blue bias
on DSC08016). A per-channel gain conditioned on the same key explains only
part of it (per-pixel gain residual 0.20 → 0.14 stops), so it was not added.
Photos with strongly saturated dark colours at high positive Shadows are
where the difference shows; a chart of saturation sweeps exported at +50
and +100 only would let that term be measured directly if it matters.

## Revision, 30 September: colour and local contrast

The table above matched the chart's flat patches, and the whole-frame
averages looked good, but zoomed-in photographs at +50 and +100 came out
flat and grey next to Resolve's. A texture test on DSC08016 showed why:
Resolve's per-pixel gain follows the picture's own detail (correlation
+0.89 at +100), so pixels a little brighter than their surroundings are
lifted more, and colour is boosted along with the lift. A smooth gain can
do neither, and the chart's flat patches could not reveal it.

Fitted through the real output transform on the chart and all five sRGB
JPEGs at +50 and +100 together, in Intermediate, per channel:

    out = in + lift + w · ( 0.348 · (luma − blurred luma)
                          + 0.48 · (1 + 2.09 · (0.4 − luma)) · (in − luma) )

with luma the Rec.709 luma of the Intermediate values, the blur 0.8% of
the short edge, and w the lift in Intermediate units over 0.169. One set
of coefficients serves both +50 and +100 because both terms scale with
the lift. The same fit at −50 and −100 found neither term, so darkening
is unchanged. A wider key for the lift itself made every image worse.

| full resolution, mean levels | before | after |
|---|---|---|
| sRGB JPEGs, +100 | 4.71 | 2.61 |
| sRGB JPEGs, +50 | 2.35 | 1.29 |
| chart, +100 | 7.66 | 3.62 |
| chart, +50 | 4.48 | 2.37 |

On DSC08016's face at +100, saturation 0.223 → 0.370 (Resolve 0.347) and
fine detail 5.8 → 8.0 levels (Resolve 8.8), brightness equal. The detail
base is a fifth neighbourhood plane, packed with the Shadows key at half
precision into the structure entry's fourth component.

The lesson: judge a control on zoomed-in texture and colour, not only on
whole-frame averages.
