# V3 input audit and implementation

## Why this is engineering work, not a user prerequisite

Floating-point storage does not identify a color space or transfer function.
V3 needs to know whether, for example, 0.5 is an encoded photo value or a linear
light value, and which primaries define RGB. Users do not need to identify these
manually for supported tagged photos. Decoder/profile code should do that.

## Findings from the current code

`image_loader::load_image_with_orientation` decodes through `image`, applies
orientation and converts storage to RGB32F. It does not explicitly transform the
embedded ICC profile to the renderer's working space. Changing storage to float
does not itself linearize RGB or perform an ICC conversion. The HEIC system-codec
and half-float TIFF fallback paths require separate characterization.

`raw_processing::develop_internal` removes rawler's sRGB encoding stage and
normally retains camera calibration and white balance. In the pinned rawler
revision `424cc109`, `imgop/raw.rs::map_3ch_to_rgb` constructs its camera-to-RGB
matrix using `SRGB_TO_XYZ_D65`: the intended calibrated output basis is linear
sRGB, **not camera RGB**. However:

- Missing or invalid calibration matrices can cause calibration to be skipped.
- LinearRaw options explicitly allow skipping calibration and applying an
  assumed sRGB inverse transfer. These must not get an unconditional color label.
- The dependency's calibration helpers call `clip_euclidean_norm_avg`, which
  removes negative channels and can modify high values.
- Our own development then rescales values, desaturates highlights and clamps
  to a configurable ceiling (or 1.0 for the fast path).

Therefore the existing RAW result is not an untouched scene-linear master.
Simply relabeling it, disabling one final clamp, or raising precision would not
undo those operations. Legacy behavior is intentionally unchanged. A dedicated
v3 RAW development path must move irreversible appearance adjustments out of
decoding and validate camera calibration, fast/full consistency and highlight
handling with RAW fixtures before being enabled.

## Implemented now

`color_engine/input.rs` provides a separate PNG/JPEG adapter:

- Reads embedded RGB ICC profiles and converts to floating-point linear sRGB
  coordinates with relative-colorimetric intent, then lets the GPU perform the
  existing conversion to linear DWG. Linear encoding does not change the source
  domain: rendered photos remain display-referred.
- Retains out-of-sRGB negative/above-one coordinates for matrix-profile input;
  tested using Display P3 red. No intermediate 8-bit conversion.
- Applies orientation and preserves straight alpha.
- Records profile hash, decoder revision, interpretation and fallback warnings.
- Uses an explicit sRGB fallback for ordinary unprofiled PNG/JPEG. Rejects
  conflicting PNG gamma/chromaticity metadata, CICP/HDR declarations, EXIF
  non-sRGB/uncalibrated declarations, corrupt ICC, and unsupported profile types.
- Rejects other formats in automatic mode instead of guessing. Gray/CMYK ICC
  conversion, untagged CMYK characterization, TIFF/HEIC/HDR and RAW adapters
  are not completed. The ordinary-image sRGB fallback is an assumption, not
  proof of an untagged file's color space.
- Writes embedded sRGB ICC profiles in both developer preview/export PNGs.

`render_color_v3` now accepts `auto` instead of a configuration path to use this
adapter. Explicit configuration mode remains available for controlled fixtures
and intentionally bypasses automatic input interpretation. Manifest fingerprints
include interpretation provenance as well as source contents and render settings.

## Next gates

Expand and validate camera RAW and remaining format adapters; attach these descriptors to
v3 app loading/caches without changing legacy pixel interpretation. Then proceed
with primary controls and visual reference fixtures. Monitor-profile conversion,
full gamut mapping and app-wide v3 controls remain separate work.

## Bayer RAW increment

`color_engine/raw.rs` now provides a separate experimental RAW path. It reuses
the pinned rawler file decoder, demosaicing and cropping, but does not call its
calibration/color-clipping stage or the legacy RapidRAW highlight processing.

The supported subset is 2×2 RGB Bayer mosaics with even dimensions, supported
black-level layouts and a single sensor white level, a finite invertible decoder-reported D65 3×3 matrix,
and valid as-shot RGB white balance. Unsupported inputs fail explicitly:
X-Trans, four-color CFA, monochrome and LinearRaw, missing D65 calibration,
invalid levels, invalid white balance and ill-conditioned matrices are not
silently guessed or routed through the legacy path.

Pipeline:

1. Subtract the correct Bayer-site black levels and divide by their white-minus-
   black spans. Values below the sensor black floor go to zero; there is no upper
   clamp. This is not a claim of preserving below-black noise statistics.
2. Demosaic and crop without rawler rescaling, calibration or gamma encoding.
3. Apply a checked camera-to-linear-sRGB matrix and as-shot white balance,
   normalized to green. The calibration follows the existing row-normalized
   camera-matrix convention, calculated in f64. The resulting f32 RGB may be
   negative or above one: no highlight desaturation or RGB gamut clipping here.
4. Apply orientation and supply a scene-referred descriptor to v3. The existing
   renderer converts to DWG and uses its provisional scene-output transform.

Provenance records the camera, source matrix, effective transform, white balance,
black/white levels, demosaicing mode and calibration revision. Cancellation is
checked between stages; dependency decoding/demosaicing is not interruptible.

Use `raw` or `raw-fast` in the developer example's final argument to evaluate
quality or reduced-resolution demosaicing. Neither mode enables v3 in the app.
The old RAW developer and its highlight/shadow behavior remain untouched by this
increment. This does not reconstruct channels already saturated at the sensor,
implement dual-illuminant calibration or promise a Resolve-equivalent appearance.

Five RAW tests pass, including a generated uncompressed DNG passed through the
real file decoder, both actual demosaicers on a flat field, signed/HDR matrix
behavior, level normalization, metadata rejection and cancellation. Together
with the seven photo-input tests, twelve input tests pass. Real-camera scene
validation remains outstanding: synthetic fixtures prove numerical contracts,
not perceptual quality or universal camera support.

## Validation of this increment

Passed: seven input tests (sRGB decode, P3 headroom, invalid metadata, unsupported
gray profile, JPEG domain, orientation/alpha, tagged-versus-fallback agreement),
three v3 contracts including real-GPU tagged PNG16 export/reopen/rerender,
four existing precision tests, six highlight-detail tests and four shadow-lift
tests. The automatic adapter also rendered the local 1920×1080 reference chart
and wrote tagged preview/export PNGs and intermediate EXRs.

The roundtrip test exposed a near-black mismatch caused by comparing serialized
ICC colorants with an unquantized built-in profile. The conversion destination
and fallback now use the same serialized sRGB characterization as the export,
and the tagged export/reopen test passes its existing 0.00005 encoded-channel
tolerance. This is not a claim of exact color matching across arbitrary profiles
or monitors.
