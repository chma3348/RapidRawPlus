# Color Engine v3 — implementation and verification

## Where v3 stands — September 21, 2026

**Colour chain.** Linear DaVinci Wide Gamut working space. Resolve's own input
and output transforms, captured from this machine's Resolve 21.0.4 and
installed as cubes, verified against Resolve's Photo page on six real
photographs to within about a level out of 255 with no channel bias
(`docs/resolve-output-transform.md`). Without the cubes, a built-in rendering
with soft gamut compression.

**Controls.** Exposure, warmth/tint, contrast and pivot, shadows, highlights,
blacks, whites; luminance curve and red/green/blue curves (in DaVinci
Intermediate, as Resolve's curves are); saturation, vibrance, hue; eight
selective-colour bands and eight custom ranges with a picker; four grading
wheels; dehaze, sharpening with threshold, texture, clarity, structure,
luminance and colour noise reduction; vignette and grain; glow, halation and
light flares; film saturation; Centre; chromatic aberration correction;
camera calibration; flat-field correction; creative LUTs in three input
spaces (display, DaVinci Intermediate, F-Log2 C film simulations). Auto
adjust, the white-balance picker and the clipping warning work in v3.

**Local work.** Brush, gradient and bitmap masks; colour and luminance range
masks; detail inside masks; heal, clone, generative and Sky Replace patches.

**Files.** JPEG, PNG, TIFF, WebP, HEIC, AVIF, Photoshop (flattened) and RAW,
each with its colour profile honoured. 16-bit export with profile tags.

**Speed, 24–33 megapixels on this machine.** First preview about 0.3 s; a
slider move about 11 ms, 28 ms with masks; export about 1 s plus detail.

**Not in v3.** Whole-frame effects inside masks (vignette, grain, glow,
halation, flare, Centre, film saturation, chromatic aberration, LUTs): they
describe the frame, and a mask carrying them is refused with a message. The
previous engine allowed glow and halation per mask; that is the one real loss.
The grading controls are v3's own, not yet Resolve's: matching them is
prepared (`docs/resolve-controls.md`) and waits on fourteen captures.

**Tests.** 260 library tests, the v3 GPU contracts (including forced chunk
boundaries, preview/export agreement and the strip contract), frontend
preset and history tests; `cargo fmt --check` and `clippy -D warnings` clean.

## The rest of the previous engine — September 21, 2026

With this, everything the previous engine applies to a picture has a v3
definition, and `validate_features` refuses nothing.

**Flat-field correction.** Refused until now for a real reason: its divide
decodes sRGB first, which on v3's linear data divides the wrong quantity. V3
divides linear light directly, per channel in linear sRGB primaries (where the
master flat's ratios were measured), in the unwarped frame before geometry,
and hands the shared geometry step edits without the profile so it cannot run
twice. A missing profile is an error, not a silently uncorrected render.

**Lens corrections** (distortion, TCA, lens vignetting) already reached v3
through the shared geometry step. On v3's linear pixels the vignetting gain is
applied to light, which is what it models; the previous engine applied it to
encoded values.

**Centre.** Two halves, as before. Exposure and chroma by a radial weight in
the GPU pass; local contrast as clarity at the Centre's strength blended by
`2m − 1`, so it is positive in the middle and negative at the edges, in the
cached spatial stage.

**Film saturation.** Chroma eased near white and in deep shadow, in Oklab, as
before; applied after the grade.

**Camera calibration.** The previous engine's primary hue and saturation
shifts and shadow tint, in linear sRGB primaries, as the first step of the
grade — with one deliberate difference: its hue matrix did not preserve
white (a red hue shift turned grey green). V3 normalises the rows, so neutrals
stay neutral while the primary still moves; `effects_contracts` pins both.

**Auto adjust** (`auto_controls`), measured on the scene data, not display
pixels: exposure toward mid grey by the log-average luminance over the 1st–99th
percentiles, leaving anything within half a stop alone and correcting 60% of
the rest (within ±2 stops) because high- and low-key pictures are usually
meant; grey-world white balance at a third of its strength within ±25; then
highlights and shadows from what still clips or crushes. About 0.3–0.4 s.
`examples/v3_auto.rs` prints the choices and writes before/after sheets.

**White-balance picker.** Solves v3's own cone-space gains for the clicked
neutral; the rendering between it and the screen compresses ratios, so it
undershoots slightly and a second click refines it.

**Clipping warning.** Drawn on the editor preview after the histogram has been
computed from the real picture, never on exports.

## Lens and film effects, and LUTs — September 21, 2026

The last creative controls the previous engine had that v3 did not.

**Glow, halation, light flares.** Light added to light, so computed on linear
values in their own CPU stage (`color_engine/optics.rs`), after detail and
before the GPU pass. What they respond to is how bright a highlight *will be*:
thresholds are in post-exposure units, so raising exposure makes more of the
picture glow, as through a real lens, and the added light scales with
exposure like everything else (`exposure_decides_what_glows`). The shapes and
colours are the previous engine's, defined in linear sRGB and converted to
whatever primaries the prepared image holds, so a DWG source gets the same
light (`wide_gamut_sources_get_the_same_light`). They are computed on a grid no
larger than 1024 pixels with radii as fractions of it, thresholded at full
resolution first so a small bright point still glows; preview and export
therefore agree (`preview_and_export_agree`). The flare is the previous
engine's starburst, ghosts, halos and streak, on a 256² map in normalised
coordinates. The stage is cached with detail; exposure is part of the key only
while one of these is on.

**Chromatic aberration.** Red and blue scaled about the frame's centre
relative to green, the previous engine's slider (±100 = ±1% of the distance
from centre), sampled bilinearly rather than rounded to whole pixels. Applied
first, before detail, so sharpening sees the corrected edge.

**LUTs.** The previous engine applied every LUT to its display output, which
left the LUT's input undefined in v3's terms — hence the refusal until now.
The same settings (`lutPath`, `lutIntensity`, `lutInputSpace`,
`lutSimExposure`) are read, so film simulations, presets and copy/paste carry
over, and the input space now decides where the LUT goes:

| Space | Fed | Placed |
|---|---|---|
| Display | sRGB code values | after the rendering, as before |
| DaVinci Intermediate | graded scene data, DI-encoded | before the rendering, as a node in a DWG timeline; output decoded back to linear |
| F-Log2 C | the scene as a Fujifilm camera encodes it (F-Gamut C, F-Log2), times the simulation exposure | *instead of* the rendering, because the LUT is one |

The lattice is a second storage binding in the GPU pass, sharing the
tetrahedral weights with the captured output transform. Each placement is
pinned against a CPU evaluation of exactly that placement in
`look_contracts`, including intensity mixing and the simulation exposure; a
malformed lattice is refused rather than read out of bounds. Range masks
sample the picture before the LUT, like every other grade.

## Detail — September 20, 2026

Sharpening (with threshold), texture, clarity, structure, and luminance and
colour noise reduction. The last of the three things that stopped v3 being an
engine you could finish a photo in.

Everything else in v3 is pointwise, which is why its GPU pass walks the image
in one-dimensional chunks. Detail is defined by neighbourhoods, so it runs as
its own stage on the prepared image, before that pass. It acts on luminance in
log2 and scales RGB by the ratio: sharpening cannot put colour fringes on an
edge, and a control does the same thing in the shadows as in the highlights.
Colour noise reduction acts on chromaticity — colour with luminance divided out
— through a guided filter steered by luminance, so it cannot change brightness
and does not bleed colour across edges.

Radii are the previous engine's (1, 3.5, 8 and 40 full-resolution pixels),
scaled with the preview. The preview shows what the export will do: a
quarter-size render matches the downscaled full render far more closely than
the size of the edit itself. Single-pixel sharpening is the exception no
preview can escape, and the panel says to judge it at 100%.

The tiling contract: large exports are processed in horizontal strips read with
a halo wider than every filter that runs on them, and
`strips_match_the_whole_image` holds that a strip boundary changes no pixel.

Cost at 33 megapixels, this machine: sharpening 84 ms, clarity and structure
199 ms, both noise reductions 564 ms, everything 801 ms. The result is cached
against the prepared image and the detail settings, so other sliders do not
redo it.

Detail inside masks is refused with a message rather than silently ignored;
the mask panel does not offer it. Dehaze, the centre control and chromatic
aberration are not ported.

Also fixed: the v3 panel still refused to switch on for photos with patches and
still said patches and colour-range masks were unavailable, although both had
been supported since the previous change. Neither was reachable until now.

## Patches and range masks — September 20, 2026

Two of the three things that stopped v3 being an engine you could finish a
photo in.

**Heal, clone and generative patches.** V3 refused them because nothing said
what the stored pixels were. The previous engine composites a patch onto
whatever the decoded base happens to be, which works there only because nothing
declares a colour space, so nothing can disagree. The patch does record it:
`encoding: "gamma"` marks pixels lifted from float or RAW data and stored
through a 1/2.4 curve so deep shadows survive eight bits; anything else came
from rendered display pixels and is sRGB. They join at the decoded-source
stage, before geometry, because the mask stored with a patch is in those
coordinates — and when an input transform is installed the patch goes through
it too, or it would be the one part of the frame still carrying a rendering the
rest has had removed. Patches are part of the prepared image's cache key;
without that, hiding one would leave the old composite on screen.

**Colour and luminance range masks.** These needed a sampling contract, and the
previous engine already has a good one: they sample the geometrically-warped
source *before* any adjustment, so the mask does not move as you grade. V3
honours the same contract, rendered through its own pipeline at neutral, so
what the mask measures is what the picture is before grading rather than a
second opinion about colour from a different set of transforms. Full
resolution, because the mask generator maps coordinates against the warped
image's own dimensions, and cached against source, geometry and patches so it
is built once per change rather than per render.

Contracts: a colour range mask clicked on a constant colour selects the whole
frame and matches a global exposure exactly; a swatch hue that matches nothing
changes nothing; sampling is unaffected by the grade applied on top. Patch
tests cover the sRGB and gamma storage paths, mask coverage, and that a hidden
patch is a no-op.

Still outstanding from that list: detail — sharpening, clarity, structure and
noise reduction. Those need neighbouring pixels, and v3's renderer is
deliberately a one-dimensional pass over chunks of a storage buffer. Adding
them means a second, two-dimensional pass with halo padding and a tiling
contract, which is the Phase 3 gate about results not changing with zoom,
preview resolution or export tiling.

## Output and grading corrections — September 20, 2026

Six changes from a review of the engine as built. None of them adds a control.

**Oklab is composed, not routed.** The colour stage converted working RGB to
sRGB and then applied Oklab's linear-sRGB cone matrix. Those matrices multiply
out, so the result was already correct — this is a clarity and cost change, not
a bug fix, and `spaces_compose_to_the_same_oklab` pins the equivalence. The
composed matrix is anchored on Ottosson's published *sRGB* matrix rather than
his XYZ one, because the two disagree in the fourth decimal; the contract
records the size of that gap so neither can drift silently. The output stage
keeps its own inline sRGB copy, since it works in the destination's primaries.

**Soft gamut compression.** `gamut_project` put every out-of-gamut colour onto
the gamut shell, so colours that differed a lot arrived identical — the flat
look in saturated regions. `gamut_compress` leaves chroma below 85% of the
boundary alone and squeezes the rest into the band above it, approaching the
boundary without reaching it. Colours between the threshold and the boundary
lose a little chroma; that is the trade. Because it changes in-gamut colours
it ships as `scene_luminance_v2` / `display_gamut_v2` rather than as a change
to the v1 renderings, which keep their contracts. The application uses the v2
pair. Measured on an out-of-gamut chroma sweep at fixed lightness: the hard
projection leaves neighbouring samples less than 1/255 apart and jumps once at
the boundary; the compressor keeps every pair apart with no jump.

**Grading wheels key off the tone-mapped image.** They read the luminance from
before contrast, the zones and the curve, while the custom ranges selected from
after them — two selection stages in one function. They now both read the same
image, which is also the one on screen: set exposure and contrast first, then
tint the shadows you can see.

**The shadow wheel can lift black.** Tinting pure black colours pixels that
carry no colour, so the chroma terms still fade out there. The lightness term
does not, because lifting black off zero is what a shadow wheel is for.

**No cliff at the bottom of the tone chain.** DWG's blue coefficient is
negative, so a non-physical pixel can reach zero or negative luminance. The
chain used to switch off below `1e-8`, putting a hard edge between neighbouring
near-black pixels; it now fades out across the bottom of the floor.

**Stage switches are their own word.** "Is this control enabled" was read out of
spare components that also carried values (`tone.w`, `color.w`, `curve[0].z`).
They are now an explicit `flags` vector. This also made it possible to skip the
tone chain outright when it is neutral, which matters: the GPU is free to
compute `x/x` reciprocally, so the chain was costing exposure its exactness. A
stop is now an exact doubling again.

**Dither on the way to eight bits.** Eight bits cannot hold the gradients this
pipeline produces, and plain rounding turns a slow ramp into flat plateaus with
visible steps — which reads as a fault in the grade rather than in the
encoding. The on-screen image now carries one LSB of deterministic triangular
noise. Thumbnails, the scopes and the inspection image keep exact rounding,
because those measure the picture and should not measure the dither; 16-bit
export is unaffected, being already below the noise floor.

Also removed: `PipelineConfig::exposure_stops`, which every caller set to zero.
`controls.exposure` is the only exposure, applied once.

Verified: 5 v3 contract tests including real GPU execution, 235 library unit
tests, the production shader-layout test and 4 frontend preset tests. No
perceptual certification and no Resolve comparison is claimed by any of this.

## Global range picker and selection inspection — September 20, 2026

Global custom ranges now have **Pick from image / view selection**. Add/select a
range, open the picker, and click its image preview to replace that range's
center. Existing range width and adjustment values remain intact. Enter samples
the center of the preview. A grayscale image underneath shows selection strength.

The backend renders a fixed maximum-512px inspection image after source
interpretation, geometry, exposure, white balance and tone/curve adjustments,
but before saturation, hue, selective edits, grading wheels and all masks. It
samples the float DWG stage converted to Oklab—not the display PNG. This makes
the picked target independent of selective changes, main-canvas zoom and display
profile. Sampling is one inspection-resolution pixel, not a full-resolution
pixel or screen eyedropper. The displayed inspection image receives the ordinary
SDR output transform; its float sampling stage remains unclipped.

The grayscale preview uses circular hue distance, soft chroma/lightness widths,
neutral protection, active overlap normalization and visible alpha. An untouched
range previews the influence it would have when adjusted. Untouched ranges no
longer dilute already-active ranges. Primary-tone changes can legitimately alter
selection: the selection stage is after those controls.

Inspection is global-only. Local ranges remain manually targeted because their
sampling stage depends on preceding masks. This does not add picking directly on
the main canvas or a main-canvas overlay. LUT integration and real-photo/Resolve
comparison are still pending.

Requests are debounced, and stale results cannot retarget a newer image/range.
Loading disables sampling; failed refreshes remove the old clickable preview.
Out-of-bounds coordinates, transparent/neutral samples, invalid range indices and
out-of-supported-range HDR samples produce explanations rather than wrong picks.

GPU/application contracts verify crop alignment, a sampled color selecting
itself, unchanged sampling after selective edits, invalid coordinates/indices,
and unchanged output after adding an untouched range. Unit tests verify hue
wrap, neutral protection and overlap weights. Browser fixture testing uses
explicit synthetic IPC responses for loading, keyboard sampling and failures;
it is not a full native IPC test. Real backend image tests are separate.

## Tone-curve and custom-range update — September 20, 2026

The v3 panel now includes a five-knot luminance tone curve (three editable
interior points) and up to eight custom color ranges. These are also available
in v3 local adjustments. Legacy curve/point-color settings are not reinterpreted.

- Curve coordinates use `log2(1 + 16Y) / log2(17)` with fixed endpoints 0 and 1.
  Monotone cubic Hermite interpolation and minimum knot separation prevent tonal
  reversals/overshoot. Above white, the final tangent continues rather than
  clipping. The curve scales working RGB by luminance ratio, after primary tone
  adjustments and before perceptual color adjustments. It is not an RGB-channel
  curve, an arbitrary-point editor, or a Resolve curve clone.
- Custom ranges select pre-selective Oklab hue/chroma/lightness with separately
  adjustable soft widths. Circular hue distance avoids a 0/360-degree seam;
  near-neutral colors are protected. Overlapping range adjustments are normalized
  and evaluated from the same source color, avoiding cascading/order-dependent
  selection. Hue/chroma/lightness adjustment units match the existing v3 bands.
- The initial range implementation was manually targeted. The global picker and
  inspection update above adds pre-selective sampling. Hue curves, RGB-channel
  curves and LUT integration remain open.
- New fields default to an identity curve and no ranges, so older v3 settings
  still load. Preset strength blends curves toward identity and scales only range
  adjustments, preserving the selection centers and widths.

GPU tests cover extreme curve ordering, HDR headroom, neutral protection,
unselected-color stability, hue seam continuity, range order independence, and
identical pixels after serializing/reopening advanced settings. Frontend tests
exercise curve interpolation and preset blending. Browser QA of the actual
component in the 340px standalone fixture verified edits/reset and range
add/remove with independent values, using the application's dark theme. This is
component QA, not a full native application test or real-photo certification.

Manual component fixture (using the regular Vite development server):
`/tests/color-v3-controls.html`. It does not load or modify photos or sidecars.

Editor-history integration test:

```sh
./node_modules/.bin/esbuild tests/colorV3-history.test.ts --bundle --platform=node --format=cjs --outfile=/tmp/rapidraw-color-v3-history-test.cjs
node --test /tmp/rapidraw-color-v3-history-test.cjs
```

## Application integration update — September 20, 2026

V3 is now an explicit opt-in in the adjustment panel. It has its own persisted
`adjustments.v3` controls, leaving previous color values intact when switching
back. The application routes previews, comparison views, thumbnails and exports
through `color_engine/application.rs`. It caches interpreted sources, spatial
transforms and the GPU pipeline. Ordinary output reads back only the final float
pixels; diagnostic capture additionally reads the two working stages.

Implemented controls: linear exposure, relative Bradford warmth/tint, pivoted
luminance contrast, monotonic shadow/highlight/black/white curves, Oklab
saturation/vibrance/hue, eight smoothly overlapping selective-color bands, and
global/shadow/midtone/highlight tint controls. V3 shadows/highlights deliberately
have a new response; v1/v2 edits still use their original engine.

The application uses `scene_luminance_v1` for scene inputs and `display_gamut_v1`
for rendered photos. The former uses a luminance shoulder; both reduce Oklab
chroma for out-of-gamut colors. Numerical roundoff near the destination cube is
handled separately so neutral in-gamut colors are not needlessly compressed.
This is a defined SDR transform, not Resolve's proprietary output rendering.

Brush/gradient/bitmap local adjustments blend in float working space before the
final output transform. Image-dependent range masks remain explicitly rejected.
Legacy LUTs, AI patches and flat-field correction are also rejected; detail
effects are unavailable in this opt-in mode. Additional curve families, sampled point-color controls,
LUT contracts, broader RAW calibration and monitor-specific display validation
remain unfinished. This is **not yet a complete professional release**.

New numerical checks exercise extreme combined tonal controls, neutral balance,
alpha, in-gamut identity, exact exposure scaling, source/transform cache use,
full-mask versus global exposure, zero-opacity masks, and constant-color
preview/export agreement. Preset intensity now preserves discrete engine/control
versions and fades selective-color values without rotating grading hues.

Run the frontend preset contracts with:

```sh
node --experimental-strip-types --test tests/colorV3.test.mjs
```

The frontend production build passes. Project-wide TypeScript checking still
reports errors across existing modules; it is not a clean verification gate yet.
Native interface inspection timed out, so no end-to-end desktop QA or calibrated
monitor/Resolve match is claimed.

Verified in this update: 4 v3 contract tests (including real GPU/application
integration), 12 input/RAW tests, 5 precision/profile export tests, 6 legacy
highlight tests, the production shader-layout test and 2 frontend preset tests.
The normal-output and diagnostic-capture GPU paths produce identical final
pixels. PNG, JPEG and TIFF profile tags are checked after encoding, and PNG/TIFF
retain real 16-bit sample values. On tiny TIFF fixtures the generic image reader's
pixel-sized allocation limit hides metadata; direct TIFF metadata decoding
confirms the embedded profile. The exported TIFF was not missing its profile.

## Original foundation record

The following describes the initial isolated prototype; application routing and
controls above supersede its original limitations.

The experimental renderer now implements explicit decoded-input interpretation,
conversion to linear DaVinci Wide Gamut, exposure in stops, provisional SDR output,
and intermediate-stage capture. This is an executable foundation, not a finished
professional grading engine or an app-wide replacement.

## Implemented boundaries

- `color_engine/config.rs`: strict, serializable configuration with explicit
  primaries, transfer function, reference domain and rendering revision. Unsupported
  controls are rejected rather than silently ignored.
- `spaces.rs`: D65 sRGB/DWG transforms and reference transfer math. DWG and
  Intermediate constants follow [Blackmagic's published specification](https://documents.blackmagicdesign.com/InformationNotes/DaVinci_Resolve_17_Wide_Gamut_Intermediate.pdf).
- `plan.rs`: validated settings, GPU parameter packing, deterministic fingerprint
  incorporating source revision. There is no v3 cache or sidecar integration yet.
- `renderer.rs` and `shaders/color_v3`: real GPU execution with f32 buffers and
  bounded pixel chunks. Negative and above-white values survive the working and
  exposure stages. Destination clipping occurs only at output.
- Preview PNG8 and export PNG16 derive from the same floating-point rendered
  frame. Working and graded images can be captured as float EXR.
- Versions 0/1/2 keep the existing renderer. Version 3 and unknown versions are
  rejected by the old processor; v3 requires the explicit experimental API.
  Existing highlights/shadows shader code is untouched by this milestone.

## Two explicit output routes

Already-rendered sRGB photos use `display_passthrough_v1`: decode, convert,
adjust exposure, convert back, clamp to the destination, encode. Neutral settings
preserve in-range source colors within floating-point error.

Scene inputs use `scene_shoulder_v1`: a provisional maximum-channel shoulder
and negative-channel compression before sRGB encoding. This is deliberately a
small, inspectable test transform, **not Resolve's rendering transform** or a
production-quality gamut mapper. It needs visual evaluation before release.

## Evaluate outside the application

From the repository root, using an actual sRGB-encoded image:

```sh
cargo run --manifest-path src-tauri/Cargo.toml --example render_color_v3 -- \
  /absolute/path/to/srgb-photo.jpg \
  /absolute/path/to/new-evaluation-folder \
  docs/color-v3-srgb.json
```

The output directory must not already exist. Results are `preview.png`,
`export-16.png`, `working-linear-dwg.exr`, `graded-linear-dwg.exr`, and a manifest
recording configuration, source fingerprint, dimensions and rendering time.
Change `exposure_stops` in a copy of the example config to evaluate exposure.

Pass `auto` instead of `docs/color-v3-srgb.json` to use the new profile-aware
PNG/JPEG adapter. It reads embedded RGB ICC profiles, preserves alpha and
orientation, and records the input interpretation. Ordinary untagged photos use
a documented sRGB fallback; unsupported or ambiguous inputs produce errors.
See `color-engine-input-audit.md` for exact coverage and RAW findings.
Pass `raw` or `raw-fast` to evaluate the separate Bayer RAW development path
with quality or reduced-resolution demosaicing. It validates camera metadata,
preserves calibrated RGB headroom, and uses the provisional scene output route.
Unsupported sensor layouts/calibration return errors; this is not yet universal
RAW support or an app-wide switch.

Explicit JSON mode bypasses automatic interpretation; its config describes the
actual decoded pixel values. The supplied sRGB config must not be used for
arbitrary wide-gamut or linear images. Both output PNGs now embed an sRGB ICC
profile. Monitor-profile conversion is not yet implemented. EXR stage space is
in the manifest.

## Verification

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test color_v3_contracts
```

Contracts cover published DWG/Intermediate values, signed/HDR transform
roundtrips, strict configuration and version handling, neutral GPU rendering,
chunk boundaries, capture/no-capture agreement, preview/export quantization,
PNG16 roundtrip, linear exposure headroom, monotonic neutral scene output,
equivalent linear-sRGB versus DWG/Intermediate sources, and invalid inputs.
The GPU test fails rather than silently skips if no adapter is available.
These are numerical contracts, not perceptual certification or Resolve matching.

Verified on this machine on September 19, 2026: 3 v3 contract tests (including
real GPU execution), 4 existing precision tests, the production shader-layout
test, 6 highlight-detail tests and 4 shadow-lift tests passed. `cargo check --lib`
also passed. The developer example successfully rendered the 1920×1080 local
highlight reference chart and wrote both PNG outputs, both EXR stages and its
manifest; the preview was visually inspected for gross artifacts.

## Next implementation gates

1. Audit actual decoder output, carry verified source metadata through loading,
   and add ICC/camera input handling. Establish a fixed Resolve comparison setup
   plus representative RAW, skin, saturated color and highlight fixtures.
2. Implement v3 primary controls, including an explicit port/calibration of
   highlights/shadows. Preserve old-edit rendering; do not promise identical
   slider response merely by copying formulas into a different working space.
3. Build perceptual selective-color controls and a measured output/gamut mapper.
4. Integrate LUTs, masks, scopes, persistence, profile-aware display/export and
   an explicit opt-in UI only once its controls are supported.
5. Optimize GPU-resident preview, stage capture and memory. This prototype reads
   every stage back even when captures are discarded, and float captures consume
   substantial CPU memory. Do not use it as a performance benchmark for final UI.

The existing render-quality suite has two previously established baseline failures
(`v2_contrast_is_hue_stable_and_filmic_rolls_off` and `tonal_dials_zone_contract`).
Those are separate work; passing v3 contracts does not mean the entire legacy
quality suite passes.
