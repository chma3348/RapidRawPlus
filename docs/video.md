# Viewing video

Video files now appear in the library beside photographs and play in the
editor. They are viewed, not edited.

Supported containers: `.mov`, `.mp4`, `.m4v`. That list is exactly what the
webview decodes natively (H.264 and HEVC), which is the whole design:
playback, scrubbing and audio come from WebKit's own hardware-accelerated
decoders, so the app ships no video decoder of its own. `.mkv` and `.avi`
are deliberately absent — listing them would mean promising playback the
webview cannot deliver.

## How it fits together

| piece | where | how |
|---|---|---|
| Listing | `file_management::list_images_in_dir` | `is_supported_media_file` = photo or playable video |
| Grid thumbnail | `video::poster_frame` | `qlmanage` renders a frame; ~170 ms for a 4K clip |
| Duration / size / codec | `video::video_info` | Spotlight metadata via `mdls`, parsed in `parse_mdls` |
| Playback | `VideoViewer.tsx` | a `<video>` element on Tauri's asset protocol, which serves byte ranges so scrubbing works |
| Frame grab | `file_management::save_video_frame` | writes the displayed frame as a PNG beside the video |

Both system calls follow the existing precedent: HEIC decoding already
shells out to `sips` because macOS has the codec and bundling one would be
a poor trade. On other platforms `poster_frame` returns an error and
`video_info` returns empty fields, so the grid falls back to a placeholder
rather than breaking.

Selecting a video short-circuits the image pipeline entirely in
`useAppNavigation`: no `load_image`, no preview render, no histogram. Asking
the RAW pipeline to decode a MOV would only produce an error.

## Save frame

The bridge between viewing and editing. Pause where you want, press **Save
frame**, and the frame is written next to the video as
`<name>_frame_<time>.png`, numbered if one already exists. That still is an
ordinary photograph the editor can work on with every tool.

## Verified

- All 11 dive clips in the test folder produce correct poster frames,
  including the portrait one (2160×3840 → 576×1024), in 155–185 ms each.
- `parse_mdls` is unit-tested against real `mdls` output, including the
  `(null)` fields Spotlight returns for unindexed files.
- Extension classification is unit-tested, including that `.mkv`, `.avi`
  and `.webm` are *not* offered.

Not verified by me: the player itself, the grid badge and Save frame need
the app running, which I cannot drive. The pieces behind them are tested.

## Editing video

Not supported, and see `docs/video-editing-note.md` for why the LUT route
is the sane answer today.
