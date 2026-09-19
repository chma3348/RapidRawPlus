# Sky Replace (in progress)

Replacing a sky is not pasting one. Two things have to happen or the result
reads as fake, and both are implemented in `src-tauri/src/sky_replace.rs`:

1. **Un-mix the old sky out of soft edges.** Along a branch, a wire or a
   strand of hair a pixel is part sky: `I = a·B + (1-a)·F`. Solving for `F`
   removes the old sky's colour from those edges. Without it every twig
   keeps a bright halo of the sky you just deleted, which is the classic
   give-away of a pasted sky.
2. **Relight the foreground.** A landscape lit by white overcast does not
   belong under an orange sunset. The foreground is shifted toward the new
   sky's colour, strongest near the horizon, and a little of the new sky is
   mixed in there as atmospheric haze.

Plus: the plate's bottom edge is placed on the photo's horizon (the median
of the lowest sky row per column, so one tall tree does not drag it down),
scaled to cover, mirror-tiled if too short rather than stretched, and given
grain matching the photo's own so it is not suspiciously clean.

Status: the compositor and its tests are in the repo and run end-to-end
through `src-tauri/tests/probe_sky_replace.rs`. Not yet wired to the UI.

## Cost

| step | time (2400 px working copy, M5 Pro) |
|---|---|
| sky mask | ~10 s (already cached per photo by the Sky mask) |
| composite | 0.4–0.7 s |

## The plate library

`~/Library/Application Support/io.github.CyberTimon.RapidRAW/skies/` holds
the plates and `library.json`, which records each plate's title, licence,
author and source page.

144 plates were fetched from Wikimedia Commons (freely licensed only:
CC0, CC BY, CC BY-SA, public domain) and verified with the app's own sky
model — a candidate is kept only when the top of the frame is ≥97% sky for
at least 45% of its height, and that block becomes the plate. They are
classified by look from colour statistics:

| look | plates |
|---|---|
| blue with clouds | 52 |
| sunset | 49 |
| overcast | 20 |
| twilight | 9 |
| stormy | 7 |
| mixed / clear blue | 7 |

Rebuild or extend with:

```
python tools/fetch_sky_plates.py <dir>       # keyword search
python tools/fetch_sky_categories.py <dir>   # Commons cloud/sky categories
python tools/build_sky_plates.py <dir>       # verify, cut and classify
```

Be polite to the API: the category pass hits rate limits (HTTP 429) if run
back to back.

## Known limits

- Water, windows and wet roads still reflect the *old* sky. Fixing
  reflections is a separate problem.
- A plate whose sun sits in a different place from the original lighting
  will not be caught automatically; pick a plate that matches, or turn the
  relight up so the foreground follows the new sky.
- The classifier is colour statistics, not semantics: a dark sunset lands
  in "twilight" and a mackerel sky in "overcast" as often as not.
- One fetched plate carries a photographer's watermark; watermark detection
  is not implemented.
