# Rendering through Resolve's own output transform

The largest single reason a graded image "looks like Resolve" is its rendering
transform: the step that takes wide-gamut working values to something a display
can show. Tool response — how a wheel falls off, where contrast pivots — is a
smaller term, and RAW development is a separate question again.

That transform is proprietary, and reimplementing it by fitting constants is
the slow and uncertain route. It does not need reimplementing. It can be
**sampled**: put a regular lattice covering the whole working range through
Resolve, read back what comes out, and that is the transform at lattice
resolution, exactly as this machine's Resolve computes it. No fitting, no
per-image tuning, nothing to tune wrong.

This is the same method used to solve the shadow behaviour earlier: capture a
cube rather than iterate on constants.

## What this gets you, and what it does not

It gets you Resolve's rendering, applied to a correct, predictable pipeline.
Because v3 works in linear DaVinci Wide Gamut, the lattice can be laid out in
DaVinci Intermediate and the captured cube dropped straight in — the spaces
already line up. That is what the v3 work bought: a LUT applied on top of an
engine with undeclared transforms is guesswork, because you cannot say what it
is being applied *to*.

It does not make the controls respond like Resolve's. Exposure, contrast and
the wheels are still ours. Matching those is a separate, much less certain
piece of work, and the honest reason to do it second is that this one is
exact and that one is a fit.

## Capturing it

```sh
tools/resolve_drt.py make-lattice --size 64 --output ~/Desktop/lattice.tiff
```

That writes a 512×512 16-bit TIFF holding a 64³ lattice, and prints the Resolve
recipe. In short: a DaVinci YRGB Color Managed project, DaVinci Wide Gamut
processing, timeline colour space DaVinci WG/Intermediate, **output colour
space sRGB**, timeline resolution set to 512×512, the clip tagged
DaVinci WG/Intermediate, no grade at all, delivered as a 16-bit TIFF at full
data levels with no output LUT and no resizing.

Output sRGB rather than Rec.709 Gamma 2.4 matters: the files this app writes
are tagged sRGB, so capturing to Rec.709 would leave every render tagged with a
transfer function it does not have. If you grade to Rec.709 Gamma 2.4 in your
real work, capture that instead and the file tagging needs to change to match —
that is a deliberate change, not a detail to skip past.

Then:

```sh
tools/resolve_drt.py read-lattice \
  --manifest ~/Desktop/lattice.json \
  --rendered ~/Desktop/rendered.tif \
  --output ~/Desktop/resolve.cube \
  --note "Resolve 21.0.4, DWG/Intermediate -> sRGB, no white point adaptation"
```

The reader refuses two failures that otherwise produce a confidently wrong
cube: a frame Resolve resized, and a frame that came back identical to the
lattice, which means the transform never ran.

## The other half: the input transform

An output transform maps *scene* values to a display. A photograph has already
been through someone's rendering, so applying it directly renders the picture
twice — measured over the fixture set, mean Oklab lightness fell from 0.477 to
0.371, with mid grey going in at sRGB 0.461 and coming out at 0.351.

Resolve does not do that either. On import it applies an *input* transform that
undoes the rendering a file already carries, and that is what makes an
untouched sRGB photograph round-trip through a colour-managed project looking
unchanged. Capture it the same way, with two settings changed:

- Output colour space: **DaVinci WG/Intermediate** (so the output end is
  identity and only the input transform is measured)
- The clip's **Input Color Space: sRGB** — on the clip, via right-click in the
  Media Pool. In the *HDR DaVinci Wide Gamut Intermediate* processing mode
  there is no project-level input colour space to set instead.

Install it beside the other as `input-transform.cube`. Rendered photographs
then pass through it at decode, once, and arrive as scene data
indistinguishable from a RAW's — after which the output transform is the right
thing to apply, because it is no longer a second rendering.

Measured round trip, sRGB in through both cubes and back: median 0.77/255,
mean 1.19/255, and within 0.7/255 everywhere on the neutral axis from black to
white. The outliers (p99 7/255) are in the saturated corners, where the input
transform expands and the output transform compresses.

## Installing it

Copy the cube to the app's data directory as `output-transform.cube`:

```
~/Library/Application Support/io.github.CyberTimon.RapidRAW/output-transform.cube
```

v3 picks it up at startup, logs the size and content hash it is rendering
through, and uses it in place of its built-in rendering — not after it. Two
rendering transforms in series is the mistake the whole pipeline is arranged to
avoid. Remove the file to go back to the built-in rendering.

The cube's *contents* are part of the render fingerprint, so recapturing under
different project settings correctly invalidates cached renders rather than
silently reusing them.

## Licensing

A captured cube is derived from Blackmagic's transform. It is for matching your
own machine. Do not commit it to a repository you might publish, and do not
redistribute it.

## What has been verified

`captured_transform_contracts` puts values spanning the working range — below
black and well above white, which is the point of a log-encoded domain —
through the real GPU pipeline and checks each against a reference lookup: the
cube is applied as captured, on the right axes, and nothing encodes after it.
`cube.rs` tests cover parsing, interpolation, out-of-domain clamping, refusal
of 1D LUTs, short files and shifted domains, and that the digest follows the
entries rather than the title.

## Captured, 20 September 2026

Resolve 21.0.4 free, DaVinci YRGB Color Managed, processing mode *HDR DaVinci
Wide Gamut Intermediate*, automatic colour management off, output sRGB. Both
cubes are 64³.

Over the 22-fixture set, against the built-in rendering:

| | built-in | Resolve pair | Resolve output only |
|---|---|---|---|
| neutral, mean L | 0.477 | 0.475 | 0.371 |
| exposure +1, clipped high | 11.85% | 0.90% | 0.00% |
| contrast 40, clipped high | 3.87% | 1.01% | 0.00% |
| combined grade, clipped high | 4.16% | 0.57% | 0.00% |

Neutral is preserved — the pair is transparent on an untouched photograph,
which is the contract. What changes is what happens when you *grade*: pushing
a stop of exposure clipped nearly 12% of the frame under the built-in
rendering and under 1% through Resolve's, because it rolls highlights off
instead of cutting them. On a high-key fixture the difference is 50 percentage
points of clipped pixels.

The third column is the trap: an output transform with no input transform
clips nothing because it has darkened everything first.

## Checking it against Resolve

Everything above verifies the transforms against each other. What is still
unverified is the thing they were captured from: that a photograph rendered
here and the same photograph rendered in Resolve agree.

In the same project, set the output colour space back to **sRGB**, put one of
your own stills on the timeline, tag its **Input Color Space: sRGB**, add no
grade, and deliver a 16-bit TIFF at full data levels. Render the same file
here with no adjustments, then:

```sh
tools/compare_to_resolve.py --ours ours.png --resolve from-resolve.tif --report sheet.png
```

It reports encoded difference, Oklab lightness and chroma difference, and the
per-channel bias — which is the number that matters most, because a constant
offset means a transform is wrong while scatter only means precision.

Agreement is expected at **neutral only**. The transforms are captured; the
grading controls are still ours, so anything but neutral compares two
different sets of tools and proves nothing about the colour chain.

## Verified against Resolve, 20 September 2026

Six of Chris's own photographs, straight out of camera, varied subjects,
exported from Resolve's Photo page with no grade, each clip tagged sRGB,
output sRGB. The same files rendered through v3 at neutral with both captured
transforms installed, compared at Resolve's delivery size:

| photo | mean /255 | p99 /255 | Oklab ΔL | bias R G B /255 |
|---|---|---|---|---|
| DSC08222 | 0.52 | 2.70 | 0.0015 | −0.11 −0.09 −0.06 |
| DSC08265 | 1.13 | 8.75 | 0.0034 | +0.09 +0.14 +0.49 |
| DSC08270 | 0.62 | 3.24 | 0.0018 | −0.11 −0.14 −0.04 |
| DSC08279 | 1.03 | 9.11 | 0.0037 | +0.21 +0.16 +0.26 |
| DSC08304 | 0.33 | 1.22 | 0.0010 | −0.08 −0.08 +0.02 |
| DSC08319 | 0.49 | 1.73 | 0.0014 | −0.09 −0.08 +0.05 |

Under half a level of bias on every channel of every photo: no transform is
wrong. The remaining scatter is eight-bit rounding, the display dither in our
render and resampling to Resolve's size.

Two earlier rounds disagreed badly, and both were setup, not maths — worth
recording because both will recur. Untagged clips in the *HDR DaVinci Wide
Gamut Intermediate* mode are read as the timeline space, not sRGB. And an
output colour space left on DaVinci WG/Intermediate produces log-encoded
exports; those matched the captured input transform alone to within
0.07–0.36/255, which turned the mistake into an independent check of that
half on real photographs.

The Photo page uses the project's colour management, so transforms captured
on the timeline apply to stills.
