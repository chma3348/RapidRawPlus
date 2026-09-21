# Color engine v3: consistency and Resolve-oriented grading

Status: implementation started, September 19, 2026. App defaults remain unchanged.

Implementation update: the export precision bottleneck is addressed separately
in `export-precision.md`. The inventory below records the baseline before that
change. An isolated exposure/input/output prototype is implemented; see
`color-engine-v3-progress.md`. The remaining broader pipeline is still planned.

## Outcome and recommendation

Build a versioned, explicitly color-managed processing pipeline, with stable control behavior and measurable preview/export agreement. Use a fixed Resolve workflow as a behavioral and visual reference. Borrow suitable techniques from the provided Photo Editer sources; do not transplant their entire engine or assume their output matches Resolve.

Success means small slider movements produce smooth, predictable changes; neutral tools do nothing; bright colors retain useful headroom; grading does not introduce unexplained casts or brightness shifts; saved edits reopen consistently; preview and export agree after accounting for their output profiles. Cross-camera consistency depends on input characterization and cannot mean that unrelated RAW and JPEG encodings respond identically or recover the same information.

An exact clone of Resolve's proprietary rendering is not the acceptance criterion. Resolve's result also depends on project settings, input interpretation, tool family, and output transform. Define that reference before fitting behavior.

## What the current code establishes

- `src-tauri/src/shaders/shader.wgsl`: HSL uses linear-RGB HSV, hue curves use gamma-encoded HSV, newer saturation uses Oklab, and grading applies additive RGB offsets. Multiple spaces are not inherently wrong, but conversions and behavior need explicit contracts.
- `apply_hue_curves` clamps channels before display rendering; an enabled neutral curve can change HDR colors. Previous arithmetic reproduction establishes a candidate defect; add a real-GPU regression before claiming an end-to-end fix.
- `compress_gamut_soft` addresses negative channels, not a complete output-profile gamut boundary. Values above 1 are not inherently errors in scene-linear processing and must not be indiscriminately clamped.
- RAW/basic, AgX, filmic, film-simulation LUTs, and late display adjustments take distinct routes. RAW/basic includes empirical display matching. Inventory their intended roles before consolidating them.
- Shader output is `rgba8unorm`; `gpu_processing.rs` returns rendered `ImageRgba8`. A later 16-bit conversion in the export encoder cannot recover precision lost there. This is a professional export limitation, not proof that 8-bit output causes every current visual complaint.
- `raw_processing.rs` uses rawler development with special handling for linear RAW and highlight preprocessing. Confirm the actual output primaries, white point, transfer function, and any irreversible changes; do not infer these from `is_raw` or float storage alone.
- `src/utils/adjustments.ts` already persists `processVersion`, retaining v1 for older sidecars. Preserve that pattern for v3 and retain both existing versions.
- `src-tauri/tests/render_quality.rs`, the headless render harness in `gpu_processing.rs`, and `scripts/compare_image_pair.py` are reusable. The current comparison script mainly measures encoded RGB differences; it is not a complete perceptual color benchmark. GPU tests that skip without an adapter cannot count as release validation.
- The supplied reference is a collection of modules, not a complete build. Color Equalizer uses perceptual color coordinates, spatial filtering of corrections, and profile-based gamut mapping. RGB Color Balance separates several color/brightness operations. Their supporting helpers, integration assumptions, and licensing need review before any direct code reuse.

## Target architecture

1. Decode and characterize input: camera calibration/white balance for RAW; embedded profiles and transfer functions for rendered files. Define a documented fallback for untagged files. Retain source metadata and provenance.
2. Convert to a documented floating-point working space. Prototype linear DaVinci Wide Gamut as the leading candidate, with explicit conversion to/from DaVinci Intermediate for tools that benefit from logarithmic response. Verify published specifications before implementing. Changing gamut alone does not improve controls or recreate Resolve.
3. Apply technical corrections and exposure, then defined tone and creative color stages. Each operation declares input space, output space, units, neutral state, HDR behavior, and clipping policy.
4. Apply looks with declared input/output contracts. A display-producing film LUT replaces the ordinary scene-to-display rendering step; it must not be followed by a second unintended display transform. Legacy ambiguous LUTs retain their old route until explicitly configured for v3.
5. Apply the selected output rendering and output-profile conversion exactly once. Initially ship one recommended SDR rendering path. Preserve legacy alternatives under their existing process versions.
6. Branch into display presentation and export encoding. Share the scene rendering and intended output appearance, but account for the monitor profile versus the file's destination profile. Quantize/dither only where required by the final destination.

Use f32 arithmetic with float textures where suitable. Start by measuring existing half-float storage; use f32 storage where precision tests justify the bandwidth. A blanket precision upgrade is not a substitute for fixing transforms.

Do not apply today's hard-coded sRGB Oklab matrices or luma weights directly to a new working RGB gamut. Audit Oklab conversion, luma, white balance, masks, scopes, AI patch encoding, LUT matrices, and every sampler consuming those pixels.

## Phase 0 — Establish the reference and failure cases

Effort: small to medium. Value: essential; prevents expensive work being aimed at an undefined look.

- Collect approximately 20–30 representative photos: several skin tones, daylight and mixed light, foliage, deep blues, saturated reds, stage/neon light, low-light noise, underexposure, specular highlights, and neutral gradients. Include the user's actual failures. Keep a separate holdout set for judging changes.
- Choose a fixed Resolve version/project with explicit input tagging, working space, output transform, data levels, and disabled automatic effects. Proposed reference: a color-managed DWG/Intermediate SDR workflow. Freeze which primary/HDR controls we are trying to emulate; they are not interchangeable.
- Separate decoder matching from grading matching. Feed both engines identical decoded, tagged float images or appropriately tagged high-bit-depth rendered images for control tests. Evaluate native RAW development separately.
- Export neutral renders, modest and strong single-control sweeps, and representative combined grades. Use common comparison encoding/profile; do not compare an sRGB export directly against a differently encoded Rec.709 export or screenshots.
- Record baseline preview latency, export time, GPU memory, and full-resolution versus preview differences on the user's hardware.

Deliverable: reproducible fixture manifest, reference settings, baseline images/metrics, ranked visible defects. If Resolve reference renders are unavailable, correctness work can proceed, but label behavior matching unvalidated.

Gate: reproducing the same input/settings produces the same reference output within declared precision tolerances. Reference acquisition is a dependency for claiming Resolve similarity, not for fixing objective defects.

## Phase 1 — Precision, color contracts, and safe versioning

Effort: large. Value: highest; necessary for dependable professional output.

- Add explicit image color metadata instead of using `is_raw` as the only interpretation signal. Audit RAW calibration and highlight preprocessing, embedded ICC handling, untagged fallback, AI patch conversions, and preview caches.
- Add opt-in `processVersion: 3`, persist the pipeline configuration, and include the resolved configuration in cache keys. Avoid predicates such as `>= 2` silently applying v2-specific empirical corrections to v3.
- Add a float render/readback path for verification and high-bit-depth export. Produce genuine 16-bit output for at least one supported export format, with correct profile tagging. Keep the current display path where suitable.
- Extract explicit source/working/tool/output transforms into testable modules. Share the pipeline between preview/export; audit monitor presentation for double encoding or profile conversion.
- Prototype the candidate working space on fixtures. Correct every basis-dependent operation before judging its appearance. Choose a single default output rendering using the reference/holdout set; retain alternative experiments behind the v3 flag.
- Retain legacy sidecars/presets unchanged. Upgrading an existing edit must be explicit, reversible, and presented as potentially changing its appearance. Reusing the same slider values is not a guaranteed visual migration.

Likely touchpoints: `raw_processing.rs`, `image_loader.rs`, `app_state.rs`, `image_processing.rs`, `gpu_processing.rs`, shader assembly/bindings, display surface code, `export_processing.rs`, `cache_utils.rs`, and `src/utils/adjustments.ts`.

Gate: declared transforms round-trip within numerical tolerances; neutral operations preserve values before output rendering; an HDR ramp retains distinctions above 1; high-bit-depth export contains real extra precision; old fixture renders remain unchanged; preview/export match in a common output space.

## Phase 2 — Predictable primary controls

Effort: medium to large. Value: very high; this is the everyday grading feel.

- Implement exposure as a defined scene-linear stop adjustment; define brightness separately or retain it as a clearly specified creative control.
- Specify contrast pivot and tone response without accidentally changing hue. Define shadow/highlight influence zones and smooth transitions, then test combinations, not only isolated sliders.
- Implement white-balance adjustment with an explicit source/reference white relationship, avoiding double application of camera white balance. Separate creative tinting from technical adaptation.
- Replace v3 additive tint behavior with a documented grading design. Measure the intended shadow/midtone/highlight control response against the chosen Resolve tools. Preserve intentionally available offsets instead of forcing every tool to preserve luminance.
- Revisit saturation and vibrance as a coherent family. Decide which property each preserves: luminance, perceived lightness, or perceived brightness. A hue-based skin guard is not a skin detector.
- Fix hue-curve clipping in v3 and establish identity behavior for all enabled-but-neutral tools. Any optional backport to v1/v2 requires a separate compatibility decision.

Gate: slider sweeps are continuous and monotonic where appropriate; neutral settings are identity; no NaNs or unintended pre-output clipping; hue/brightness side effects stay within each tool's declared contract; the combined grades improve on baseline and holdout images.

## Phase 3 — Selective color and clean transitions

Effort: medium to large. Value: high, after the foundation is stable.

- Unify the semantics of HSL, Point Color, and hue curves using one documented perceptual representation with explicit conversion from working RGB. Prototype existing Oklab versus the reference's UCS approach on the same fixtures; choose on results, HDR behavior, performance, and implementation cost, not the name of the color space.
- Preserve brightness or lightness intentionally during hue shifts and define how saturation changes approach the output gamut boundary.
- Add edge-aware smoothing of color selection/corrections where noisy membership produces blotches. Borrow the reference's principle of filtering corrections, not blurring the final photograph. Keep this off for simple global controls unless needed.
- Test neutral colors, hair/sky boundaries, skin gradients, fine fabric, JPEG blocks, and mask overlaps. Scale spatial radii correctly across preview, full resolution, ROI, and tiled output; provide filter padding at tile boundaries.

Gate: fewer visible blotches/boundaries than baseline; no added halos or detail loss on holdout images; equivalent regions do not change color merely because zoom, preview resolution, or export tiling changes.

## Phase 4 — Integration, responsiveness, and release

Effort: medium, with risk depending on the display/export audit. Value: mandatory for shipping.

- Verify LUT/preset contracts, local/global adjustment equivalence, mask compositing, AI patches, cache invalidation, undo, reopen, thumbnails, and batch export.
- Ensure scopes have a documented measurement stage. Scopes should help verify the image rather than conceal an extra transform.
- Profile while dragging controls. Cache invariant conversions and LUTs; fuse pointwise passes where worthwhile. Separate perceptual improvements from UI scheduling/stale-preview issues when diagnosing clunkiness.
- Establish latency/memory budgets from Phase 0. Provisional guardrail: investigate more than 20% regression in p95 interactive latency or memory before accepting a change; replace this with measured product targets. Do not buy speed by changing color mathematics between previews and exports.
- Release as an opt-in preview, compare A/B on real projects, then make v3 default for new edits only after acceptance. Retain versioned rendering for existing work.

Gate: release GPU checks actually execute on supported hardware; reopen/undo are stable; preview/export and high-bit-depth checks pass; user accepts representative before/after results; no material legacy regressions.

## Measurement and acceptance rules

Separate three questions: mathematical correctness, agreement with a selected Resolve workflow, and user preference. They require different evidence.

- Numerical invariants: finite values, neutral identity, transform round trips, exposure ratios before output rendering, defined boundary behavior, and bounded precision error. Set tolerances from data format and intended operation, not from whichever output happens to pass.
- Image quality: perceptual color differences under a common transform, hue drift only where chroma is meaningful, neutral-axis drift, highlight separation, clipping counts, gradient continuity, and boundary/halo inspection. RGB RMS alone can reward a duller image and miss local defects.
- Persistence/output: identical saved settings and input interpretation must reproduce results; compare preview and export after matching spatial scale and profiles. Spatial operations need dedicated resolution/tiling checks.
- Reference response: compare modest and strong adjustments, positive/negative sweeps, and combinations. Similar slider numbers do not automatically represent equal effects in different applications.
- Visual judgment: inspect full-size and fit-to-screen contact sheets, including unseen holdout images. Do not optimize per-photo constants against the evaluation set.
- Performance: cold load, warm slider interaction, settled preview, high-resolution export, and peak GPU memory. Count skipped GPU tests as missing evidence.

## Where effort is and is not justified

| Work | Recommendation | Reason |
|---|---|---|
| Input interpretation, output/display agreement | Essential | Wrong transforms contaminate every later judgment. |
| HDR-safe operations and real high-bit-depth export | Essential | Lost information cannot be recovered by better sliders. |
| Real-GPU regressions and fixed reference fixtures | Essential | We already have useful infrastructure; extend it. |
| Consistent primary controls | Essential | Most edits depend on these; highest visible daily benefit. |
| Perceptual selective color and edge-aware corrections | Worth doing next | Directly targets unnatural transitions and blotches. |
| DWG/Intermediate working model | Prototype, then adopt if validated | Supports a Resolve-oriented design but is not itself a look. |
| Full import of reference modules | Avoid | Missing dependencies, integration cost, and different design goals. |
| Exact reverse engineering of every Resolve control | Defer | High effort, uncertain match, weak return compared with core consistency. |
| More LUTs, film looks, or ad-hoc per-image correction constants | Defer | Can obscure rather than repair foundational defects. |
| HDR monitor output, broad print proofing, many new output modes | Later | Stabilize one well-managed SDR path first. |
| Rewrite demosaicing or implement a new raw decoder | Only if measurements implicate it | Reuse the decoder; audit/calibrate its output first. |
| UI redesign or extra controls | Later | It does not fix image mathematics; responsiveness still needs measurement. |

## First implementation batch

1. Preserve current fixture outputs and document existing pipeline branches.
2. Extend the real-GPU harness with neutral hue-curve HDR, grading brightness side effects, and precision/export regressions. Side effects are failures only against an explicitly chosen v3 contract.
3. Add float stage capture and profile-aware comparison; confirm the exported precision bottleneck end to end.
4. Introduce the v3 opt-in and explicit transform/configuration boundaries, preserving old renders.
5. Demonstrate one vertical slice: tagged input → working-space exposure → fixed SDR output → matching preview and true 16-bit export.
6. Review those results before building every control on top. This is the first meaningful decision checkpoint.

The work is a multi-stage engineering project, not a reliable one-session slider patch. Precision/output integration and source interpretation have the largest uncertainty. Re-estimate implementation effort after the first vertical slice; commit to evidence-backed milestones rather than a calendar promise before profiling and reference capture.

## Reference inputs needed before behavior matching

Useful user input: a small set of disliked RapidRAW examples and the Resolve setup/results they prefer. Until supplied, use a documented neutral SDR reference as a provisional target and continue objective correctness work. Do not claim that a guessed setup reproduces the user's preferred Resolve behavior.

Blackmagic reference: https://documents.blackmagicdesign.com/UserManuals/DaVinci-Resolve-17-Colorist-Guide.pdf (color-managed working-space concepts; pin the actual installed Resolve version for measurements).
