# Matching Resolve's controls

The colour chain — input and output transforms — is captured from Resolve and
verified against it on real photographs to within about a level out of 255.
What is not yet Resolve's is the grading controls: v3's exposure, contrast and
wheels are its own. This is how they are brought into line.

## Why not simply capture them

An output transform is one fixed function, so sampling it on a lattice
captures it exactly. A slider is a continuum. Capturing one setting says what
that setting does, not what the slider does elsewhere. So each control is
captured at a known setting and its **operation identified** — the formula it
follows and the units its numbers are in — and then implemented, with the
captures as the test.

## Why captures are made with the output on Intermediate

With the project's output colour space set to DaVinci WG/Intermediate — the
same as the timeline — Resolve applies no output transform, and a capture is
the control's own operation in Resolve's working encoding. Nothing has to be
undone before it can be read.

## Spatial controls

Some controls look at neighbouring pixels. A lattice capture of one of those
is quietly wrong — it measures what the control did to *that* arrangement of
colours — so the likely candidates (Shadows and Highlights, and Contrast as a
check) are also captured on a scrambled lattice. `tools/resolve_fit.py`
compares the two: a pointwise control gives every colour the same answer
either way, a spatial one does not, and it is reported as spatial instead of
being fitted. Midtone Detail is spatial by definition and is not captured.

## The captures

In the existing "RapidRAW DRT capture" project:

1. **Output colour space → DaVinci WG/Intermediate.**
2. Import `lattice.tiff` and `lattice-scrambled.tiff` from
   `~/Desktop/RapidRAW control captures/`, and tag **both** clips
   **Input Color Space → DaVinci WG/Intermediate**.
3. For each row below, in the Photo page: set that one control, export as a
   16-bit TIFF at 512×512 the same way as the lattice before, named as shown,
   then reset the control before the next row.

| file name | control | setting | scrambled too |
|---|---|---|---|
| contrast-1.5 | Contrast | 1.500 | yes |
| contrast-1.5-pivot-0.3 | Contrast, Pivot | 1.500, 0.300 | |
| lift-plus | Lift | +0.10 | |
| gamma-plus | Gamma | +0.10 | |
| gain-1.25 | Gain | 1.25 | |
| saturation-75 | Saturation | 75 | |
| hue-60 | Hue | 60 | |
| temp-plus | Temp | +1000 | |
| tint-plus | Tint | +25 | |
| shadows-plus-50 | Shadows | +50 | yes |
| highlights-minus-50 | Highlights | −50 | yes |

For "scrambled too", export the same setting from the scrambled clip and add
`-scrambled` to the name. If a control will not take the value shown, use the
nearest it will and put the actual number in the file name — the units are
part of what is being measured.

That is fourteen exports. Then:

```sh
tools/resolve_fit.py identify --capture contrast-1.5.tif \
  --scrambled contrast-1.5-scrambled.tif --report contrast-1.5.json
```

for each, which reports whether the control is pointwise, whether it acts per
channel or mixes channels, and the form and parameters that explain it. The
tool's self-test (`resolve_fit.py self-test`) builds captures with known
answers — a contrast, a saturation and a spatial operation — and requires it
to identify all three, including ignoring the samples a 16-bit TIFF clips.
