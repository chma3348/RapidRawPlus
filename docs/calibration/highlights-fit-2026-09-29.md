
> **Superseded 30 September 2026** by the tone zones (`docs/tone-zones.md`): the app no longer uses this broad, Resolve-matched response. The measured colour and texture finishes carry over.
# Highlights, fitted to Resolve — 29 September 2026

Second control calibrated against DaVinci Resolve 21's Photo page, from the
same `Davinci Test` exports (chart-all, five sRGB JPEGs; ±50 and ±100), by
the method used for Shadows (see `shadows-fit-2026-09-29.md`).

## What Resolve's Highlights does

- **Pulling (negative) is one gain in linear light on all channels**, keyed
  on a luminance blurred only a few pixels: hard edges show a halo of about
  5 levels within 40 px, a quarter of Shadows'. Per-channel gains on the
  colour patches agree to a hundredth of a stop.
- **Lifting (positive) acts on each channel by its own value.** On the
  chart's colours and on the three correctly oriented photos a per-channel
  gain beat one shared gain by half a level; a colour's brighter channels
  run into white first, as they do in Resolve. The two 80-level rows in the
  photo scoring were my analysis ignoring EXIF rotation, not the model.
- **The gain grows with the key** and reaches deep into the midtones: at
  −100 an 18% grey drops 0.6 stop, display white 2.7 stops. ±50 is about
  0.6 of −100 on the pull side and 0.42 on the lift side, so the table is
  interpolated in the slider rather than scaled.
- **In stops against the key's Intermediate value** (0.336 = 18% grey,
  0.74 = display white):

| key | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| 0.2 | −0.23 | −0.15 | +0.20 | +0.50 |
| 0.4 | −0.68 | −0.42 | +0.62 | +1.48 |
| 0.6 | −1.45 | −0.86 | +1.31 | +3.01 |
| 0.8 | −2.73 | −1.70 | +1.84 (held) | +3.15 (held) |
| 1.0 (extrapolated) | −4.67 | −3.08 | +1.84 | +3.15 |

Lifting saturates the export to white above key 0.62 (+100) and 0.72 (+50),
so nothing more can be measured there and the gain holds. Pulling continues
into scene values above display white, the slider's purpose on RAW, so its
last slope is carried on; that region is unmeasured. Black is left alone:
the gain runs to zero at key 0.

## What the app does now

`highlight_stops` in the v3 shader, alongside `shadow_stops`: a 65-knot
table (`resolve_highlights_table.rs`, written by `tools/fit_resolve_tone.py
highlights`), read by the neighbourhood buffer's finely blurred luminance
for a pull and by each channel's own value for a lift. Both keys come from
the unedited picture, so Shadows and Highlights commute; the previous
engine's remaining Basic controls see the picture as the two left it, and
its own Highlights no longer runs in v3.

## Result, as the app renders, full resolution (mean 8-bit levels)

| class | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| chart, before | 9.2 | 6.7 | 12.3 | 27.4 |
| chart, after | 2.4 | 1.8 | 2.3 | 3.4 |
| sRGB JPEGs (5), before | 13.4 | 8.4 | 8.1 | 18.3 |
| sRGB JPEGs (5), after | 1.9 | 1.0 | 0.9 | 1.5 |

The sweep finds the best app value at −97.4, −49.0, 49.8 and 100.0.
Shadows' numbers are unchanged by the addition.

## What remains

The residual is spread thin: bias under a level on the photos at every
setting. On the chart the lift side's colour sweeps still differ by a few
levels where the per-channel gain meets the output transform's shoulder.
The order in which Resolve applies Shadows and Highlights when both are
set is unmeasured (both keys are taken from the unedited picture here); the
`order-check` export in the captures README would settle it.

## Revision, 30 September: colour and local contrast

The same fit as Shadows' revision found Highlights has the same kind of
terms with opposite signs. Lifting highlights softens local contrast and
colour a little (detail −0.205, colour −0.258 per unit of the key's gain
over 0.2 Intermediate); pulling them adds a little local contrast (detail
+0.215, colour −0.045). Full resolution, mean levels on the sRGB JPEGs:
+100 1.50 → 0.97, +50 0.91 → 0.58, −100 1.88 → 1.73, −50 1.02 → 0.95.
