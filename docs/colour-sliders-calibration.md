# Colour sliders: Lightroom's strength, our colour

11 October 2026, the fourth wave of Lightroom calibration. Darkroom Index's
colour comes from Resolve, so these sliders keep **our way of moving
colour** and take Lightroom's **strength**: how far a number moves the
picture, and which colours it reaches. Saturation itself is Resolve's and
already moved as far as Lightroom's (+50: chroma ×1.50 vs ×1.51), so it is
unchanged.

Measured against Lightroom Classic's exports in `Adobe references #2`. Most
carry Color NR 25, so each was compared against Lightroom's Color NR 25
export, and ours against our Color NR 25 render. Every change was judged on
whole photos and crops, not just numbers.

## Vibrance

Measured by how colourful each pixel already was (CIELAB chroma below 8,
8-20, 20-40, above 40), median of 16 photos, chroma afterwards ÷ before:

| | <8 | 8-20 | 20-40 | >40 |
|---|---|---|---|---|
| -100 Lightroom / before / now | 0.03 / 0.50 / 0.04 | 0.10 / 0.52 / 0.07 | 0.19 / 0.58 / 0.15 | 0.28 / 0.70 / 0.27 |
| -50 Lightroom / before / now | 0.45 / 0.71 / 0.45 | 0.46 / 0.72 / 0.47 | 0.50 / 0.76 / 0.51 | 0.55 / 0.84 / 0.56 |
| +50 Lightroom / now | 1.42 / 1.41 | 1.35 / 1.38 | 1.29 / 1.32 | 1.20 / 1.19 |
| +100 Lightroom / before / now | 2.76 / 1.99 / 2.44 | 2.39 / 1.91 / 2.31 | 1.90 / 1.72 / 1.99 | 1.53 / 1.38 / 1.45 |

Lowering Vibrance now fades muted colours nearly to grey at -100 while
strong ones keep a quarter of theirs, as Lightroom's does; raising it grows
faster toward +100 (`primary.wgsl`).

## Colour Mixer

From the chart, exported with each band's Hue, Saturation and Luminance at
-100 and +100 (48 exports), and from all 16 photos with Red at all six,
Orange Saturation -50, Green Luminance +50 and Blue Hue +50.

What was wrong:

- **Our bands were averaged together.** Each colour's change was the mean
  over every band near it, so one band at -100 Saturation only reached
  ×0.65 at its own centre (Lightroom: ×0.0-0.1). Now each colour is shared
  between the two bands either side of it: a band acts in full at its
  centre and fades out at its neighbours'.
- **Centres.** Several sat in the wrong place against Lightroom's: Yellow
  leaned toward green, Orange reached into red. Now they are centred where
  Lightroom's are (mapped from the chart to the mixer's own hue, Oklab of
  the working space).
- **Strength.** Saturation -100 now takes a band's colour away; +100 nearly
  doubles it. Luminance darkens further than it lightens, and each band has
  its own strength, as Lightroom's do (blue and purple darken far more than
  aqua) (`MIXER_SATURATE`, `MIXER_DARKEN`, `MIXER_LIGHTEN` in `plan.rs`).
- **Muted colours.** The chart only has vivid colours. On the photos,
  Lightroom's Luminance barely moves muted colours (skin: -2 to -5 L* at
  Red -100, vivid reds -42), and Saturation -100 leaves a muted colour a
  third of its colour. Ours moved everything alike, which put blotches on
  lips and cheeks. Both now scale with the colour's chroma. Lightroom's hue
  shift does not, so ours does not either.

On the chart, at each band's centre (ΔL*, and chroma ratio for Saturation):

| band | Lum -100 LR / ours | Lum +100 LR / ours | Sat +100 LR / ours |
|---|---|---|---|
| Red | -19.1 / -19.2 | +14.4 / +14.3 | 1.53 / 1.52 |
| Orange | -12.5 / -13.0 | +9.4 / +8.4 | 1.67 / 1.68 |
| Yellow | -14.5 / -14.2 | +9.4 / +8.9 | 1.18 / 1.16 |
| Green | -16.3 / -17.2 | +9.7 / +9.3 | 1.28 / 1.19 |
| Aqua | -9.3 / -10.6 | +7.8 / +7.2 | 1.83 / 1.60 |
| Blue | -31.7 / -31.1 | +14.5 / +14.2 | 1.74 / 1.21 |
| Purple | -13.8 / -13.0 | +10.6 / +10.6 | 1.10 / 1.10 |
| Magenta | -8.9 / -8.7 | +7.7 / +7.7 | 1.21 / 1.20 |

Blue cannot be raised as far as Lightroom's: the picture is already at the
edge of what it can hold there.

**Hue** keeps our scale (the slider is in degrees). Lightroom's Hue ±100
moves a band's centre by, in our degrees: Red 0.35 per step, Orange 0.45,
Yellow 0.35, Green 0.42, Aqua 0.51, Blue 0.78, Purple 0.66, Magenta 0.27
(used by the Lightroom import).

**Gaps.** Red's hue shift still reaches a little further toward magenta
than Lightroom's. The most vivid reds on the photos darken somewhat less
than Lightroom's at Red Luminance -100 (-23 vs -42 L*).

## Re-running

    adobe_sweep "<Originals>" OUT 1500 NAME=bands.I.C:V+detail.color_noise:25 ...
    (scratch scripts: mixer_measure.py on the chart, mixer_photo2d.py by
    hue and chroma on the photos, colour_measure.py for the global sliders)
