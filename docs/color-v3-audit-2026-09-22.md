# V3 colour-engine audit — September 22, 2026

## Verdict

Keep the float/DWG foundation, shared preview/export entry point, profiled
decoding, isolated spatial stages, and the new controls. There is useful
engineering here, not a reason to start over. However, coverage is not the
same as professional consistency. Several caches and measurement tools had
stopped reflecting the pipeline they were supposed to describe.

This audit fixes demonstrable correctness problems without retuning the
creative sliders or modifying the legacy engine. It is not certification of
Resolve equivalence. The reported earlier Resolve comparisons were neutral
renders on a limited photo set; they do not validate every slider, camera,
colour space, spatial effect, or combined grade.

Scope: the v3 Rust modules and shaders, their application/export integration,
control serialization and preset/history tests, the new detail/effects/patch
stages, LUT handling, and the baseline/Resolve-comparison utilities. Frontend
integration was inspected, but this was not a full interactive UI or
accessibility audit. Existing uncommitted work in
`src-tauri/examples/color_v3_timing.rs` was preserved unchanged. No photo,
sidecar, installed transform, or saved preset was rewritten.

## Fixed, and why

| Problem | Change | Expected effect |
| --- | --- | --- |
| Centre's clarity component was absent from its spatial cache key. | Key includes Centre; regression compares repeated slider changes against fresh rendering, including reset to zero. | Dragging/reopening/exporting no longer selects different cached Centre detail. |
| CPU captured-transform sampling was trilinear; GPU sampling was tetrahedral. The old test allowed their disagreement. | CPU now uses the same tetrahedra. Added a nonlinear cube that distinguishes the methods across all six tetrahedra, and tightened GPU agreement from 0.004 to 0.000002. | Input conversion, neighbourhood calculations and display conversion agree more closely. Captured-transform renders may change slightly. |
| Captured cubes accepted missing channels as zero and incomplete domain declarations. | Exact entry/header arity, finite values, duplicate-size rejection, bounded entry count; inline comments supported. | Corrupt data raises an error instead of creating unexplained colour casts. |
| Creative `.cube` LUTs used the legacy parser, which ignores declared domains, and a filename-only cache. | V3 cubes use the strict, file-version-aware parser. Non-unit domains are explicitly refused, not silently misinterpreted. | Replacing a cube updates the look; unsupported cubes fail visibly. The existing captured-cube size limit of 128 now also applies to creative cubes. Other LUT formats retain their existing path. |
| Source cache ignored changes to the installed input transform. Missing/broken transforms could silently fall back. | Input-transform content digest participates in source identity; load errors propagate; a transformed display source requires the input/output pair. | A changed transform cannot reuse an old decoded source. Broken configured transforms are errors rather than a different-looking successful render. |
| The captured input cube clamps to sRGB 0–1, destroying legitimate P3/Adobe RGB values before the grade. | Detect out-of-domain source colours before applying it. Preserve the whole source on the existing display-referred path, recording a warning in provenance and logs. | Wide-gamut photos retain their input colours. **They can look different from the previous clipped result and do not use the captured Resolve rendering.** This is a safeguard, not an extended-gamut inverse transform. |
| Image-dependent mask caches ignored replacement source files and rendering choices. | Sampling identity includes source metadata, neutral settings and transform digests; the same identity reaches the bitmap cache. Tone neighbourhoods also include the output-transform digest. | Warm and fresh renders agree after replacing an image or changing the tone mapper. |
| Custom-range inspection included vignette and film saturation, although the shader selects ranges before those stages. | Disable these two downstream stages in inspection. | The picker targets the colour the range actually evaluates; changing those effects cannot move the picked centre. The inspection image is intentionally a pre-selective diagnostic, not the final grade. |
| Explicit `PipelineConfig` serialization discarded non-neutral Basic settings on reload. | Allow `tone` to deserialize in the standalone config. The application still overwrites it using shared top-level controls, so a nested `v3.tone` cannot bypass application settings. | Exported diagnostic configurations reproduce their Basic settings. |
| Diagnostic fingerprints omitted display-domain lattices, neighbourhood data and render scale. | Include these bound inputs and update the fingerprint implementation identifier. | Different render inputs cannot share the same diagnostic identity merely because the serialized controls match. This does not yet pin transforms in saved edits. |
| Requested stage captures vanished whenever active masks were present. | Return captures through the final masked pass, preserving initial working pixels and final graded pixels. | Diagnostics can inspect masked renders; a GPU test checks that capture does not change output. |
| Installed display input transforms could be passed to patch conversion for native RAW sources. | Only pass the captured input transform for sources actually converted through it (`rendered_origin`). | Display-encoded patches cannot be transformed into DWG and then blended into a native linear-sRGB RAW source. |
| Oversized unchunked LUT bindings could reach GPU validation errors. | Check all lattice buffers against device limits before creating bindings. | An actionable render error replaces an invalid oversized GPU binding. |
| The baseline runner bypassed the app's spatial/domain stages and deserialized obsolete control names, silently making some named tone cases neutral. | Run the actual application pipeline; require explicit `edits` in each case; migrate the bundled test manifest and assert its tone cases are non-neutral. | Baselines now exercise the code used by the editor. Old-format manifests fail instead of producing misleading results. |
| Baseline hue-drift measurement folded rotations above 90° back toward zero. | Use circular `atan2(sin(delta), cos(delta))` distance. | A 180° hue error is measured as 180°, not zero. |

The baseline manifest migration expresses old exposure stops as the shared
EV-shift slider (`stops × 0.8`). Other tone case amounts now address the
current shared sliders. The obsolete 0.18 pivot was removed rather than
pretending it meant the same thing as the shared control's pivot. Therefore
the historical September 20 baseline is retained as history, **not a directly
comparable reference for the new tone cases**. Render timings now include the
application path and cannot be compared to the former GPU-only timings.

## Important remaining work, in priority order

1. **Freeze and persist render identity before further creative retuning.**
   Current edits still use process version 3/control revision 1 across
   materially different implementations. Old `v3.exposure`, `contrast`,
   `shadows`, etc. are ignored by the current Controls deserializer; the shared
   top-level controls replaced them without an equivalent old-render path.
   Machine-local input/output cubes are selected from app support, not pinned
   by each edit. Save an explicit renderer revision and immutable transform
   digests/assets with each edit; preserve an old-render implementation or an
   explicit migration policy. Do not silently remap old creative settings:
   the old and new operators are not mathematically equivalent. This audit
   did not rewrite users' existing edits or invent that equivalence.

2. **Replace the RAW clipped-highlight heuristic with measured recovery.**
   `raw.rs` currently neutralizes and brightens pixels when sensor green
   approaches clipping, regardless of the other channels. Green clipping
   alone does not prove the scene was neutral. It can erase saturated green
   light and bakes that decision into the decoded source. Keep this separate
   from the already-tuned highlights/shadows sliders. Test clipped neutral
   lights, coloured LEDs and skin over multiple cameras; make recovery
   versioned and reversible. The present single-camera sun comparison is
   insufficient evidence for universal behaviour. No heuristic retune was
   guessed during this audit.

3. **Validate combined grades at multiple preview sizes against exports.**
   The app downsizes before detail, glow and nonlinear grading; exports
   process full resolution. Nonlinear operations do not generally commute
   with resizing. In particular a tiny light can disappear before preview
   glow thresholds it, even though the optics module thresholds its own
   input before building its blur grid. Existing flat-field and isolated
   effect tests are valuable but not proof of app-level equality. Add real
   fine-detail/highlight fixtures at interactive, settled and 100% sizes;
   define perceptual tolerances and measure the discrepancy before choosing
   full-resolution precomputation versus faster approximation.

4. **Give the captured-transform path a verified extended-domain policy.**
   The wide-gamut bypass prevents destructive input clipping, but is not a
   uniform Resolve-like rendering across all source gamuts. Display-domain
   round trips also clamp at their cube boundaries. A captured inverse is a
   bounded approximation, not recovery of the camera's original scene from
   an arbitrary JPEG. Test saturated primaries, near-black ramps, negative
   working values and strong exposure. Pair and version transforms; measure
   approximation error rather than merely increasing lattice size. The
   fallback warning currently lives in provenance/logs; expose rendering
   provenance in the UI in a separate interface pass.

5. **Harden the remaining LUT formats and reproducibility tooling.**
   V3 `.cube` validation is fixed; the inherited 3DL path still does not
   establish nonuniform input knots/output integer scaling. Implement a
   format-aware adapter or explicitly refuse unsupported variants. The
   Resolve comparison script assumes decoded code values are sRGB and does
   not verify ICC/transfer/range metadata. Its “agree” verdict is meaningful
   only for known matching exports. Require an explicit comparison manifest
   and preserve source/output hashes, settings and tolerances.

6. **Performance cleanup after correctness gates.**
   Cube parsing is cached and this audit bounds retained historical cubes,
   but lattices are still cloned/uploaded repeatedly. Cache immutable GPU
   resources by digest/device and benchmark the full app path. Avoid making
   preview quality worse to improve a GPU-only timing number.

## Verification

Three regressions were first observed failing on the pre-fix code: Centre
cache invalidation, CPU tetrahedral lookup and malformed-cube refusal.
Additional coverage checks source/LUT replacement, wide-gamut preservation,
selection-stage invariance, mask cache identity, configuration round trips,
fingerprints and masked capture.

Final results: **69 targeted tests passed; three optional performance tests
were ignored.** This is not a claim that the entire repository test suite
was run.

- V3 library: 47 passed, three ignored.
- V3 integration/contracts: seven passed (the GPU contract contains many
  individual pixel, mask, LUT and precision assertions).
- Shared Basic-control parity: one passed.
- Shader layout/compilation: one passed.
- Export precision, profile and dithering: four passed.
- Baseline manifest/control wiring: one passed.
- Frontend v3 preset/curve utilities: seven passed; actual store undo/redo
  and loaded-edit normalization: one passed.
- Targeted Rust Clippy with warnings denied: passed. Repository Rust
  formatting check and diff whitespace check: passed.
- Frontend production build: passed, with existing large-bundle and dynamic
  import warnings.
- Whole-app TypeScript checking: **failed** in existing frontend modules.
  No frontend source was edited in this audit. A successful production
  bundle does not mean type checking is clean.

GPU tests initially could not see Metal inside the sandbox; they were rerun
with GPU access and passed, not skipped.

### Real-photo smoke run

Rendered `DSC08203.JPG`, `IMG_0716.jpeg` and `DSC08226.JPG` through the
application path with the installed transform pair at a 512-pixel long edge:
ten cases each, **30 successful renders**. Verified distinct output hashes
for neutral, positive/negative exposure and contrast; positive exposure
increased average lightness and negative exposure decreased it on all three.
This is a control-wiring/render-completion smoke test, not a new perceptual
quality assessment or a new Resolve comparison. Temporary images, manifest
and measurements are under `/tmp/rapidraw-v3-audit.ZERSTr/` and may be removed
by the operating system. Original photos were read only.

Reproduction commands (run from the repository root):

```sh
cargo test --manifest-path src-tauri/Cargo.toml --offline --lib color_engine
cargo test --manifest-path src-tauri/Cargo.toml --offline --lib export_processing::precision_tests
cargo test --manifest-path src-tauri/Cargo.toml --offline --test color_v3_contracts --test shader_layout --test basic_parity -- --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --offline --example color_v3_baseline
cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --test color_v3_contracts --example color_v3_baseline -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
node --experimental-strip-types --test tests/colorV3.test.mjs
./node_modules/.bin/esbuild tests/colorV3-history.test.ts --bundle --platform=node --format=cjs --outfile=/tmp/rapidraw-v3-audit-history.cjs
node --test /tmp/rapidraw-v3-audit-history.cjs
npm run build
npm run typecheck
```
