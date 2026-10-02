# Lighting sliders: Exposure, Contrast, Highlights, Shadows, Whites, Blacks

1 October 2026. The six lighting sliders have Lightroom's strength, measured,
and render like Lightroom's (smooth, with its colour response), on
RapidRAW's own base colour: the Resolve-matched rendering, colour transforms
and Saturation. This
replaces the hand-designed tone zones of 30 September and the previous
engine's Contrast.

## What is matched, and what isn't

How far each tone moves is taken from Lightroom: its change in CIE lightness
(L*) for a tone at a given lightness, at slider ±50 and ±100 (Exposure ±1 and
±2.5 stops). Each engine is measured against its *own* unedited picture, so
Lightroom's base look (its Adobe Color profile, its contrast) is never
copied. The base colours stay RapidRAW's: the Resolve-matched rendering,
colour transforms and Saturation.

How a slider moves colour was chosen by scoring candidates against every
Lightroom export, in Oklab, for colour (a, b) and for smoothness (see
"Choosing how each slider renders" below):

- **The four zones** (Blacks, Shadows, Highlights, Whites) move a pixel by
  one gain on all three channels, hues kept, except Blacks pulling down and
  Whites lifting, which move each channel along the zone's curve on its own
  (Lightroom's colour there: deep colours keep their saturation as Blacks
  sinks them, and Whites lifts colours without paling them). Resolve's
  colour and texture finishes on Shadows and Highlights are off: they made
  the result less like Lightroom in colour and in smoothness.
- **Contrast**: a curve on each channel's DaVinci Intermediate value, the way
  Resolve applies contrast, after Exposure as in Lightroom; the pivot slides
  it along the tonal range.
- **Exposure**: Lightroom's change for a tone is much the same curve on every
  photo, judged by the tone before exposure, JPEG or RAW. Its table takes
  each pixel's unexposed tonal key to its exposed one (one gain on all three
  channels); the plain gain only supplies the colour, and carries on beyond
  2.5 stops. Then Lightroom's colour: saturation −0.25 for the first stop
  brightening, +0.10 darkening, held beyond (`EXPOSURE_COLOUR` in plan.rs;
  ours gained chroma as it brightened where Lightroom's holds steady, and
  Lightroom's colour change levels off past a stop).

## Choosing how each slider renders

Strength alone isn't enough: the first fit matched Lightroom's
lightness but rendered badly. Highlights −100 put rounded "puddles" on
smooth gradients. Two causes, both measured:

1. **Tables fitted knot by knot.** Each knot followed the measurement's
   noise, so curves had kinks, their slope jumping from 0.1 to 3–10
   between neighbouring knots (Exposure −1 had a near step). A kink in a
   tone curve shows as a band, and through the regional key as a puddle.
   Every table is now smoothed (a Whittaker smoother, then its slope held
   between 0.3 and 2.2; Exposure 0.1 to 8, as its curve is steep by white).
   That alone cut the blotchiness added beyond Lightroom's by more than half
   and the colour miss by a quarter.
2. **The key's locality.** The zones judge a pixel by a mix of its own tone
   and its region's (`ZONE_STYLE` row 0): Blacks and Whites by the pixel,
   Shadows and Highlights half and half. Fully regional left soft
   region-shaped lumps on smooth ramps; fully per-pixel exaggerated texture
   under Shadows and Highlights.

Scored on the chart and five photos (JPEGs and RAWs), against Lightroom,
in Oklab ×100 (about 2 is just visible), averaged over the four zone
sliders at ±50 and ±100: colour miss 0.37 → 0.27, blotchiness added beyond
Lightroom's 2.76 → about 0.9. `ZONE_STYLE` and `EXPOSURE_COLOUR` in plan.rs
hold the choices.

## How bright is a colour

Every zone, and Exposure, judges a pixel's brightness with weights that are
all positive (Rec.709's, on the working values). The working space's own
luminance weighs blue negatively, so a deep blue or violet read as nearly
black: Shadows −100 crushed it to a black blob and +100 lifted it in a pale
patch, shaped by the regional key. Greys read the same either way, so the
tables carry over.

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

Typical per-photo error against Lightroom (median over photos of each
photo's rms L* difference along the tonal range), JPEGs and chart / RAWs:
before calibration → the first fit → now (both 1 October).

| slider | before | first fit | now |
|---|---|---|---|
| Whites +100 | 17.1 / 11.7 | 3.1 / 11.2 | 3.2 / 12.2 |
| Whites −100 | 3.7 / 3.5 | 0.8 / 0.8 | 0.3 / 0.2 |
| Blacks −100 | 14.4 / 14.5 | 0.8 / 1.7 | 0.8 / 1.0 |
| Blacks +100 | 6.2 / 9.1 | 0.5 / 1.9 | 0.5 / 2.0 |
| Shadows +100 | 6.0 / 9.0 | 2.3 / 3.7 | 2.9 / 3.8 |
| Shadows −100 | 3.9 / 5.2 | 1.9 / 1.9 | 1.9 / 1.6 |
| Highlights +100 | 3.0 / 9.2 | 1.2 / 5.9 | 1.2 / 4.8 |
| Highlights −100 | 4.4 / 9.7 | 3.5 / 5.1 | 4.1 / 5.7 |
| Contrast +100 | 7.0 / 8.0 | 1.7 / 2.9 | 1.8 / 3.0 |
| Contrast −100 | 7.0 / 6.6 | 1.4 / 2.1 | 1.4 / 2.0 |
| Exposure +1 | | 1.3 / 4.0 | 0.6 / 2.0 |
| Exposure +2.5 | | 2.8 / 6.0 | 0.9 / 4.1 |
| Exposure −1 | | 0.4 / 8.0 | 0.4 / 1.7 |
| Exposure −2.5 | | 0.3 / 10.8 | 0.5 / 2.3 |

The smoothing costs a little lightness accuracy here and there (a curve
can't follow every wiggle of the measurement) for no kinks. Exposure's RAW
error fell because its shaping was read by the exposed tone, which on a RAW
(gained in scene values, where a JPEG is gained in display values) landed
past the range the table was learnt on: darkening a RAW brightened its
L* 80–90 tones.

Known gaps: Whites +100 on RAWs (Lightroom's default lifts a RAW's
highlights near white where ours keeps the camera's exposure, so the two
start apart and the adaptation, learnt on JPEGs, doesn't carry over); one
bright JPEG's Highlights −100 (Lightroom pulls it 53 L*); Lightroom pulls
the brightest pastels and whites a little further under Highlights −100.
Exposure +1 and +2.5 colour still differ a little beyond chroma (about 1.1
Oklab ×100). Glow inside a mask is added after Exposure, global glow before
it, so with exposure set they differ slightly.

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
the zones follow their tables at ±50 and ±100 and move colours as
`ZONE_STYLE` says, greys stay grey, the far end of each slider's range barely
moves, no combination reverses tones, and Exposure takes greys where its
table says and colours by one factor on all channels then its colour mix.
