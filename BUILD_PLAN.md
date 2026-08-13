# Build plan: masking & recovery round (agreed 2026-08-16)

Scope contract for the current four-phase build. Each phase ends with its
acceptance list re-checked, tests run, and a dedicated commit + push.
Nothing gets added or dropped from this list silently.

## Phase 1 — AI Paint interaction fix (bug) — SHIPPED 47ec62b7
- [x] Clicking/dragging with an AI Paint sub-mask active PAINTS strokes;
      it never pans the image. (root cause: brush tool never activated +
      pan not disabled for the type; fixed at all sites — user to confirm)
- [x] Works in both the Masks panel and the AI panel flows.
- [x] Releasing the stroke triggers SAM refinement (existing behavior).

## Phase 2 — Whole-mask controls — SHIPPED
- [x] Mask CONTAINER properties gain Opacity, Grow, Feather controls,
      alongside the existing per-component ones. (Opacity already existed
      at container level; Grow/Feather added.)
- [x] Backend applies container grow/feather AFTER component composition
      (add/subtract/intersect), so it hones the combined shape.
- [x] Old sidecars unaffected (serde defaults = neutral).

## Phase 3 — Lightroom-class color select (both interactions) — SHIPPED
- [x] Eyedropper: click the image to sample the target color.
- [x] Shift-click adds up to 5 samples, shown as removable chips.
- [x] Swatch row: 8 preset hue chips (same bands as HSL mixer) as
      one-click generic selections.
- [x] Matching runs in hue/sat-weighted space (luminance de-weighted) so
      one object is selectable across its shading.
- [x] Works with existing tolerance / clean / grow / feather and the B/W
      matte view.

## Phase 4 — Clipped-pixel selection + AI Reconstruct — SHIPPED
- [x] New parametric mask type "Clipped": selects pixels above a white
      threshold and/or below a black threshold (adjustable), with
      clean/grow/feather. Usable as a normal adjustments mask (Others list).
- [x] AI panel action "Reconstruct": builds the clipped mask
      automatically; the fill runs via the existing Generate button so the
      user can preview the matte and tune thresholds first (declared
      deviation from one-click auto-run — deliberate, for control before
      an expensive fill).
- [x] Non-destructive patch; correctly exposed pixels untouched (soft
      3% threshold edges + feather).
- [x] Prompt box available (existing generative section); white/black
      thresholds independently adjustable for highlights vs shadows.

## Standing rules for every phase
- Verify: cargo tests (incl. GPU harness where measurable), clippy 0,
  TS baseline 120, vite build.
- Rebuild RapidRAW+, install, relaunch.
- Commit + push with the phase name; report acceptance-list status.
