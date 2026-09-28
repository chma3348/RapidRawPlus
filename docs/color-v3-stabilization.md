# V3 pre-calibration stabilization

## September 27, 2026 — pre-calibration build implemented

The remaining planned engine and reference-intake work is implemented. This
means **ready to validate the supplied calibration package**, not already
matched to Resolve or universally pixel-identical across preview resolutions.
No slider curves or control strengths were fitted or changed.

### Changes and reasons

- **Input interpretation:** saving v3 edits refreshes a `v3Input` audit snapshot
  with source-content hash, ICC hash/fallback, source reference domain,
  transform/bypass decision, recovery mode, dimensions, actual transform
  hashes, stage revision and a diagnostic implementation fingerprint. It is
  photo-specific metadata, not a transferable look. Interpretation errors
  prevent writing an apparently verified snapshot.
- **Cache correctness:** source and sampling caches now include file-content
  hashes, not only size and timestamps. This catches replacements even when
  timestamps are preserved. The tradeoff is a file read/hash during rendering;
  the three 1024-pixel real-photo smoke cases still rendered warm edits in
  roughly 17–34 ms on this machine, not a universal performance promise.
- **Stage contract:** the common application renderer checks the ordered
  source/recovery, preparation, global spatial, working/global grade, ordered
  local grades/blends, and creative-look/output/grain stages. Cached and
  uncached paths use the same contract; stage names deliberately describe
  groups, including the existing source-space spatial processing.
- **Reversible RAW treatment:** development retains unrecovered float pixels.
  `neutral_green_v1` preserves the historical treatment; `off` selects the
  unrecovered pixels. Switching back develops from the original source; it
  does not attempt to invert neutralisation. The mode participates in source
  caching, history and preset application. It is available under **RAW source
  options** and does not change the Highlights slider.
- **Compatibility:** new pins use `v3-stable-input-2`; revision 1 remains
  supported with its unchanged default behavior. Disabling recovery requires
  revision 2. The explicit UI action adopts revision 2 and retains existing
  pinned transform assets, so older builds refuse rather than ignore the mode.
- **Reference intake:** `color_v3_reference` validates hashes, profiles,
  16-bit RGB data, native dimensions, neutral cases, declared capture settings
  and requested strength coverage. It rejects masked/external-look calibration
  cases and separates display photographs from Intermediate lattices. Full-
  resolution app renders are measured in common linear RGB and Oklab without
  reducing the comparison to 8 bits. The old unmanaged Python comparison now
  requires an explicit legacy flag.

### Verification and accepted boundaries

Final checks: 285 library tests and 9 GPU/integration tests passed (7 library
tests intentionally ignored), plus 9 frontend controls/history tests. Production
frontend build, strict targeted Clippy, Rust formatting and whitespace checks
passed. Browser verification exercised recovery adoption, pending/success and
failure-without-mutation states using the real component and a mocked command.

The automated gates cover source profiles and ambiguity, Bayer decode,
recovery on/off/on, revision compatibility, pinned dependencies, saved metadata
and history, masks, cached/fresh output, source replacement, shader layout,
Basic-slider parity, greater-than-eight-bit precision, gradient preview error
and reference intake failures. The reference runner also passed an end-to-end
synthetic self-test: app render → profiled 16-bit PNG → validated package →
comparison, with maximum linear channel error below 0.0001.

Nine real-photo smoke renders covered shadows, highlights and saturated color.
Full-resolution cached and fresh output matched exactly on each of those three
photos with a mixed tone/detail grade. Reduced-preview encoded-channel mean
errors against downscaled full output were 0.000532, 0.006255 and 0.000408;
maximum local errors were 0.1963, 0.1988 and 0.1601. These are **not exact
preview/export equivalence**: edge/spatial differences remain, and calibration
uses full output only. The fixed regression tolerances are 0.01 mean on the
synthetic smooth gradient and 0.02 on these real-photo probes.

RAW retention costs an extra RGBA float buffer (16 bytes/pixel). Its default
neutralisation remains a heuristic, not recovered sensor detail or a new
camera calibration. The snapshot records interpretation; it does not lock
the source photo against deliberate replacement. Reference packages do lock
source/reference contents by hash. Archive the full package and transform
assets; copying an edit alone still is not a portable rendering archive.

Whole-project TypeScript errors and existing frontend bundle warnings remain
outside this color-engine task. A diagnostic source-code fingerprint is not
a whole-binary reproducible-build guarantee. Future pixel-changing work must
use a new compatible renderer revision and revalidate the neutral baseline.

See [the intake guide](color-v3-calibration-intake.md) and
[package template](color-v3-reference-template.json). Next: validate the user's
actual exports, establish the neutral baseline, then begin slider calibration.

## September 24, 2026 — first milestone implemented

This is engine-consistency work, **not slider calibration**. No tone functions,
highlight/shadow response, RAW recovery math, or creative control strengths
were changed in this milestone.

### What changed and why

- Saved `v3Pipeline` records the engine, input-policy and RAW-development
  revisions, plus hashes of the captured input/output transform files. This
  makes those dependencies explicit instead of relying on installed filenames.
- Enabling v3 snapshots the installed transforms into the application's
  `color-v3-assets` directory, named by their BLAKE3 byte hashes. Publication is
  atomic and does not overwrite existing assets or the installed transforms.
- Rendering resolves that saved pair for source decoding, patches, shared
  Basic controls, sampling and final output. Replacing the installed cubes
  therefore does not silently change a pinned edit.
- Missing, damaged or unsupported saved dependencies produce errors rather
  than substituting another rendering. Assets are verified before use.
- Existing unpinned edits keep their current compatibility path. The explicit
  **Lock current pipeline** action adopts today's appearance; it cannot recover
  the appearance of an earlier implementation that was never versioned.
- History, saved JSON normalization and preset intensity preserve the identity
  as a discrete value. The small status/action UI includes pending and error
  states; creative LUTs are explicitly excluded from its lock claim.

### Verification

- Color-engine library tests: 50 passed, 3 performance tests ignored.
- GPU contracts, Basic-slider parity and shader layout: 9 passed. New checks
  exercise identical pixels before/after pinning, save/reopen after deleting
  the original installed cubes, 16-bit pixel conversion, constant-image
  reduced previews, masked renders and refusal of missing pinned assets.
- Frontend controls/history: 9 passed.
- Production frontend build, Rust formatting, targeted strict Clippy and
  whitespace checks passed. The build retains existing bundle-size warnings.
- Browser fixture checked the actual switch component's pending, success and
  failure states. It mocks only the native command and touches no user photos.
- Whole-project TypeScript checking still fails on existing unrelated typing
  issues; it is not a clean project-wide quality gate yet.

### Limits — not a complete rendering archive yet

The asset directory must be backed up alongside edits. Copying just a sidecar
or preset to another computer is not sufficient. Portable asset packaging,
creative LUT/flat-field dependency capture and source-file identity remain
future work. Unknown renderer revisions are refused; future builds must retain
their implementations or provide an explicit migration, not merely reuse the
same revision string. Legacy edits and presets without an identity still use
the compatibility path. Preview/export tests here do not establish universal
pixel equality for spatial effects at different resolutions.

## Original remaining build sequence (completed above on September 27)

1. **Explicit input interpretation:** persist/report the actual source profile,
   assumed-profile decision, reference domain and transform/bypass decision;
   validate profiled, untagged, wide-gamut and RAW inputs independently of look.
2. **Shared stage contract:** make decode, recovery, working conversion, spatial
   processing, grade, local composition and output order explicit and testable.
   Establish compatibility dispatch before changing any stage's behavior.
3. **Reversible RAW recovery:** retain unrecovered developed pixels; move the
   existing green-clipping treatment into a named versioned mode, preserve its
   current appearance by default, and test disabled/enabled behavior separately.
4. **Broader consistency gates:** cover cached/fresh renders, complete sidecar
   persistence, dependency edits, mask combinations, precision and meaningful
   preview/export tolerances on gradients and real photos—not only flat fields.
5. **Reference-package infrastructure:** record source and Resolve setup,
   profiles, output settings, control values and hashes; validate comparisons
   before accepting references. Do not fit slider curves until the gates above
   pass and the rendering contract is frozen for that calibration revision.

This historical list is retained to show the scope of the September 27 build.
