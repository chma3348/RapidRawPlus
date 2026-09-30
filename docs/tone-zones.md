# Tone zones: Blacks, Shadows, Highlights, Whites

30 September 2026. Replaces the Resolve-matched Shadows and Highlights
(`calibration/shadows-fit-…`, `highlights-fit-…`) and the previous engine's
Whites and Blacks. Exposure (brightness) and Contrast still run through the
previous engine's functions; Saturation is Resolve's measured log mix.

## What each slider does

Each slider moves only its own part of the tonal range. At ±100, for greys
by 8-bit display level (your captured Resolve output transform):

| slider | +100 | −100 | untouched |
|---|---|---|---|
| Blacks | pure black lifts to ~10 (soft matte); 20→26 | 20→9, near-black crushes | above ~75 |
| Shadows | 20→68, 40→98, 64→110 (deep shadows about +1.8 stops) | 40→13, 64→30 | above 160; middle grey moves ≤ 8 levels |
| Highlights | 160→176, 200→220 | 200→174, 230→190; clipped detail comes back (brightest → ~222) | below middle grey |
| Whites | 200→207, 230→242 (toward clipping through the soft shoulder) | 230→216, 250→232 (lower white point) | below ~185 |

## How it works

- **Zones are judged on a tonal key:** the working-space luminance in DaVinci
  Intermediate. Each zone is a curve on that key, designed from on-screen
  intent by `tools/design_tone_zones.py` (monotone PCHIP through the targets,
  exactly "no change" outside the zone with a smooth join) and stored in
  `color_engine/tone_zones_table.rs` (offsets at +100 and −100).
- **The key is regional and edge-aware:** a guided filter (radius 2% of the
  short edge, eps 0.01 ≈ 1.4 stops) of the unedited picture, so a region moves
  as one and its texture rides along, while a dark subject against a bright
  sky is its own region and lifts without a halo. It is packed with the detail
  base (two f16) in the neighbourhood's structure entry.
- **Applied in order:** Blacks, Shadows, Highlights, Whites, each on the
  previous result. Every step is monotone at any strength, so no combination
  of sliders can reverse tones (checked for every ±100 combination).
- **One gain on all three channels** reaches the target key, so hues never
  shift; where no gain can reach it (pure black under a Blacks lift) the rest
  is filled neutral.
- **Colour and texture finishes** (measured on Resolve's exports) ride on the
  zone's own lift: lifting shadows brings out local contrast and colour,
  lifting highlights softens them, pulling highlights adds a little contrast.
  They are proportional to the lift, so they only act where the zone acts.

## Tested

`tone_sliders_are_the_tone_zones` (contract tests): the grey ramp follows the
tables at ±50/±100 for each zone; nothing moves outside a zone; middle grey
moves only under Shadows, within 0.02 in the key (~8 levels); Blacks +100
lifts pure black by a neutral fill; colours follow the gain and finishes
exactly; no tone reversal for any combination at ±100. The guided filter and
box mean have their own unit tests.

## Changing the zones

Edit `INTENT` in `tools/design_tone_zones.py` (display level in → out at ±100)
and run it; it refuses a design that could reverse tones, reports where middle
grey lands, and rewrites the table.
