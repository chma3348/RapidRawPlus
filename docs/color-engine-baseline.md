# Color engine v3 — measured baseline

Phase 0 of the roadmap asks for a fixed fixture set and a recorded baseline
before any more behaviour is tuned. This is the objective half of that gate.
It says nothing about whether the pictures look right, and nothing about
Resolve; what it does is make the engine's behaviour comparable across changes,
so an argument about whether something improved can be settled with numbers.

Reproduce with:

```sh
cargo run --release --manifest-path src-tauri/Cargo.toml \
  --example color_v3_baseline -- docs/color-v3-fixtures.json NEW_OUTPUT_DIRECTORY
```

Every fixture is rendered through every case at 1024px on the long edge, a PNG
is written per render, and `measurements.json` records the numbers below.
The colour operators are pointwise, so the render size changes measurement cost
rather than measured behaviour.

## The fixture set

22 photographs from `Test Photos`, `Test Photos/NYC` and `Test Photos/Tahiti`,
chosen by measurement rather than by eye: the two darkest, the two brightest,
the two most and least saturated, the two already clipping hardest, the two
widest and two narrowest in dynamic range, and eight ordinary frames spread
across the library's tone range. Each carries a `why` recording what it is in
the set to exercise. `docs/color-v3-fixtures.json` is the manifest; edit it to
add fixtures, and keep a holdout set out of it.

What the set does **not** yet contain, from the roadmap's Phase 0 list: RAW
files (two ARW are available and the RAW path is separate), stage or neon
light, and deliberate mixed-light skin tones. Those are gaps to fill before
claiming the set is representative.

## Baseline, 20 September 2026

Recorded on this machine, after the output and grading corrections of the same
date. `drift` is the largest chroma found in a pixel the source had as neutral;
`hue` is the mean Oklab hue rotation weighted by source chroma; `L` and `C` are
mean Oklab lightness and chroma. Full data in
`docs/color-v3-baseline-2026-09-20.json`.

| case | drift | hue° | clip hi | clip lo | L | C | ms |
|---|---|---|---|---|---|---|---|
| neutral | 0.0217 | 0.34 | 0.00% | 1.93% | 0.477 | 0.0565 | 18.4 |
| exposure +1 | 0.0217 | 0.27 | 11.85% | 1.48% | 0.593 | 0.0622 | 19.4 |
| exposure −1 | 0.0217 | 0.41 | 0.00% | 2.53% | 0.378 | 0.0450 | 18.9 |
| contrast 40 | 0.0217 | 0.35 | 3.87% | 3.99% | 0.470 | 0.0533 | 18.2 |
| shadows +50 / highlights −50 | 0.0217 | 0.31 | 0.00% | 1.48% | 0.493 | 0.0594 | 18.2 |
| warm balance | 0.0340 | 4.77 | 1.32% | 1.57% | 0.483 | 0.0580 | 18.3 |
| saturation 30 | 0.0217 | 0.29 | 0.00% | 2.41% | 0.477 | 0.0617 | 18.3 |
| vibrance 50 | 0.0217 | 0.29 | 0.01% | 2.43% | 0.477 | 0.0616 | 18.6 |
| shadow wheel, teal | 0.0473 | 11.87 | 0.00% | 10.75% | 0.490 | 0.0618 | 18.2 |
| combined grade | 0.0217 | 1.65 | 4.16% | 2.28% | 0.528 | 0.0645 | 18.1 |

## What the baseline already says

**The tonal controls are hue-stable.** Exposure, contrast and the shadow and
highlight zones all move lightness a long way while leaving hue within a third
of a degree of neutral — the same figure neutral itself produces, which is to
say within the noise of eight-bit rounding. That was a design goal and it is
now a measured fact rather than an assumption.

**Saturation and vibrance do not rotate hue either.** Both lift mean chroma
from 0.0565 to about 0.0617 with hue drift of 0.29°.

**Warm balance moves hue by 4.8°, which is the point.** A chromatic adaptation
that left hue alone would not be doing anything. It is listed here so that the
number has a recorded resting place: a later change that moves it to 10° is a
question to answer.

**The shadow wheel is the one control that pushes pixels to the floor.**
Clipped-low rises from 1.93% to 10.75%. Adding chroma at low lightness drives
channels negative and the output stage clamps them, so some of this is
inherent to tinting shadows hard. Whether 10.75% is *too much* is a judgement
the fixture PNGs can settle, and it is the first thing in this table worth
looking at with eyes.

**The neutral-axis floor is 0.0217, and it is quantization, not the engine.**
It comes entirely from the deep-shadow fixtures: near black, one level of
eight-bit rounding is a large relative chroma. It does not move between cases,
which is how you can tell it is the encoding rather than the grade.

**Interactive cost is ~18ms per megapixel-ish frame**, near enough constant
across cases — the pipeline is one pass and the controls are nearly free. At
this rate a 33-megapixel export is on the order of a second of GPU time before
readback, which is the figure Phase 4's budget should be set against.

## What it does not say

Nothing here is perceptual certification, and nothing here compares against
Resolve. Neither is possible until the reference half of Phase 0 exists: a
fixed Resolve version and project with explicit input tagging, working space,
output transform and data levels, fed the same decoded images. Until then this
baseline can prove a change did not break something; it cannot prove the look
is right.
