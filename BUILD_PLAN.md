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
