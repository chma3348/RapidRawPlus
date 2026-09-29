# Saturation, fitted to Resolve — 29 September 2026

Third control calibrated against DaVinci Resolve 21's Photo page, from the
`Davinci Test` exports (chart-all and four sRGB JPEGs at ±50 and ±100).

## What Resolve's Saturation does

A mix of the DaVinci Intermediate log values toward their Rec.709 luma:

    y   = 0.2126 R' + 0.7152 G' + 0.0722 B'        (R'G'B' in Intermediate)
    out = y + (in − y) · (1 + slider/100)

Fitting the factor freely on the chart's flat pixels gave 0.005, 0.503,
1.499 and 1.995 at −100, −50, +50 and +100, with a residual of 0.3–0.4
levels, the chart's own noise floor. Greys do not move at any setting.
The same mix in linear light, with DaVinci Wide Gamut luma weights, or as
an Oklab chroma scale (what v3 did before) is five to ten times worse:

| space, luma | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| Intermediate log, Rec.709 | 0.36 | 0.26 | 0.31 | 0.41 |
| Intermediate log, DWG luma | 3.02 | 1.62 | 1.36 | 2.35 |
| linear, Rec.709 | 1.60 | 1.88 | 2.55 | 4.32 |
| Oklab chroma scale | 4.33 | 2.83 | 3.10 | 5.49 |

(mean levels on the chart's flat pixels at each space's best factor)

The log-domain mix is why saturated brights darken slightly as they
saturate, and desaturated ones lighten: that is Resolve's behaviour too.

## What the app does now

`resolve_saturation` in the v3 shader applies that mix in the working space
after the tone gains and before the Oklab colour stage; the Saturation
slider's former Oklab chroma scale is gone (Vibrance, Hue, the bands and
the wheels still work in Oklab). One slider, one definition, no table
needed.

## Result, as the app renders, full resolution (mean 8-bit levels)

| class | −100 | −50 | +50 | +100 |
|---|---|---|---|---|
| chart, before | 4.8 | 3.4 | 3.9 | 6.5 |
| chart, after | 0.97 | 0.88 | 0.91 | 0.99 |
| sRGB JPEGs (4), before | 1.6 | 1.0 | 1.3 | 2.5 |
| sRGB JPEGs (4), after | 0.36 | 0.35 | 0.42 | 0.48 |

Neutral is 0.88 on the chart and 0.40 on the photos, so these are at the
floor. The sweep finds the best app value at −99.5, −49.7, 49.9 and 99.5.
Shadows and Highlights are unchanged.

## What remains

Nothing for this slider on sRGB sources. Its place in Resolve's order
relative to Shadows and Highlights is assumed (after them); a combined
export would confirm it.
