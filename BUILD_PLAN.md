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

## Round: background AI rendering (agreed 2026-08-13)
- [ ] Closing/clicking outside the enhance dialog NEVER loses a running
      job: the run continues, its state survives, and reopening the
      dialog for that photo restores it (running or finished).
- [ ] A floating indicator appears while a run continues with the dialog
      closed (spinner + progress line); it flips to "result ready" on
      completion and clicking it reopens the dialog.
- [ ] Opening the enhance dialog while a run is in flight reopens the
      running job instead of resetting state.
