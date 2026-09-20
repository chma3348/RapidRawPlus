# Applying edits to video

Asked: could the app's adjustments apply to video too?

## The short answer

Three routes, in rising order of cost.

**1. Export a LUT and grade in Resolve — available today, zero work.**
The app already exports `.cube` LUTs. Grab a frame from the clip, edit it
until it looks right, export the grade as a LUT, and apply that LUT to the
footage in Resolve. This is how the grade travels in professional work, it
handles the whole clip at full quality, and it uses the tool you already
use. The limits: a LUT carries colour, not anything spatial, so crop,
lens correction, masks, healing and sharpening do not come along.

**2. Live preview with adjustments — moderate.**
The renderer is a compute shader over an RGBA texture, so feeding it
decoded frames is not the hard part. The hard part is decode: the webview
plays video but will not hand us frames at speed, so the app would need a
real decoder (AVFoundation on macOS, ffmpeg elsewhere) to pull frames,
push them to the GPU, run the pipeline and present. That is a genuine
feature, and it only gets you a preview.

**3. Exporting graded video — large.**
Everything in 2, plus an encoder, plus audio passthrough, plus timeline
and bitrate handling. At that point the app is a video grader, which is a
different product with different performance rules.

## Recommendation

Do 1. It costs nothing, it is the standard workflow, and it plays to
Resolve's strengths. Revisit 2 only if you find yourself wanting to judge
a grade on moving footage rather than on a still.

One caveat on LUTs: they assume the video and the frame you graded share a
colour space. A clip from a phone is already display-referred, so a LUT
built from a still of the same clip transfers cleanly. Log footage would
need its own handling.
