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
| Sky | the "sky" class of a scene-labelling model (UperNet Swin-L on ADE20K's 150 classes): one pass over the whole photo at 768 px, plus overlapping tiles at 1152 px where that pass is undecided | colour matte in the uncertain band and reaching into fine structure, guided filter where sky and land colours match |
| Foreground | everything nearer the camera than the subject: Depth Anything v2 relative depth (518 px, photo + mirror averaged), cut just in front of the subject's own depth | guided filter; the subject is always excluded |
| Subject | BiRefNet lite, a dichotomous-segmentation model that cuts out the main object(s) directly | BiRefNet's own matte, placed at full resolution by the guided filter |

Foreground follows the photographer's layering, foreground → subject →
background: it is whatever lies between the camera and the subject. It is
therefore measured against a subject. The panel passes the Subject mask you
already made (same mask container first, then any), provided it was made on
the current crop/rotation; otherwise the automatic subject is used. The cut
sits at the larger of the subject's 75th-percentile disparity and its median
plus 0.03, so the ground the subject stands on and things beside it stay out.
The subject itself is never part of the foreground.

### Why these models

Measured on 170 of the user's photos (city, hazy landscape, underwater,
plus charts and logos as negatives). Both new models are MIT-licensed.

**Sky.** The old U-2-Net sky model is the weak link on exactly the photos
that matter: it found 13–18% of the frame on three hazy mountain shots
where the sky is 34–39%, and *nothing at all* on four overcast city
photos where the sky is 35–62%. It also hallucinated sky underwater
(16–98% of the frame on shots looking up at the surface). The scene model
gets all of those right and returns zero sky on all 24 dive photos.

A five-way degradation test (heavy haze, flat contrast, underexposed plus
noise, dusk tint, grey overcast) on 20 sky photos, scored as overlap with
each photo's clean-image sky:

| model | mean over the five degradations | worst single case |
|---|---|---|
| U-2-Net (old) | 0.29–0.56 | 0.00 — sky lost entirely, on 13 of 20 photos |
| UperNet ConvNeXt-small | 0.89–0.97 | 0.03 — collapses when underexposed |
| **UperNet Swin-L (shipped)** | **0.98–0.99** | 0.74 |

(The reference came from Swin's own clean-photo output, which flatters it;
ConvNeXt's collapses are real failures, not an artifact of that choice.)

**Subject.** Saliency picked the wrong object often enough to matter: the
camera housing instead of the diver, one diver out of two, nothing at all
in 7 of 16 dive photos. BiRefNet gets both divers, the whole diver
including fins, and the coral head, and correctly finds no subject in the
hazy valleys where the full-size BiRefNet wrongly selects hillsides. The
lite model is the more conservative of the two and a quarter of the size.

## What changed versus the previous behaviour

0. **Better models.** Sky is a scene-labelling model rather than a
   single-purpose saliency network, and Subject is BiRefNet rather than
   U-2-Net saliency + SAM. SAM is still what your clicks use.
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

### Sky through fine structure

The model paints a tree crown or a bridge lattice as one solid object, so
the sky showing between the branches is labelled not-sky. Refinement is
therefore allowed to reach past the mask's edge into what the model
rejected, but only where all three hold:

- confident sky is within reach (colours are sampled at three scales, the
  widest about a sixth of the frame, so a gap deep inside a crown still
  sees the sky's colour, while a sunset gradient is still followed locally);
- the neighbourhood is finely structured — high local luma contrast, so a
  pixel sits among both branch-dark and sky-bright neighbours. A smooth
  pale wall beside the sky has low contrast and is left alone;
- the pixel's colour is within ~1.5 of the confident sky's own colour
  spread, fading out by 3.5.

Inside foliage the ordinary two-colour matte cannot work: the local
"non-sky" colour is contaminated by the sky showing through the gaps, so
the reach test asks instead whether the pixel *is* the sky's colour.

Measured: sky between branches and inside bridge lattice cells is
recovered (tree photo 34.8% → 35.3% of frame, bridge 39.6% → 41.5%), with
no change on plain skies, no bleed into pale buildings beside sky on the
hazy skyline, tower and street canyon photos, and no change to the 24
underwater negatives. One synthetic graphic (a logo of thin curved lines)
went from 1.2% to 6.8% false sky; the gates are tuned for photographs.

### Gates

| mask | declines when |
|---|---|
| Sky | peak probability < 0.6; mirror-pass agreement (IoU of >50% regions) < 0.55; region centroid not in the upper half as displayed; coverage < 0.5% |
| Foreground | no subject (message asks for a Subject mask first); nothing nearer than the subject (< 0.5% of frame); photo and mirror disagree on what is in front (IoU < 0.5, checked once the region exceeds 2%) |
| Subject | peak probability < 0.5; coverage < 0.5% or > 85% of the frame; mask runs along all four frame borders. (The old saliency-era rules declined at 50% coverage and three borders; measured against real portraits that was wrong — a close portrait legitimately fills 58–67% of the frame and touches three edges.) |

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
| timing (2400 px working copy, M5 Pro, CPU) | sky 3.4 s with no sky (one pass), ~10 s with sky (one pass + 2 tiles); subject ~2 s; foreground ~1 s plus the subject it measures against |

## Models and where they come from

| file | source | size | licence |
|---|---|---|---|
| `birefnet_lite.onnx` | onnx-community/BiRefNet_lite-ONNX (official ONNX build) | 224 MB | MIT |
| `upernet_swin_large.onnx` | exported from openmmlab/upernet-swin-large by `tools/export_upernet.py` | 978 MB | MIT |

BiRefNet downloads on first use like the app's other models. The scene
model has no published ONNX build, so it is exported locally by the script
(checked against PyTorch to 2e-5 before use) and placed in the models
folder. When either file is missing the masks fall back to the previous
models, with a warning in the log.

Apple's CoreML accelerator cannot compile either model (the pyramid
pooling and the deformable convolutions are unsupported), so both run on
the CPU.

## Known limits

- Sky through very fine structure is much better with the tiles and the
  reach, but the thinnest gaps (a single-pixel wire) stay partly soft.
- Refinement may move an uncertain pixel by at most ±0.35, so it corrects
  an edge that is slightly off but cannot rescue one the model placed far
  from the real boundary.
- Clicking on an automatic Subject mask hands the selection back to SAM,
  which is a different (and sometimes coarser) model than the BiRefNet
  cutout the click started from.
- Foreground is only as right as the subject it is measured against. When
  the automatic subject is wrong (a camera housing over a small diver),
  make the Subject mask by clicking first, then add Foreground.
- Anything nearer than the subject counts, including things a photographer
  might not call foreground (a stranger's head at the frame edge, the walls
  around a projected screen). The cut across a featureless water plane is a
  depth line, not an object edge.
- Subject follows the model's idea of the main object. On a skyline it may
  pick a boat rather than the buildings. Click the subject you want to
  override it.
