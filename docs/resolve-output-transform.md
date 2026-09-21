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

Not yet verified, because it needs the capture to exist: that a real photograph
rendered here and the same photograph rendered in Resolve actually agree. That
comparison is the point of the whole exercise and is the next thing to do once
a cube is captured. `docs/color-engine-baseline.md` has the fixture set to run
it on.
