# Interactive subject selection

AI Subject now keeps include/exclude clicks for the current selection. Click a
missing part to include it; Shift-click, Alt-click, or choose Exclude to remove
background. Dragging a box starts a new selection. Start over clears the prompts
and mask. AI Paint samples both brush and eraser strokes as model prompts.

Edge balance tightens or expands uncertain coverage without rerunning the model.
Existing Grow and Feather controls remain available. The same controls work in
the Masks and AI panels.

## Implementation

The reference project in `Photo Editer` informed accumulated prompts, model-native
low-resolution feedback, quality-based candidate selection, and image-guided soft
edges. No external source implementation or new model dependency was copied.

RapidRAW retains its installed SAM ViT-B model and its existing image encoding,
transforms, compositing, and saved bitmap masks. A first ambiguous click prefers
a larger mask only among similarly confident, prompt-consistent candidates.
Refinement retains the original prompts and the model's padded 256-square logits;
it no longer resizes an already-rendered rectangular bitmap into model feedback.
Each request starts with its complete prompt history, so correction clicks cannot
inherit an earlier mistake through a cached mask. A second decoder pass uses only
that request's native logits and is accepted only when it improves prompt agreement
or preserves the object (at least 85% overlap) without a material quality drop.
Image embeddings remain cached, so this does not repeat the expensive encoder.

Single-click ranking excludes SAM's dedicated multi-prompt token, following the
token distinction in the
[SAM ONNX implementation](https://github.com/facebookresearch/segment-anything/blob/main/segment_anything/utils/onnx.py).
Among the three ambiguous-click hypotheses it decides by **containment**, not by
a score tie: SAM's predicted IoU rewards the tight, confident part (a hood, a
vest), while the whole object sits one hypothesis over at a slightly lower
score. Measured on four different clicks on one diver, the whole-body
hypothesis was present every time (20–22% of the frame, IoU 0.67–0.76) and the
earlier 0.04 tie band picked a fragment in three of the four. The rule now takes
the largest prompt-consistent candidate within 0.25 IoU of the top scorer that
contains at least 85% of it, is at least 1.5× its size, covers less than 90% of
the image, and is *committed* (at least 20% of its support confidently inside;
a threshold-sensitive spill measures ~0, real objects 0.29–0.53). The old
stability *penalty* was removed from the score: it docked whole objects
0.07–0.10 and fragments 0.01, which alone pushed a coral head and a diver out of
the band. With a negative point or several positives the prompt already
disambiguates, and the original near-tie preference for the larger mask applies.

After the choice, selection **grows**. A lone click on a person routinely yields
a fragment, yet the rest of the body is usually present in the same logits at
low confidence. Each pass takes the model's low-confidence support (probability
> 0.2) connected to the positive clicks, turns its 5%-padded bounding box into a
box prompt, feeds the previous logits back, and decodes again — up to four
passes, stopping when successive masks agree to IoU > 0.99. On the first pass
every single-click hypothesis contributes to that support; once a box is in
play only the chosen mask does. A grown result is accepted only if it keeps at
least 90% of the previous selection, does not contradict a prompt it previously
satisfied, does not drop predicted IoU by more than 0.05, grows by at most 2.5×
per pass, and stays under 60% of the frame. A user-drawn box disables automatic
growth. This is the loop the reference implementation runs, without its peak
point (the box alone converged on every test case).

Final masks crop model padding, use RGB-guided boundary refinement, suppress
small unseeded islands, and retain soft boundaries while removing probability
tails from unanimous interior/background neighborhoods. These are segmentation masks, not
dedicated hair/transparency alpha mattes. Difficult overlaps, translucent objects,
and fine hair may still need correction clicks or manual brush work.

Saved mattes are full resolution; when one is mapped onto a smaller render (the
editor preview is typically a third the size) it is now sampled bilinearly.
Point-sampling picked one source pixel per output pixel and turned every smooth
boundary into a jagged one — measured contour jaggedness fell 31% — which read as
a bad selection when the mask itself was fine.

Boundary refinement fits local RGB/alpha relationships over overlapping windows
at up to 1024 pixels, then evaluates smoothly interpolated coefficients against
the full-resolution photo. It replaces isolated coarse-grid color samples that
could introduce patterned edges on texture. Its influence is limited to uncertain
coverage; solid foreground/background remains unchanged. This is a postprocessing
improvement, not a higher-resolution segmentation model.

Late selection results are ignored after reset, deletion, changed geometry,
navigation, or a newer request. Existing saved masks remain compatible; masks
without prompt history infer their initial click/box when refined.

## Validation

- `node --test tests/subjectSelection.test.mjs`
- In `src-tauri`: `cargo test --lib subject_selection::tests --no-default-features`
- Optional installed-model regression: set `SUBJECT_MODELS` to the installed
  models directory and `ORT_DYLIB_PATH` to the bundled runtime, then run
  `cargo test --test subject_selection_model -- --ignored --nocapture`.
  `SUBJECT_PHOTO`, `SUBJECT_CLICK=x,y`, `SUBJECT_EXCLUDE=x,y`, and `SUBJECT_OUTPUT` optionally select a
  real-photo fixture and save masks/overlays. Without a photo it uses a synthetic
  portrait-aspect object and checks foreground/background coverage.
- `npm run build`

Model validation covers real-photo person and bus selections, negative prompting,
and repeat selection after undoing prompt history. It is a smoke test, not a
representative quality benchmark or a guarantee of perfect one-click selection.

Measured on a set of underwater photographs (hard: thin hoses, fins, low
contrast, busy reef), single first clicks, share of frame selected:

| case | before | after |
|---|---|---|
| diver, click on torso | 0.28% (hood) | 21.6% (whole diver) |
| diver, click on head | 1.16% | 21.6% |
| diver, click on tank | 3.00% | 21.6% |
| diver, click on legs | 21.6% | 21.6% |
| pairwise IoU across those four clicks | 0.034 | **0.997** |
| diver against reef, click on shoulder | 3.7% (vest) | 8.7% (body + tank + legs) |
| coral head, centre click | 0.26% (sliver) | 29.9% (whole head) |

Known limits, all model capacity (SAM ViT-B): an object made of visually
dissimilar parts — a black wetsuit's legs and yellow fins next to a bright tank
— may still come back partial from one click (a second click on the missing
part completes it), and two touching subjects are proposed as one (an exclude
click on the other separates them). Diagnostic probes live in
`tests/probe_sam_candidates.rs` (dump every hypothesis for a click),
`tests/probe_growth.rs` (the growth loop pass by pass), and
`tests/probe_prompting.rs` (box prompts and U-2-Net saliency boxes); all are
`#[ignore]` and driven by the same `SUBJECT_*` environment variables.

The local installed-runtime harness reports passing assertions and exit code 0,
but also prints a mutex error during ONNX runtime process teardown. Full-project
TypeScript checking remains blocked by existing errors outside this feature.
