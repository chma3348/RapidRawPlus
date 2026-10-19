# Photos in the cloud: smart previews

18 October 2026. Photos kept by a cloud service on macOS (Nextcloud, iCloud
Drive, Dropbox…) can stay in the cloud while the library browses them and
the editor edits them, as Lightroom's Smart Previews allow.

## How it works

**Recognising them.** Those services keep a photo that has not been
downloaded as a "dataless" placeholder that downloads in full the moment
anything reads it. `cloud_files::is_cloud_only` reads only the file's flags
(`SF_DATALESS`), never starting a download. Listings mark such photos
(`cloud_only`). Thumbnails and camera data are never read from them.

**Smart previews** (`smart_preview.rs`). Beside each photo, as
`<photo>.dripreview`, so the service syncs it to every Mac:

- **What it holds:** the photo's *decoded* picture (what the colour engine
  develops, before highlight recovery, the RAW look or any edit), at most
  2560 px on its long side, plus its colour space, ICC profile and camera
  data.
- **Pixels:** DaVinci Wide Gamut, highlights rolled off (`x / (x + 0.75)`),
  the sRGB curve, 16-bit lossy JPEG XL at distance 0.3.
- **Size and accuracy:** 0.6–1 MB per photo (a very noisy ISO 12800 frame,
  1.9 MB). 95% of pixels are within about 0.1 stop; the rest is grain and
  noise a preview cannot hold.
- **Versioning:** it records the photo's size and modification time, and a
  changed photo's old preview is not used.

**Using them.**

- **The engine:** when a photo is only in the cloud, it develops the preview
  instead (`application::source_for`), scaled back up to the photo's own
  size so crops, masks and patches (all in the photo's pixels) land where
  they belong. With Shadows +100 or Exposure +1, whole-frame renders match
  the original's within 1–2 levels in 255; close up, the preview is softer.
- **The editor:** opens such a photo from its preview without downloading
  it, and shows a **Preview** pill by the file name.
- **The grid:** thumbnails are drawn from previews.
- **When the original is needed:** exports and the patch-making tools
  (fill, heal, sky) always read the photo itself, downloading it
  (`smart_preview::need_original`, per photo, so other renders at the time
  still use previews).

**Building them** (`cloud_builder.rs`). In the background, three photos at
a time (most of the time is the download, which a service often does
faster several at once):

1. Download it.
2. Make its preview (or, in the thumbnails mode, only its thumbnail).
3. Draw its grid thumbnail.
4. Give it back to the cloud (`NSFileManager evictUbiquitousItemAtURL`).

It never holds more than three photos' worth of space.

- What gets built: the folder opened in the library (`list_images_in_dir`
  with `buildCloud`), and photos on screen anywhere, which go first (their
  thumbnails asked for: `cloud_builder::request`). Home and the import
  dialogs list folders without queueing them; queueing every folder Home
  lists put 4,811 photos in line at once.
- Speed is the service's: each photo downloads whole (9 MB on average for
  Chris's) to make its 1 MB preview. His Nextcloud serves about 1.4 MB/s
  with three at a time, about 8 s a photo.
- A download that times out is tried again later (3 tries, pausing 15 s).
- Nothing downloads until the first-time card has been answered, and
  building can be paused.
- A cloud-folder photo decoded here anyway (opened while downloaded) also
  writes its preview.

**Giving back.** A cloud-only photo the app brought down (opened in the
thumbnails or "download when opened" modes, exported, or read by a tool) is
given back afterwards:

- the open photo, when another is opened;
- an exported one, when its export finishes.

Photos the user downloaded themselves are never touched.

## The choice (what a person sees)

**Small previews** (default, recommended) / **Thumbnails only** / **Download
when opened**:

- **The default:** Settings → General → "Photos stored in the cloud".
- **Per folder:** "Change…" above a folder's photos, which shows what each
  choice would take for that folder.
- **First time:** a window, when a folder (or Home) shows cloud-only photos
  and nothing has been chosen: why nothing shows, the three choices with
  what this folder would download and keep (`cloud_folder_summary`, from
  file sizes only), "Use this for all my Nextcloud folders", and "Not now"
  (asked again next launch; the line says "Not downloading anything yet ·
  Set up…"). Nothing downloads until it is answered.
- **Signals:** a cloud badge on cloud-only photos (the grid and Home's
  strips), one quiet line ("151 in Nextcloud · Making previews for this folder: 23 of
  151 · about 17 min left · Pause"): this folder's count (`has_preview` from
  the listing, plus `cloud-preview-made` events), the time from the pace of
  the last dozen photos (the first ones include starting up, and timing
  them read 71 hours), and the
  Preview pill in the editor.

## Network and cloud folders on Home

- Every folder on Home can be closed to its heading, with or without
  folders inside it.
- Folder lists are read off the main thread: a cloud or NAS folder slow to
  answer no longer freezes the window, and one that cannot be read says so,
  with Try again, instead of looking empty.
- Home's folder tree does not look inside its second level (each look makes
  the service list that folder from its server).
- Strips ask for their first 12 thumbnails, the rest as they drift into
  view. The thumbnail queue (500) tells the app which requests it drops, and
  photos still on screen ask again.
- Names a network drive stores with U+F028 / U+F029 (a trailing space or
  period) are shown with the space or period, not a box.

## Not yet

- **Zoom:** zooming to 100% shows the preview (softer); it does not yet
  fetch the original.
- **Space:** no "space used by previews" line or clearing in Settings →
  Data. Previews are synced, so clearing them would clear them on every Mac.
- **Folder context menu:** no "Cloud photos…" entry when right-clicking a
  folder; the choice is reached through Change… and Settings.
- **List view:** has no cloud badge.
- **Testing switch:** `DARKROOM_CLOUD_ONLY=<folder>` treats a folder as
  cloud-only, for testing without a cloud service.
