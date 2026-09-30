# Output colour space: Display P3 by default, sRGB on request

30 September 2026.

## What the user sees

The Export panel's File Settings have a **Colour space** choice: Display P3
(the default) or sRGB. It is one setting for two things that must always
agree: the editor preview and the exported file. Changing it re-renders the
preview and the before/after image in the new space.

## How it works

- `OutputSpace` (`color_engine/config.rs`) is the app setting
  `outputColorSpace` ("displayP3" or "srgb"; unset means Display P3), held in
  `AppState.output_space`. It is sRGB until the app reads its settings, so
  tools, tests and the calibration harness keep rendering sRGB.
- `application::render_for_output` renders in that space; the editor preview,
  the before/after image, `preview_bytes` and exports use it. `render_file`
  and everything else (library thumbnails, preset tiles, the crop view, mask
  sampling, AI-enhancement inputs) stay sRGB.
- **Native P3.** When `output-transform-p3.cube` is installed (a Resolve
  lattice capture with Output Color Space = Display P3) and the picture ends
  in Resolve's transform, the final pass renders through the P3 capture, so
  colours beyond sRGB survive exactly as Resolve renders them. The capture is
  checked at startup to render greys as the sRGB capture does, and pinned in
  `v3Pipeline.output_transform_p3` like the other captures.
- **Stored as P3.** Without that capture, or through the previous engine's
  tone mappers, or into a creative LUT made for display sRGB, the sRGB
  rendering is converted to P3 at the end (`cube::srgb_encoded_to_p3`): the
  same colours in the wider container, nothing lost or changed.
- `RenderedFrame.space` records the space. PNG previews and JPEG/PNG/TIFF
  exports embed the matching ICC profile, and exported EXIF writes ColorSpace
  1 for sRGB or 0xFFFF ("uncalibrated", the embedded profile decides) for P3,
  instead of a camera's copied sRGB tag.

## Why P3 by default, and when to pick sRGB

P3 holds about a quarter more colour than sRGB (richer greens, reds and
cyans) with the same white and curve, and Apple devices, most recent phones,
Safari and Chrome display it correctly. Where a viewer ignores the profile
(many Windows programs, some upload services that strip it), a P3 file looks
duller than an sRGB export would; and an sRGB screen converting a P3 file on
the fly clips rather than using Resolve's gamut mapping. sRGB remains the
choice for anything going somewhere uncontrolled.
