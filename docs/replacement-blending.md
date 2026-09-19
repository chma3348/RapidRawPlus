# Replacement blending — September 2026 checkpoint

## Try it

Open the freshly built `src-tauri/target/debug/bundle/macos/RapidRAW+.app` after quitting an older RapidRAW instance. Select the existing successful replacement in the AI panel. Below the generation button, **Blend this result** offers:

- **Transition width**: starts at 40, measured relative to a 1536-pixel context crop. For a recognized sky, extends outward through a conservative sky region. With no safe expansion, adjusts the narrow inward seam instead.
- **Match appearance**: starts at 35%. Bounded brightness and saturation correction using healthy sky pixels. Clipped white pixels and non-sky materials are excluded. If reliable samples are absent, tone correction is skipped.
- **Apply improved blend**: applies the controls to the saved generation, without another model call.
- **Restore original blend**: restores the exact previous color/mask/encoding. A portable copy is retained in the sidecar after the first improved blend, so restoring also works without the raw disk cache.

Fresh generations intentionally continue to use the proven original blend by default. The generation recipe and fast-repair/LaMa routing are unchanged.

## Safety and persistence

### Manual color and tone

The Masks tab includes a separate **Generated fills** listing for existing rendered AI patches. Select a fill to create or reopen its `Fill: …` adjustment mask, then use the normal Basic, Color, Curves, Details, and Effects controls. The fill is not regenerated. Subsequent selections reopen the same linked mask rather than duplicating it.

Linked masks retain manual adjustments while following the result mask after reblending, restoring, or changing variants. Their source component becomes an empty mask while its fill is hidden or deleted, preventing that component from adjusting the background instead. Ordinary masks and extra user-added components are not rewritten. These controls adjust the final composite within the fill footprint, not an isolated pre-composite AI layer; partially blended edge pixels therefore receive local adjustments too.

The complete pre-change source snapshot, working app, original photograph and sidecar, model input/mask/raw output, seed and prompt provenance are under `checkpoints/known-good-clouds-2026-09-06/`. The checkpoint is intentionally excluded from Git because it contains private photographs and large binaries. See its README for checksums and recovery instructions. Never extract its source archive over current work.

New context replacements save their full-resolution source crop, selection, orientation/geometry, model recipe, and original rendered payload in the existing per-run `ai-fill-debug` directory. No raw output is overwritten by blending. Reblending has no model calls and does not need a running engine. Disk-cache loss blocks further improved blending with an explicit error, not silent regeneration.

The older successful sky can be migrated using its existing raw output. Migration compares the reconstructed original source crop against the saved engine input; if it no longer matches, it refuses the blend and leaves the current result untouched. Accepted geometry and source are then frozen for subsequent reuse, including after rotation changes or app restarts.

Sky expansion is conservative, not a guarantee of perfect segmentation. Strong source edges, red markings and non-sky pixels block expansion; tiny isolated selected components do not seed large new islands. Subtractive refinements disable outward expansion. User-selected interiors retain their selection authority. The generation's cloud contrast is not normalized to a blank sky.

This stage does not add whole-sky replacement, another AI seam pass, or automatic texture/grain synthesis. The saved-example comparison shows softer transitions, but the screen texture and remaining brightness differences still warrant visual judgment. Keep original blending available while evaluating those separately.

## Verification

- Rust library regression suite, including bit-exact original blending, foreground/red-line barriers, tiny-island protection, negative-refinement confinement, and white-surroundings contrast preservation.
- Opt-in saved-photo fixture: `REPLACEMENT_FIXTURE` points to the known-good checkpoint; `REPLACEMENT_PREVIEWS` points to an output directory. Run `cargo test --offline --test replacement_blend_fixture -- --ignored --nocapture` from `src-tauri`.
- Fixture source/context mean absolute difference: 0.0367 on the 0–255 channel scale (migration refusal threshold: 3).
- Themed 300-pixel-wide controls checked in a browser, including Apply and Restore states and accessible slider names. This is a component check; it does not substitute for an end-to-end native app test.
- Frontend production build and macOS app bundle build. The repository-wide TypeScript check has existing unrelated errors; it is not a clean gate at this checkpoint.
