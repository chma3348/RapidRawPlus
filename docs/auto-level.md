# Auto level

The wand button next to the straighten tool in the Crop panel estimates the
camera roll from the photo and writes it straight into the fine-rotation
slider. It replaces any existing fine rotation (like the straighten tool
does) and runs on the geometry-warped image with the current 90° orientation
and flips applied, so it agrees with what the user is looking at.

Implementation: `src-tauri/src/auto_level.rs` (`estimate_level`, command
`auto_level`), button and status line in
`src/components/panel/right/CropPanel.tsx`.

## How the angle is estimated

1. Downscale to 1024 px on the long side, take luma, blur (σ 1.2 px),
   Scharr gradients, and a 5×5 structure tensor. Transparent pixels (warped
   borders, PNG alpha) and a 3 px halo around them are ignored.
2. Keep pixels in the top 15% of gradient magnitude whose tensor coherence
   is at least 0.6, i.e. pixels that sit on a straight edge rather than
   texture or a corner.
3. Fold each pixel's line direction to its offset from the nearest axis
   (−45°…45°] and accumulate `magnitude × coherence²` into a 0.1° histogram,
   one histogram for near-horizontal lines and one for near-vertical.
4. Choose the peak within ±15° after weighting by a Gaussian prior
   (σ 5°) on the tilt. Accidental tilts are small; a strong line at 12° is
   almost always a bridge deck, staircase or roof, not a rolled camera. The
   horizontal histogram beats the vertical one unless the vertical peak has
   1.5× its mass. A peak on the window edge is refused.
5. Sub-bin refinement by weighted centroid within ±0.5°.
6. Converging lines: a comparable local maximum at the mirror angle
   (|tilt| ≥ 2.5°, within 1.25°, ≥ 50% of the mass) means perspective, and
   the roll is taken as the axis of symmetry between the two peaks.
7. Corrections above 5° require the *other* axis to hold a peak within 1°
   carrying ≥ 20% of the mass. A rolled camera tilts horizontals and
   verticals alike; a single family of diagonals does not, and correcting
   it produces a Dutch angle.
8. Confidence is the peak's share of all coherent-edge mass; below 5% the
   tool reports "no clear horizon or vertical lines found" and leaves the
   rotation alone.

The returned angle is in the app's clockwise-positive `rotation` convention.
The unit tests verify sign self-consistently against `apply_rotation`: the
returned angle, applied, re-estimates to 0.

## Measured behaviour

Probe: `AUTO_LEVEL_DIR=<dir> [AUTO_LEVEL_TILT=3.5] cargo test --test
probe_auto_level directory_report -- --ignored --nocapture`. With a tilt it
also rotates each photo through `apply_rotation`, crops the centre and
reports how far the change in estimate is from the applied tilt.
`AUTO_LEVEL_FILE=<photo> ... single_file_peaks` prints the competing peaks.

| set | result |
|---|---|
| 59 New York street/skyline JPEGs | 35 estimated, 24 declined; median tilt-recovery error 0.58°, max 4.3° |
| 11 assorted JPEGs | 6 estimated; median 0.14°, max 3.1° |
| 96 underwater JPEGs (no horizon) | all declined |
| synthetic charts / LUT images | recovered to 0.05° |
| per-photo cost | 60–115 ms at 6000 px source |

The large recovery errors are all perspective-heavy scenes (street looking
down the block, skyscrapers from below) where the horizontal lines fan over
±2–4° and no single "level" exists; the tool picks the strongest family,
which can differ between the original and the tilted copy.

Declined cases that used to be wrong before the prior and corroboration
rules: skyline under the Manhattan Bridge (deck at 12° read as roll),
Oculus interior (ribs at 9° read as roll).

## Known limits

- Perspective: converging lines are handled only when they are roughly
  symmetric. A street receding to one side gives an estimate within the fan
  of its lines, not a unique answer.
- Large rolls with only one axis of reference (an 8° tilted seascape with no
  verticals) are declined; use the straighten tool.
- No semantic knowledge: a sloped hillside or a leaning subject with strong
  edges will be levelled to.
