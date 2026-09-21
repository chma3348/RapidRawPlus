# High-precision image export

PNG and TIFF exports now use a dedicated 16-bit output path. Source pixels are
uploaded as 32-bit floats, processed by the existing color shader, and quantized
directly to 16-bit unsigned RGBA values. There is no intermediate 8-bit rendered
image or 8-bit dither. PNG retains RGBA16; TIFF retains RGB16 as before, now with
real additional precision. `.tif` and `.tiff` use the same encoder.

Resizing retains the image's 16-bit type. Watermarks blend through a typed 16-bit
buffer instead of DynamicImage's generic 8-bit pixel interface. JPEG, WebP, AVIF,
and JPEG XL still use the app's existing 8-bit encoders, converting at encoding.
Export size estimates and individual masked-image exports use the same precision
path. Exported LUT precision remains limited by the existing LUT conversion code.

The native preview is unchanged: half-float input, floating-point calculations,
and an 8-bit display texture. This change improves export precision; it does not
enable HDR/10-bit monitor output or replace the color engine. Blur/effect scratch
textures remain half-float, masks remain 8-bit, and source information already
lost during decoding or earlier processing cannot be recovered.

GPU caches distinguish preview and export formats, avoiding reuse of half-float
source uploads for export. High-precision export cannot be bound to the preview
surface. Export scratch textures are limited to a tile plus filter padding, and
unused display buffers are minimal. Full-precision source uploads and 16-bit CPU
images increase memory requirements; switching modes may rebuild the processor.
Images exceeding the GPU's supported dimensions return an export error instead
of silently saving an unprocessed source image.

## Verification

- `cargo test --lib precision_tests -- --nocapture` from `src-tauri` validates
  both shader and codecs, checks resize/watermark preservation, and executes a
  real-GPU gradient spanning a tile boundary with non-aligned rows and a cropped
  ROI. It checks exact preview restoration after export and more than 1,500
  distinct output levels. GPU absence is a failure, not a skipped release check.
- `cargo test --test render_quality --test shader_layout` checks existing color
  behavior and the ordinary preview shader's bindings.

The rendering suite currently has two pre-existing failures: blown-highlight
color recovery and highlights affecting midtones beyond the test tolerance.
Both were reproduced with identical measurements in an isolated copy using the
previous GPU/export implementation. They are not precision regressions. The
layout fixture was also updated to include the existing shadow-correction LUT
binding that it previously omitted.

No slider formulas, tone mapping, working color space, or saved edit versions
are changed. This addresses the export precision bottleneck identified in
`color-engine-roadmap.md`; the remaining color-engine work is still separate.
