# One-click scene masks: Subject, Sky, Foreground

Adding a **Subject**, **Sky** or **Foreground** mask in the Masks panel now
selects that region immediately, with no click or box required. Subject
masks keep their click/box refinement: the automatic result stores the
prompts that reproduce it, so a later click adds to it instead of starting
over, and the subject panel has a "Select subject automatically" button to
re-run it.

Implementation: `src-tauri/src/scene_masks.rs`; commands
`generate_ai_sky_mask`, `generate_ai_foreground_mask` (rewritten) and
`generate_ai_auto_subject_mask` (new) in `src-tauri/src/ai_commands.rs`;
frontend in `src/hooks/useAiMasking.ts`, `MasksPanel.tsx`,
`SubjectSelectionControls.tsx`.

## What each mask means

| mask | semantic source | edge source |
|---|---|---|
| Sky | sky-segmentation U²-Net probabilities (320 px, photo + mirror averaged) | guided filter against the full-resolution photo |
| Foreground | everything nearer the camera than the subject: Depth Anything v2 relative depth (518 px, photo + mirror averaged), cut just in front of the subject's own depth | guided filter; the subject is always excluded |
| Subject | U²-Net saliency proposes components; SAM ViT-B is prompted with a padded box per component | SAM matte (falls back to guided saliency if SAM disagrees with the proposal by IoU < 0.4) |

Foreground follows the photographer's layering, foreground → subject →
background: it is whatever lies between the camera and the subject. It is
therefore measured against a subject. The panel passes the Subject mask you
already made (same mask container first, then any), provided it was made on
the current crop/rotation; otherwise the automatic subject is used. The cut
sits at the larger of the subject's 75th-percentile disparity and its median
plus 0.03, so the ground the subject stands on and things beside it stay out.
The subject itself is never part of the foreground.

## What changed versus the previous behaviour

1. **No more min–max stretching.** Both U²-Net models are sigmoid
   classifiers. The old code rescaled whatever came out to 0–255, so a photo
   with no sky (max probability 0.001) got a full-strength "sky" mask made
   of noise, and a skyline with no salient object got a random blob as
   "foreground". Probabilities are now used as-is and gated.
2. **Models see the photo upright.** Inference runs with the user's 90°
   orientation and flips applied, and the map is un-oriented afterwards.
   The sky model in particular has an "up" prior; sideways input roughly
   halves its recall (measured: 13.9% → 2.0% sky on one portrait shot).
3. **Edges come from the photo, not from a 320 px map.** A guided filter
   (He et al.) fits `alpha = a·RGB + b` in ≤1024 px neighbourhoods and
   renders it at full resolution; interiors at 0/1 are untouched and
   uncertain pixels may move by at most ±0.35. Edge alignment (share of
   mask boundary within 3 px of a strong luma edge) on foreground masks with
   objects in them rose from 0.45→0.80, 0.61→0.80, 0.83→0.95 on the reef
   set; sky edges against buildings were already ~1.0.
4. **Confidence gates instead of always answering.** Each mask can say
   "nothing found"; the panel shows a toast and leaves the mask empty.

### Gates

| mask | declines when |
|---|---|
| Sky | peak probability < 0.6; mirror-pass agreement (IoU of >50% regions) < 0.55; region centroid not in the upper half as displayed; coverage < 0.5% |
| Foreground | no subject (message asks for a Subject mask first); nothing nearer than the subject (< 0.5% of frame); photo and mirror disagree on what is in front (IoU < 0.5, checked once the region exceeds 2%) |
| Subject | saliency peak < 0.5; coverage < 0.5% or > 50% of the frame; mask runs along ≥3 frame borders (≥25% each); spans ≥90% of the bottom edge at ≥25% coverage (ground, not subject) |

## Measured behaviour

Probe: `SUBJECT_MODELS=<models dir> ORT_DYLIB_PATH=<libonnxruntime.dylib>
SCENE_DIR=<photos> SCENE_OUT=<dir> [SCENE_SUBJECT=1] cargo test --test
probe_scene_masks scene_eval -- --ignored --nocapture`. Per photo it reports
coverage, mirror-consistency IoU, Otsu separability, edge alignment of the
coarse vs the refined mask, SAM/saliency agreement and timings, and saves
1024 px masks for contact sheets. `raw_stats`, `orientation_sensitivity`,
`depth_maps` and `input_size_flexibility` are the diagnostics that drove the
design (the models accept only 320/518 px input; no larger-input option).

Sets: 24 New York skyline/bridge JPEGs, 16 underwater dive JPEGs (no sky, a
diver or coral as subject), 18 assorted (landscapes, a projector screen,
logos and test charts).

| result | |
|---|---|
| Sky, city set | 21/24 confident, all visually correct including sky between bridge cables; mirror IoU ≥ 0.95 on 20 of them. 2 sideways-stored photos and 1 spurious 1% blob declined |
| Sky, logos/charts/no-sky shots | all declined except one logo graphic with a blue field (11%, mirror IoU 0.74) |
| Sky, underwater | declined on 12/16; the 4 accepted are shots looking up at the water surface, which the model reads as sky |
| Foreground, underwater | sand in front of the coral, reef in front of divers, the coral head a diver swims behind; declined for the diver in open water (nothing in front). Never overlaps the subject on any photo |
| Foreground, city | water and near people in front of the skyline or bridge |
| Subject, underwater | 9/16 selected: divers and coral, full body each time; the diver in IMG_0716 matches the four-click consensus from the click-based tool exactly (21.6%, IoU 0.98 with the proposal). 5 declined as no clear subject, 1 open-water region declined as ground |
| Subject, assorted | projector screen, an inverted person and a river selected; whole-valley and chart "subjects" declined |
| timing (6000 px JPEG, M-series CPU) | sky 0.7–1.0 s, foreground ~1 s, subject 1.6–1.9 s including SAM embeddings |

## Known limits

- The sky model reads the underwater surface seen from below, and large
  featureless blue areas, as sky. Position and consistency gates remove the
  unstable cases but not the confident ones.
- Foreground is only as right as the subject it is measured against. When
  the automatic subject is wrong (a camera housing over a small diver),
  make the Subject mask by clicking first, then add Foreground.
- Anything nearer than the subject counts, including things a photographer
  might not call foreground (a stranger's head at the frame edge, the walls
  around a projected screen). The cut across a featureless water plane is a
  depth line, not an object edge.
- Subject follows saliency: with no clear object it declines, and when two
  objects compete it takes the largest salient component (the camera
  housing over a small diver in one shot). Click the intended subject to
  override; the click tool's containment and growth rules apply from there.
- A graphic with a coloured field can still be read as sky; the gates are
  tuned for photographs.
