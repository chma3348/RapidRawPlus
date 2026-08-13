# Build plan: masking & recovery round (agreed 2026-08-16)

Scope contract for the current four-phase build. Each phase ends with its
acceptance list re-checked, tests run, and a dedicated commit + push.
Nothing gets added or dropped from this list silently.

## Phase 1 — AI Paint interaction fix (bug)
- [ ] Clicking/dragging with an AI Paint sub-mask active PAINTS strokes;
      it never pans the image.
- [ ] Works in both the Masks panel and the AI panel flows.
- [ ] Releasing the stroke triggers SAM refinement (existing behavior).

## Phase 2 — Whole-mask controls
- [ ] Mask CONTAINER properties gain Opacity, Grow, Feather controls,
      alongside the existing per-component ones.
- [ ] Backend applies container grow/feather AFTER component composition
      (add/subtract/intersect), so it hones the combined shape.
- [ ] Old sidecars unaffected (defaults = neutral).

## Phase 3 — Lightroom-class color select (both interactions)
- [ ] Eyedropper: click the image to sample the target color.
- [ ] Shift-click adds up to 5 samples, shown as removable chips.
- [ ] Swatch row: 8 preset hue chips (same bands as HSL mixer) as
      one-click generic selections.
- [ ] Matching runs in hue/sat-weighted space (luminance de-weighted) so
      one object is selectable across its shading.
- [ ] Works with existing tolerance / clean / grow / feather and the B/W
      matte view.

## Phase 4 — Clipped-pixel selection + AI Reconstruct
- [ ] New parametric mask type "Clipped": selects pixels above a white
      threshold and/or below a black threshold (adjustable), with
      clean/grow/feather. Usable as a normal adjustments mask.
- [ ] AI panel action "AI Reconstruct": builds the clipped mask
      automatically and runs the existing generative fill (Fooocus/Flux,
      LaMa prefill, feathered composite) only on those regions.
- [ ] Non-destructive patch; correctly exposed pixels untouched.
- [ ] Prompt box available; works on both blown highlights and crushed
      shadows.

## Standing rules for every phase
- Verify: cargo tests (incl. GPU harness where measurable), clippy 0,
  TS baseline 120, vite build.
- Rebuild RapidRAW+, install, relaunch.
- Commit + push with the phase name; report acceptance-list status.
