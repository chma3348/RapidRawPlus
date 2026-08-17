# Build plan: authentic restoration round (agreed 2026-08-18)

Scope contract for the current three-phase build. Each phase ends with its
acceptance list re-checked, tests run, and a dedicated commit + push.
Nothing gets added or dropped from this list silently.

## Phase A — Authentic Texture + grain match (enhance dialog) — SHIPPED a32b67df
- [x] New "Texture" slider on the enhance dialog: frequency-split blend —
      the model output keeps its structure (low/mid frequencies), while
      the ORIGINAL's fine-detail layer (micro-texture, pores, grain) is
      blended back on top at the chosen amount. Directly targets
      plastic-looking skin from restore/upscale models.
- [x] New "Match grain" slider: measures the original's fine noise level
      vs the result's and adds neutral grain to close the gap, scaled by
      the slider. Never adds grain if the result is already as grainy.
- [x] Both settings work on instant retries (cached raw output re-blend),
      fresh runs, chained steps, and engine (SeedVR2) results.
- [x] Both persist across dialog opens like strength does.
- [x] Texture 0 + grain 0 = byte-identical to old behavior.

## Phase B — Eyedropper on the Color Mixer — SHIPPED ffe506aa
- [x] Pipette button in the Color Mixer header (global adjustments panel).
- [x] While active: crosshair cursor, click the photo to sample; the
      mixer switches to the band nearest the sampled hue. Stays active
      for repeated picks until toggled off.
- [x] Clicking while active never pans/zooms the image.
- [x] Works with the WGPU renderer (samples the displayed preview).

## Phase C — Refine brush for the Clipped selection — SHIPPED c594421f
- [x] With a Clipped sub-mask active (Masks panel or AI Reconstruct),
      the brush is live: paint adds to the selection, eraser (or
      Alt-drag) removes from it. Brush size/feather/tool controls shown
      in the sub-mask config in BOTH panels.
- [x] Refine strokes apply AFTER thresholds + clean/grow/feather, so an
      erased region stays erased when sliders move.
- [x] Red overlay (and zebra pulse) updates live as you paint.
- [x] Painting never pans the image; strokes persist in the sidecar.

## Standing rules for every phase
- Verify: cargo tests (incl. GPU harness where measurable), clippy 0,
  TS baseline 120, vite build.
- Rebuild RapidRAW+, install, relaunch.
- Commit + push with the phase name; report acceptance-list status.

## Round: color-select fill quality — SHIPPED
- [x] Neutral-aware color key: near-neutral references (sat < ~15%)
      match by brightness+saturation instead of hue noise; saturated
      behavior unchanged (existing tests must still pass).
- [x] Per-blob filling: the engine fill splits the mask into connected
      components — small blobs heal via LaMa spot passes, large blobs get
      their own tight diffusion patch (cap 6 largest; excess demoted to
      LaMa). No more whole-image bounding boxes from scattered masks.
- [x] Verify: cargo tests incl. new component/key tests, TS 120, build,
      install, commit + push.

# Build plan: Resolve-class round (agreed 2026-08-13)

Order: fix first, then features. Each phase: tests, commit + push,
rebuild + install at phase end. Nothing added or dropped silently.

## Phase H — Fill patch harmonization (fix) — SHIPPED
- [x] Every fill patch (LaMa spot or diffusion blob) is tone-matched to
      the ring of original pixels around it (boundary means aligned;
      full strength for prompt-less fills, gentle for prompted ones).
- [x] Every fill patch is grain-matched to its surroundings (same
      estimator/synth approach as the enhance dialog).
- [x] Unit tests: harmonization pulls a bright smooth patch to ring tone
      and ring noise level.

## Phase I — Film look pack (+ Sat vs Sat) — SHIPPED (bright-pass prepass deferred: existing threshold architecture + upgrades deliver the look; escalate only if speculars feel dead)
- [ ] GPU bright-pass blur prepass (thresholded highlights, wide blur).
- [x] Halation: warm red-weighted spill around bright highlights.
- [x] Glow: neutral soft bloom with adjustable knee.
- [x] Film saturation: eases saturation out of deep shadows and near
      white (density-style response).
- [x] Sat vs Sat curve tab in the curves panel.
- [x] All v2-engine; shader layout probe updated for new bindings.

## Phase J — HDR zone wheels
- [ ] Backend: six zones from Oklab lightness (blacks/dark/shadow/light/
      highlight/specular) with smoothstep boundaries; per zone one color
      offset (wheel hue/sat -> rgb) + one exposure dial, applied in the
      v2 path after the LGG wheels. Struct fields appended (16-byte
      groups), layout probe green.
- [ ] UI: "HDR Wheels" panel in the Color section — 2x3 grid reusing
      ColorWheel with an exposure slider under each; collapsed default.
- [ ] GPU render test: specular-zone exposure must raise only the
      top-end of a ramp; blacks-zone color must not leak past midtones.
- [ ] Old sidecars unaffected (all zones neutral by default).

## Phase K — Skin smoothing (frequency separation)
- [ ] Mask-level "Smoothing" slider: subtracts the blotch band (between
      the existing 3.5px and 8px blurs) scaled by amount — uneven tone
      flattens while pores/fine texture pass through untouched. No new
      GPU passes needed.
- [ ] UI: slider in the mask adjustments Details group.
- [ ] GPU render test: mid-frequency blotch amplitude drops, fine
      checkerboard amplitude survives (>80%).

## Phase L — Clone/heal stamp
- [ ] New non-AI patch type "clone": alt-click sets the source anchor,
      brush strokes copy source-offset pixels with feathered edges;
      stored non-destructively alongside AI patches; deterministic (no
      model, no engine).
- [ ] Patch runs through the same harmonization blend as AI fills.
- [ ] UI: Clone creation type in the AI panel with brush size/feather;
      visible source/target indicators on canvas while active.
- [ ] Unit test: offset copy + feather math on a synthetic image.

## Phase M — Color Warper (dedicated round)
- [ ] Before code: its own detailed plan (mesh resolution, UI
      interactions, sidecar format) appended here and agreed.
- [ ] Core: Oklab chroma-plane mesh warp applied in-shader via a small
      LUT grid; wheel UI with draggable mesh points.

## Round: background AI rendering (agreed 2026-08-13) — SHIPPED
- [x] Closing/clicking outside the enhance dialog NEVER loses a running
      job: the run continues, its state survives, and reopening the
      dialog for that photo restores it (running or finished).
- [x] A floating indicator appears while a run continues with the dialog
      closed (spinner + progress line); it flips to "result ready" on
      completion and clicking it reopens the dialog.
- [x] Opening the enhance dialog while a run is in flight reopens the
      running job instead of resetting state.

## Round: Fujifilm film simulations (agreed 2026-08-14) — SHIPPED
- [x] F-Log2C input transform: linear sRGB -> F-Gamut C -> F-Log2 curve
      in-shader; film-sim LUTs replace the tone mapping; spec-pinned
      tests (flog2c.rs) against Fujifilm's published code values.
- [x] lutInputSpace adjustment ('display'/'flog2c'), auto-inferred from
      FLog2C_to_* filenames.
- [x] Managed LUT folder (~/Documents/RapidRAW Models/luts/<pack>/) with
      per-pack SOURCES.md provenance; official pack copied there; files
      never committed (Fujifilm copyright). Repo doc: LUTS.md.
- [x] "Film simulations" preset dropdown in Effects -> LUT.

## Round: Flat-field correction (rig calibration profiles) (agreed 2026-08-16) — SHIPPED

Goal: cancel the illumination falloff of a fixed camera rig (ground-glass
repro setup) by dividing each photo by a "master flat" reference frame,
per-pixel, in linear light. Replaces parametric devignette for rig shots.

### Backend — profile store
- [x] Managed folder `~/Documents/RapidRAW Models/flats/<profile>/`:
      `flat.png` (16-bit, linear-encoded, long edge capped ~2048 — the
      field is smooth, full res is wasted) + `profile.json` (name,
      created date, frame count, source filenames, notes, stats).
- [x] `create_flat_profile(name, source_paths)` command: decode each
      frame (RAW or JPEG via existing loaders), linearize, average all
      frames, gaussian blur (sigma ~3px) to kill reference noise,
      normalize per-channel by the 99.5th percentile (brightest spot =
      1.0), save. Returns stats: deepest-corner falloff in stops,
      clipped-pixel %, frame count.
- [x] Validation warnings surfaced to UI: hot spot clipped (>0.5% at
      255), falloff deeper than 6 stops ("add diffusion / expect noise"),
      single-frame reference ("average 5+ for a cleaner master").
- [x] `list_flat_profiles` / `delete_flat_profile` commands.
- [x] AppState cache slot: loaded flat as f32 image keyed by profile
      path (avoid re-decode per render).

### Backend — application in the pipeline
- [x] New adjustments: `flatFieldProfile` (string | null) and
      `flatFieldStrength` (0-100, default 100). Sidecar-persisted; ride
      along with copy/paste adjustments and presets like everything else.
- [x] Applied at the head of `apply_geometry_warp` (image_processing.rs)
      BEFORE distortion/rotation/crop — the flat was shot through the
      same optics, so the divide must happen in the unwarped sensor
      frame. Same precedent as lensfun vignetting correction (which
      already lives in the warp stage).
- [x] Math: bilinear-resize flat to image dims, convert photo to linear
      (skip for already-linear RAW path), then
      `out = photo / max(mix(1.0, flat, strength), 0.02)` per channel
      (floor = boost cap ~5.6 stops: rig edges outside the light cone
      must not explode into noise), re-encode.
- [x] Include profile path + strength in the geometry hash so
      `full_warped_cache` invalidates correctly; preview, export, masks,
      and AI all inherit the correction automatically (it is upstream of
      everything).
- [x] Orientation check: flat and photo must agree on rotation
      (EXIF-orient the flat the same way as photos at profile build).

### Frontend UI
- [x] Effects panel, new "Flat-field correction" section at the BOTTOM
      of the Effects tab (rig-specific tool, not everyday): profile
      dropdown (None / saved profiles / "New
      profile..."), Amount slider 0-100 (default 100), small caption
      line with profile stats ("4.8 stops - 8 frames").
- [x] "New profile..." opens a modal: name field, multi-file picker /
      drop zone for flat frames (1-20), Create button with progress;
      result view shows a normalized preview of the master flat, the
      stats, and any warnings before saving.
- [x] Day-to-day flow documented in-UI (caption/tooltip): save a preset
      with the profile + amount, or copy/paste adjustments, to apply the
      rig correction across a whole shoot.

### Tests
- [x] Unit: synthetic scene x synthetic falloff -> divide recovers the
      scene within epsilon (both sRGB-encoded and linear inputs).
- [x] Unit: strength 0 = byte-identical output; floor cap limits boost
      to ~5.6 stops on a near-black flat region.
- [x] Unit: profile build — averaging reduces noise vs single frame,
      normalization puts max at 1.0, stats (stops/clip%) correct on a
      synthetic flat.

### Verify + ship
- [x] cargo tests green, clippy 0, TS baseline, vite build.
- [x] Full `npm run tauri build`, install, relaunch.
- [x] Commit + push. Flat profile files stay local (user calibration
      data, like LUTs) — never committed.

## Round: De-pixelate prep + demoiré visibility (agreed 2026-08-16)
- [x] Enhance dialog "De-pixelate" switch (all tasks) with cell dropdown
      (Auto detects the grid; 4-48px manual): detects the mosaic grid
      (cell + phase from gradient-energy profiles, smallest-harmonic
      rule), collapses each block to its true mean, rebuilds the image
      as a Catmull-Rom surface through the cell centers — the model sees
      a natural soft image instead of a grid it would sharpen.
- [x] Wired through apply_enhancement + preview_enhancement (ONNX and
      engine paths, applied to the full image before preview crops so
      grid phase matches the full run); cache keys carry the setting.
- [x] Auto mode refuses images with no detectable grid (clear error)
      instead of mangling them.
- [x] Unit tests: synthetic mosaic (cell 8, phase 3/5) → exact cell and
      phase detected, collapse at least halves the error vs the mosaic;
      noise image → detection refuses.
- [x] Demoiré: ESDNet-L (screen patterns) was never gone — hidden by the
      dropdown clipping bug fixed in e71f4881; verify it lists under
      Enhance -> Restore -> Model.
- [x] Verify: cargo tests, TS 119, vite build, full build, install,
      commit + push.

## Round: tonal dials audit — light-working power + precision (2026-08-16)
Measured every light dial on a GPU patch ladder; found and fixed:
- [x] Highlights -100 INVERTED tone order (0.85 landed below 0.6; all
      separation collapsed to flat gray). Now a monotone rational
      shoulder with unit slope at the knee: strong recovery (255->175)
      with preserved separation (142/166/175) and pinned midtones;
      neighborhood detail-restore keeps texture inside recovered skies;
      clipped-chroma inherit retuned.
- [x] Highlights +100 hard-clipped 0.85 to 255; now clip-resistant
      approach (216->244).
- [x] Shadows +100 doubled a JPEG midtone (0.18: 46->101) and squeezed
      the shadow band 2.4:1 (mist-flat). Root cause: one zone fade for
      two domains — RAW scene-linear vs JPEG display-linear put the
      same perceptual zone at different Oklab L. Per-domain fade
      (0.46 JPEG / 0.62 RAW) + grounded lift (deep floor takes a
      reduced share): 0.05 lifts 3.3x, 0.18 +59%, 0.35 pinned, floor
      0.005->5 (grounded), texture amplitude retention 66%.
- [x] Whites was a second exposure slider (moved 0.05 at full strength);
      now a true white-point control pinned below midtones.
- [x] Blacks was nearly inert (+100: 1->5); now real floor authority
      (1->12) fading out by the midtones.
- [x] Regression: tonal_dials_zone_contract — monotonicity across a
      dense ramp for every dial extreme, zone isolation bounds, pull
      strength minimums, grounded floor, texture retention >= 55%.
      All 7 render_quality GPU tests green (legacy RAW contracts kept:
      3.2x checker lift, detail contrast retained, disc chroma
      reconstruction).
- [x] Verify: full cargo suite, build, install, commit + push.

## Round: fill mask confidence boost (2026-08-16) — SHIPPED baff5acb
- [x] Diagnosed from a real run (11.2M selected px, ZERO at full
      strength): graded Clipped/Color masks were consumed as literal
      opacity — fills composited at ~20% (invisible) and the 50%-
      threshold blob router only saw total-clip cores. Fill entry now
      remaps confidence to region membership (>=30% -> full strength,
      soft rim, <6% dust dropped — also eliminates thousands-of-specks
      LaMa marathons from noisy selections).

## Round: fill mask orientation fix (2026-08-16) — SHIPPED
- [x] Value-derived fill masks (clipped/color/luminance) were generated
      in full-image space then display-transformed (flips/rotation from
      their overlay parameters) — on flipped/rotated photos the fill
      edited the MIRROR position ("found the highlights, edited the
      foliage"; measured: mask centroid matched the double-flip of the
      true highlight centroid within 5px on a 7008px frame). Fills now
      neutralize display orientation for value-derived sub-masks;
      regression test pins mask-on-highlights for a flipped photo.

## Round: AI result blend + fill quality (2026-08-16) — SHIPPED
- [x] Result blend on EVERY AI patch: Feather + Opacity sliders in the
      container settings, applied at composite time (instant re-blend,
      no AI re-run). Feather scales with image size.
- [x] Fill quality for large regions: context pad capped at 520px (a
      1.5x pad around a 1500px blob shrank the fill area to ~300px on
      the engine canvas — the gray-mush cause) and the engine canvas
      grows to 1536 for blobs spanning >=900px.

## Round: fill consolidation guard (2026-08-16) — SHIPPED
- [x] When a fill mask fragments into >200 pieces, auto-close nearby
      specks into solid regions (solidify 60) and drop isolated dust
      <250px; log reports fragments -> regions. Measured motivation:
      6,304-speck run = ~1 hour of LaMa for invisible dust healing.
- [ ] Follow-up queued: Cancel button for fill runs (today's only
      cancel path is restarting the app).
