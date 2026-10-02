# Lighting sliders: Exposure, Contrast, Highlights, Shadows, Whites, Blacks

1 October 2026. The six lighting sliders have Lightroom's strength, measured,
and RapidRAW's own colour: the Resolve-matched rendering, colour transforms,
Saturation and the colour and texture finishes measured on Resolve. This
replaces the hand-designed tone zones of 30 September and the previous
engine's Contrast.

## What is matched, and what isn't

Only how far each tone moves is taken from Lightroom: its change in CIE
lightness (L*) for a tone at a given lightness, at slider ±50 and ±100
(Exposure ±1 and ±2.5 stops). Each engine is measured against its *own*
unedited picture, so Lightroom's base look (its Adobe Color profile, its
contrast) is never copied, and neither are its colours. How a tone is moved
stays RapidRAW's:

- **The four zones** (Blacks, Shadows, Highlights, Whites): one gain on all
  three channels reaches the target tone, so hues never shift; Resolve's
  colour and texture finishes ride on the lift as before.
- **Contrast**: a curve on each channel's DaVinci Intermediate value, the way
  Resolve applies contrast, so colours gain saturation with it as there. It
  now runs after Exposure, as in Lightroom; the pivot slides it along the
  tonal range.
- **Exposure**: an exact gain (now in true stops: +1 is one stop), then
  Lightroom's shaping as one gain on all three channels: shadows and midtones
  move a little more than a plain gain moves them, highlights less, so
  brightening doesn't wash out the top and darkening doesn't sink it.

## Strength isn't even along the slider

Lightroom's ±50 does about 30–45% of its ±100, not half. Every slider has
tables at 50 and at 100 on both sides, straight between them and toward 0.

## Two sliders adapt to the photo

Lightroom's tone controls look at the picture. Measured on the photos both
engines render alike (JPEGs and the chart):

- **Highlights** works relative to the photo's median tone: a bright, high-key
  picture's highlights are pulled much harder than a dark one's. The zone
  reads its table shifted by `HIGHLIGHTS_CENTRE` minus the photo's median
  tonal key.
- **Whites** lifts a photo whose brightest tones fall short of white more
  than one already there: strength `clamp(a + b · brightest)` (`WHITES_LIFT`),
  with the brightest tone the 99th percentile of the tonal key.

The photo's median and brightest tones (`PhotoTones` in plan.rs) are taken
from the unedited picture's tonal key, computed with the zones' regional key.
They are RapidRAW's own rendering's tones: a RAW, which RapidRAW renders at the
camera's exposure where Lightroom's default lifts it near white, gets the
strength its own rendering calls for.

## Fit (16 photos: 7 RAWs, 8 JPEGs, the chart)

Typical per-photo error against Lightroom, L* (JPEGs and chart / RAWs),
before → after:

| slider | ±100 |
|---|---|
| Whites +100 | 15.3 → 2.8 / 13.2 → 5.5 |
| Whites −100 | 3.8 → 0.5 / 2.0 → 0.3 |
| Blacks −100 | 14.1 → 1.0 / 14.5 → 1.1 |
| Blacks +100 | 5.7 → 0.4 / 8.5 → 1.9 |
| Shadows +100 | 5.2 → 2.3 / 9.7 → 3.9 |
| Shadows −100 | 4.3 → 1.4 / 4.2 → 1.9 |
| Highlights +100 | 2.9 → 1.2 / 6.6 → 2.9 |
| Highlights −100 | 5.4 → 2.9 / 8.5 → 4.0 |
| Contrast +100 | 6.1 → 2.1 / 8.0 → 1.7 |
| Contrast −100 | 5.5 → 1.6 / 7.1 → 1.4 |

Exposure, averaged over all photos: within 0.1–0.4 L* at ±1 and −2.5 stops;
+2.5 stops is 1.5 rms, short in the deepest shadows (Lightroom opens pure
blacks a lot at big pushes). About 1 L* is the smallest difference most people
see side by side.

Known gaps: one bright JPEG's Highlights −100 (Lightroom pulls it 53 L*);
Whites +100 on the darkest pictures reaches its strength limit; Whites lifts
pale skin a little where Lightroom keeps its colour (no colour finish on
Whites yet). Glow inside a mask is added after Exposure's shaping, global glow
before it, so with exposure set they differ slightly.

## How the tables are made

1. `examples/adobe_sweep.rs` renders the originals as the app does across each
   slider (and reports each photo's tones).
2. `tools/adobe_lighting.py measure` measures Lightroom's exports and ours
   (`measure-ours` for ours alone).
3. `tools/fit_lighting.py seed` takes the tables from Lightroom's pooled
   changes; `adapt` fits the Highlights and Whites adaptation; `correct`
   re-renders, re-measures and moves each table by what is still missing.
   Targets live in `tools/tone_zones_targets.json`; the tables in
   `color_engine/tone_zones_table.rs` are generated, never hand-edited.

The tables never reverse tones: every knot lands above the one before by at
least a tenth of the step (so every slider at its extreme at once still keeps
tones apart), a zone's lift only raises tones and its cut only lowers them.
`tone_sliders_are_the_tone_zones` and `gpu_color_pipeline_contracts` check
the zones follow their tables at ±50 and ±100, greys stay grey, the far end of
each slider's range barely moves, no combination reverses tones, and Exposure
is its gain and shaping on all channels alike.
