# V3-only application — September 28, 2026

## Decision implemented

Development edits do not need historical appearance compatibility. V3 is now
the only application renderer. This supersedes the unresolved compatibility
decision in the earlier efficiency audit.

V1/v2/unspecified development edits adopt v3 and Resolve rendering. Existing
v3 edits keep their chosen tone mapper and controls. Accepted older v3 pipeline
identities adopt the current engine/input-policy labels, preserving valid
captured transform assets and the RAW recovery setting. Obsolete switch-back
metadata and stale interpretation snapshots are removed during migration.

This is an algorithm/policy migration, not automatic replacement of already
pinned capture assets with newly installed files. Unknown future revisions,
malformed identities, and missing/damaged pinned assets still report errors;
they are not permission to substitute an arbitrary rendering.

Migration is in memory when opening/rendering. Normal subsequent saves persist
the current settings. This task did not batch-rewrite the photo library or
modify original photographs. Shared Basic sliders, geometry, masks, and v3
controls are retained. Older creative settings that v3 does not interpret are
not promised a visual equivalent; old development appearances may change.

## What changed

- Default settings, sidecar loading, editor history, preset application, and
  saving all adopt v3. Undo/redo cannot reactivate v1/v2.
- Removed the engine switch and old panel dispatch. The existing v3 controls,
  transform-lock action, and RAW highlight recovery option remain.
- Deleted the superseded application branches for editor previews, uncropped
  previews, original previews, thumbnails, preset previews, photo previews,
  export rendering, and export-size estimation.
- Geometry previews, community preset sheets, and LUT swatches now use v3 too.
  Their previous direct calls to the old GPU processor were easy to miss.
- Auto adjustment in the editor now always uses v3 analysis.
- Removed redundant legacy decoding/compositing before v3 exports and edited
  enhancement input. V3 loads/composites its own source once through its pipeline.
- Original comparison previews clear shared tonal controls, creative LUTs,
  masks, v3 controls, and patches while preserving geometry and interpretation.
- Thumbnail cache keys have a new renderer namespace, so old cached thumbnails
  cannot masquerade as newly rendered v3 results.
- Old GPU processor implementation, request types, helpers, state, and shader
  are excluded from normal builds. They compile only for unit tests or the
  explicit `legacy-reference` feature. Shared GPU context/display infrastructure
  remains; normal builds do not allocate the retired native preview surface.
- Removed obsolete helpers and old-only LUT/separate-mask export implementations.

## Your highlights/shadows work

`src-tauri/src/shaders/tone_v2.wgsl` is deliberately retained and unchanged:
despite its historical filename, it supplies shared tone functions to v3.
The old renderer is not needed in the app for those functions to operate.

The reference renderer reads a frozen copy at
`src-tauri/tests/fixtures/legacy-tone-v2.wgsl`, captured from the development
baseline. Future tuning of v3's shared tone file cannot silently move that old
comparison baseline. The old `shaders/shader.wgsl` is also reference-only now.

Historical comparison tests are retained behind `legacy-reference`, including
`basic_parity`, `render_quality`, and the old shader-layout test. The basic
comparison covers positive and negative highlights and shadows. Normal v3 GPU
contracts remain available without that feature. Git history also retains the
earlier implementation; no commit/history deletion was performed.

Run the historical comparison suite explicitly:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --offline --features legacy-reference --lib --test basic_parity --test color_v3_contracts --test shader_layout -- --test-threads=1
```

Do not enable `legacy-reference` for a shipping app build. It exists to measure
development progress, not to offer an alternative engine to the user.

## Export limits made explicit

V3 currently supports profile-tagged composited JPEG, PNG, and TIFF exports.
The format selector offers those formats only. Older unsupported export presets
normalize to PNG and disable separate-mask export. Separate mask-image and
CUBE LUT export are visibly unavailable and rejected by the backend rather than
silently invoking v2. Exporting a photo containing masks still works through v3.

These were already v3 limitations; removing v2 does not implement those formats.
They need dedicated v3 implementations if desired later.

## Verification

- Frontend production bundle builds; native development app builds without the
  reference feature.
- Backend unit tests, v3 GPU contracts, Basic parity, and shader layout pass
  in the reference-test configuration: 303 passed, 7 ignored, none failed.
- Frontend migration/control/preset tests and bundled editor history tests pass
  (15 tests total).
- Default-build JPEG/RAW checks compare entire floating-point output hashes
  against the pre-retirement v3 baseline; no tone equations were changed.
  All eight hashes matched exactly (neutral, tone, detail, and masked edits on
  both source types). Timings from this run are not used as a performance claim
  because compilation was also running on the machine.
- The browser-only pipeline fixture was visually inspected: no old-engine
  switch, and transform locking/RAW source options remain visible.
- Rust Clippy passes for the default library, v3 contracts and audit example.
- Full TypeScript checking remains blocked by existing repository errors
  (including image-crop declarations and unrelated component prop types).
  A production Vite build succeeds but is not a clean TypeScript check.

No calibration coefficients were changed. This work makes v3 the single target
for the upcoming Resolve calibration rather than maintaining parallel engines.

The Impeccable skill guided a minimal UI simplification using existing styles,
not a redesign. Optional future UI documentation can be captured with
`$impeccable init`; it is not needed to run this build.

## Run the current source

From the repository root, `npm start` launches the Tauri development app and its
frontend together. No installed application was overwritten by this task.
