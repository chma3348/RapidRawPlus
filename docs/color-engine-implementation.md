# Color Engine v3 — coding design and implementation sequence

This is the implementation framework; the first experimental slice is now coded.
It translates `color-engine-roadmap.md` into boundaries, interfaces, and small
delivery batches. The separate 16-bit export improvement is already implemented;
most of the full color rework below remains planned. See
`color-engine-v3-progress.md` for implemented scope and evaluation instructions.

## 1. Keep the application; replace its color-processing subsystem

Keep React controls, image navigation, rawler decoding, geometry, masks, GPU
scheduling, and export encoders. Adapt their color-dependent boundaries where
needed. Preserve the legacy renderer and its parameter mapping for v1/v2.

Proposed new files:

```text
src-tauri/src/color_engine/
  mod.rs            # Version dispatcher and public render-plan API
  config.rs         # Persisted pipeline configuration and revision identifiers
  input.rs          # Resolve source profile/encoding; prepare input transform
  spaces.rs         # Matrices, white points, transfer functions, reference math
  parameters.rs     # Translate UI values into documented v3 control parameters
  plan.rs           # Validate stage order, color spaces, looks, and output route
  output.rs         # Output-rendering configuration and destination transforms
  reference.rs      # Test-only fixture manifest and intermediate-stage capture

src-tauri/src/shaders/color_v3/
  spaces.wgsl       # Explicit conversions between named color representations
  primary.wgsl      # White balance, exposure, tone controls, grading
  selective.wgsl    # Perceptual hue/chroma/lightness adjustments
  output.wgsl       # Rendering transform, gamut mapping, output encoding
  main.wgsl         # Calls stages in one explicit order

src-tauri/tests/
  color_v3_contracts.rs
  color_v3_rendering.rs
  color_v3_reference.rs
  color_v3_persistence.rs
```

These are logical modules, not necessarily separate GPU passes. Assemble WGSL
sources deterministically and validate the assembled shader with naga. Fuse
pointwise stages into one pass; use extra passes only for neighborhood analysis
such as edge-aware selective-color filtering. Do not build a general-purpose node
editor or arbitrary render graph for this project.

## 2. Make image interpretation explicit

Currently `DynamicImage`, `is_raw`, and implicit conventions carry too much
responsibility. Introduce a description alongside the pixels, not a new decoder:

```rust
// Interface sketch; referenced enums/handles require implementation.
struct SourceColorDescription {
    primaries: Primaries,
    white_point: WhitePoint,
    transfer: TransferFunction,
    reference: ReferenceDomain, // scene-referred or already rendered
    profile: Option<ProfileId>,
    calibration: CalibrationId,
}

struct DecodedFrame {
    pixels: DynamicImage,
    color: SourceColorDescription,
    source_revision: SourceRevision,
}

struct PipelineConfig {
    process_version: u32,
    working_space: WorkingSpace,
    rendering: RenderingTransformId,
    looks: Vec<LookContract>,
}

enum RenderDestination {
    Preview { display_profile: ProfileId },
    Export { profile: ProfileId, encoding: OutputEncoding },
}
```

Audit rawler's actual developed output before filling this description. Do not
label decoded sRGB-basis pixels as camera RGB or apply camera white balance twice.
Matrix/TRC profiles can use precomputed transforms; complex ICC profiles require
a supported color-management implementation, not a fabricated 3×3 approximation.
An untagged-image fallback must be explicit. A JPEG converted into a wide working
space is still an already-rendered image; the conversion does not recover RAW
highlight range or reverse the camera's look.

Keep the saved creative configuration separate from destination settings.
Changing monitor profile must not change saved edit values. Include resolved
profile, input calibration, process version, and rendering/look revisions in the
relevant cache keys; do not key rendered output solely on slider values.

## 3. Compile settings into an explicit render plan

```rust
fn build_v3_plan(
    source: &SourceColorDescription,
    edits: &V3Adjustments,
    config: &PipelineConfig,
    destination: &RenderDestination,
) -> Result<V3RenderPlan, ColorError>;

fn render_dispatch(request: &ImageRenderRequest) -> Result<RenderedImage, RenderError> {
    match request.process_version {
        1 | 2 => render_legacy(request),
        3 => render_v3(build_v3_plan(/* source, edits, config, destination */)?),
        _ => Err(RenderError::UnsupportedProcessVersion),
    }
}
```

The pseudocode omits concrete argument plumbing. The important change is a
separate v3 path: avoid sprinkling `process_version >= 3` throughout the existing
shader and accidentally inheriting legacy corrections. Old parameter values
continue through the existing mapper. A v3 mapper defines units and response
curves independently, with a separate GPU parameter layout and layout test.

The plan should resolve matrices, LUT resources, white-balance adaptation,
operation parameters, and output encoding once per relevant configuration change.
Only genuinely per-pixel work belongs in WGSL.

## 4. The actual pixel pipeline

For a scene-referred source with the ordinary SDR rendering route:

```text
Decoded pixels + source description
  → input transfer/profile conversion
  → scene-linear working RGB
  → white balance and exposure
  → tone controls and grading
  → selective color, with explicit perceptual-space conversion
  → optional scene-referred look
  → one output-rendering/gamut-mapping transform
  → destination color encoding
  → final display/export quantization
```

Rendered JPEG/TIFF sources need a defined rendered-source route so their baked-in
display appearance is not automatically tone-mapped again. A film LUT that already
produces a display image also needs its own declared route. The plan rejects
incompatible or unknown look contracts in v3 rather than silently guessing.

Proposed WGSL exposure operation:

```wgsl
fn exposure_linear(rgb: vec3<f32>, stops: f32) -> vec3<f32> {
    return rgb * exp2(stops);
}
```

This operation does not clamp. +1 stop doubles scene-linear values even above 1.
The final display response is allowed to roll off highlights; a test must observe
the exposure stage before output rendering to verify the doubling property.

For selective color, the conceptual operation is:

```text
working RGB → correctly adapted XYZ → perceptual lightness/chroma/hue
evaluate smooth hue selection weights
apply hue offset, chroma scale, and deliberate lightness adjustment
convert back through XYZ → working RGB
```

Use Oklab as the first measured prototype because we already use it, with correct
working-space conversion and defined handling of negative/HDR values. Compare UCS
only when fixtures show a worthwhile advantage. Do not feed new wide-gamut RGB
directly into the existing sRGB-specific Oklab matrices. Do not demand that every
color operation preserve both physical luminance and perceived brightness; choose
and test the intended property per control.

## 5. A contract for every control

| Control | Proposed contract | Test that matters |
|---|---|---|
| Exposure | Stops in linear working RGB; no pre-output clamp | +1 stop doubles values before rendering, including values above 1 |
| Contrast | Defined pivot and response; neutral value is identity | Smooth ramp, fixed pivot, bounded hue drift under the selected model |
| White balance | Adaptation from declared source/reference white | Neutral target mapping; no double camera adaptation |
| Saturation | Chroma change with a documented preserved quantity | Neutral axis remains neutral; controlled approach to gamut limits |
| Hue adjustment | Smooth hue rotation with deliberate lightness policy | Wraparound continuity, no unintended brightness jump, stable low-chroma behavior |
| Shadow/highlight grading | Defined zones and overlap | Continuous zone boundaries; predictable combined adjustments |
| Local adjustment | Same operator as global, with defined mask blending | Full-white mask matches global; zero mask matches bypass |
| Output | One explicit render/encode route | Profile-aware preview/export agreement and retained high-bit-depth precision |

The UI can retain its familiar sliders. `parameters.rs` defines how their values
map into stops, angles, chroma gains, and tonal curves. Matching a Resolve control
means measuring its response under fixed settings, not assuming the same slider
number means the same operation.

## 6. Implement in six reviewable batches

### Batch A — Baselines and dispatcher

Capture current outputs; retain the two known failing tone tests as documented
baseline failures. Add v3 dispatch and configuration persistence behind a disabled
feature flag. Unknown versions fail explicitly. Preserve v1/v2 output fixtures.
No user-visible color change yet.

### Batch B — One complete, minimal pipeline

Support one accurately tagged test input, exposure, a provisional fixed SDR output
transform, preview, and 16-bit export. Capture the input, working-space, post-grade,
and output stages. Use existing export precision infrastructure. Start with shared
decoded fixtures; extend native RAW and ICC input support after verifying their
interpretation. The output transform remains provisional until reference review.

Deliverable: changing exposure produces the expected intermediate values and a
predictable final image. This proves the architecture before adding other tools.

### Batch C — Primary controls

Add white balance, contrast, tonal zones, saturation, and grading one family at a
time. Each change includes isolated sweeps, combined adjustments, comparison
images, and a holdout evaluation. Replace the provisional output transform only
through an explicit revision and rebaseline decision.

### Batch D — Selective color

Port HSL, Point Color, and hue curves to consistent v3 semantics. Add spatial
correction filtering only where the tests show blotches or noisy selection.
Account for preview scale, ROI origin, and tile padding. Reject visible seams.

### Batch E — Existing feature integration

Integrate masks, LUT contracts, presets, AI patches, scopes, caches, undo, and
reopening. Store picked-color coordinates with a defined source space/revision.
Make cross-version preset application and edit upgrade explicit. Unsupported v3
features remain disabled in the experimental route; never silently ignore them
or run them against an incompatible color space.

### Batch F — Release and performance

Measure warm slider latency, settled preview, export, and GPU memory. Optimize
conversion reuse and GPU passes without changing the intended color math. Make v3
the default only for new edits after acceptance. Keep old edits reproducible.

## 7. How we establish Resolve similarity

Use a versioned reference manifest containing source hashes, decoder/input
interpretation, Resolve version, project settings, named tool family, exact
adjustments, output transform/profile, and exported reference files. Feed both
engines the same decoded images to isolate grading from RAW development.

Evaluate RGB/perceptual differences, neutral drift, highlight separation, local
boundaries, and subjective contact sheets. Some intentionally different operations
will not be pixel-identical. Do not conceal that with arbitrary per-image tuning.
Use separate calibration and holdout images. A CPU math prototype can guide work,
but acceptance measurements must come from the actual GPU pipeline.

Refinement to the earlier audit: the current parameter builder removes all-zero
hue curves. Therefore a mathematically non-identity shader path with an active
neutral curve is not sufficient proof that a neutral UI action triggers it.
Regressions must cover the whole UI-parameter-to-shader route as well as active
nonzero curves on HDR inputs. This is why the framework includes both contract
tests and end-to-end rendering tests.

## First concrete coding task

Implement Batch A and the smallest slice of Batch B: version dispatcher, explicit
source/working/output descriptions, one exposure operation, stage capture, and
real-GPU identity/precision tests. No new sliders, no complete rewrite, and no claim
of finished Resolve matching. That gives all later work a stable place to live.
