# Calibration photo intake

The pre-calibration engine and measurement path are separate from slider
fitting. The tools below **do not tune anything** and never overwrite photos.

## What to keep while exporting

- Original source file, plus one neutral Resolve export for every source.
- 16-bit RGB TIFF or PNG, full range, native dimensions, no crop or resize.
- Embedded output ICC profile when available. For untagged files, preserve the
  exact output setting; the importer only permits an explicit sRGB assumption
  when the recorded output is sRGB. Do not attach an sRGB tag to other values.
- One changed control per export, reset before the next. Name files with the
  control and **actual Resolve value**, not an assumed equivalent RapidRAW value.
- Resolve version, page/panel, input/timeline/output spaces, tone/gamut mapping,
  automatic options, RAW development settings, node state and export settings.
  Keep these fixed within a package. Screenshots/project backups are useful.

This package path compares **display-rendered photographs**. Intermediate/log
lattice captures are a different dataset and use `resolve_fit.py`; they must
not be fed through a display/ICC adapter. Existing exports need not be redone
merely because they use a different valid RGB ICC profile—we convert tagged
files into common linear sRGB coordinates before measurement. Untagged non-sRGB,
8-bit, HDR, resized or ambiguously tagged files need review before intake.

## Preparing a package

Copy `color-v3-reference-template.json` alongside `source/`, `resolve/` and
`assets/`. Replace the placeholders, add all cases, and list planned strengths
in `required_samples`. Missing strengths and missing neutrals are errors.
The template deliberately does not pass until completed.

Run from the repository directory:

```sh
cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- hash /path/to/photo.tif

cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- pin /path/to/package/assets /path/to/input-transform.cube /path/to/output-transform.cube
```

Copy the second command's JSON into `pipeline`. It copies and verifies the
exact transform bytes. `none` explicitly selects no transform; do not use it
as a shortcut for a missing captured transform. Existing pinned assets can
also be copied from the app's `color-v3-assets` directory with their hash names.
Keep the complete asset directory with the package.

Every case contains exact application edits separately from the Resolve value.
Equal numbers do **not** imply equal slider units or response. Start with the
current app behavior; fitting comes later. Recovery mode, output mapper and
other non-target settings must be held constant. Creative LUTs, flat-field
files and masks are excluded from these first global-control packages.

```sh
# Read-only validation, no GPU required:
cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- check /path/to/package/package.json

# Full-resolution app rendering and measurements into a NEW directory:
cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- render /path/to/package/package.json /path/to/new-results
```

The result includes the exact package, source interpretation reports, GPU
identity, a diagnostic engine implementation fingerprint, 16-bit profiled renders and per-case linear-RGB error, 99th-percentile
error, maximum error, channel bias and Oklab distance. No generic “matches
Resolve” verdict is inferred: neutral-chain agreement is assessed first, then
each slider and its held-out photos. The package validates declarations and
files; it cannot prove which Resolve settings actually produced an export.

The old `compare_to_resolve.py` is now explicitly legacy/unmanaged. It requires
`--legacy-unmanaged` and is not a calibration gate.

## Reproducibility gates before fitting

1. Validate references, neutral cases, planned strength coverage and profiles.
2. Archive the package, assets, current source tree/build and initial reports.
3. Confirm neutral behavior before attributing non-neutral differences to sliders.
4. Fit using full-resolution output, never an 8-bit screenshot or reduced preview.
5. Reserve some photos and intermediate strengths for validation rather than fitting.
6. Any intentional change to input, stage order, recovery or output rendering
   requires a new rendering revision and re-running the neutral baseline first.

Reduced previews retain the existing downsample-before-grade policy. Spatial
and nonlinear effects cannot be promised bit-identical to full-resolution
output. Smooth-gradient tolerances are tested, but full-resolution renders are
the calibration authority. This preserves existing slider behavior while
making the comparison reproducible.

Developer smoke test (synthetic references, **not Resolve agreement**):

```sh
cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- self-test /path/to/new-self-test-directory

# Read-only full-resolution cache/fresh and reduced-preview regression probe:
cargo run --manifest-path src-tauri/Cargo.toml --example color_v3_reference -- consistency /path/to/photo.jpg
```
