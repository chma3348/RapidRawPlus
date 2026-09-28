# V3 correctness and efficiency audit — 2026-09-28

## Scope and outcome

Audited the changes in `a73705db`, concentrating on captured input routing,
saved interpretation, preset transfer, file/transform caching, and RAW recovery.
The baseline is that commit's rendered output, not the older wide-gamut bypass.
No color equations, slider responses, precision, shader math, compression curve,
or highlight/shadow behavior were retuned.

The changes are a sound direction, but the initial preset click still bypassed
the metadata filter, several expensive copies/reads remained, and accepting an
old input-policy label did not actually reproduce that policy's old rendering.

Kept the new iPhone routing and patch-domain choice, sparse recovery storage,
Basic-reset pin preservation, snapshot-error fallback, startup handling of an
incomplete installed transform set, and removal of the unused preparation
command/state-machine plumbing. None required a pixel-changing reversal in
this pass. Stage labels/revision remain in reports; automated pipeline and
shader tests provide checks, not a runtime guarantee of semantic stage order.

## Implemented changes and rationale

| Change | Why / improvement |
| --- | --- |
| Share one bounded file-content hash cache across source loading, range-mask sampling, pinned asset validation, and routine save snapshots | Removes repeated full-file reads on warm editing paths. Previous range-mask sampling and pinned-asset checks still hashed complete files repeatedly. |
| Include Unix device, inode, and change time, alongside size and modification time, in file-version keys | Replacements preserving length and modification time no longer reuse stale source/LUT data. Reads also check that the file version did not change while reading. Non-Unix builds conservatively reread rather than trust an incomplete version key. |
| Explicit audits reread bytes; routine save snapshots reuse version-validated hashes | Calibration/reference intake verifies current bytes without making every ordinary save perform that full audit. Snapshot errors still do not prevent the sidecar write. |
| Retain shared input/output cubes inside render plans | The previous shared loader was followed by cloning both lattice vectors in each plan. Application renders now keep shared references through to GPU upload. |
| Borrow existing float pixels for read-only rendering and neighborhood analysis | `to_rgba32f()` was cloning images that were already RGBA float. Each removed clone saves 16 bytes per pixel of transient storage and the corresponding memory copy. |
| Apply the P3 input conversion in place | Removes another full float-frame allocation/copy while keeping operation order and math identical. |
| Reindex sparse RAW originals directly | The previous sparse recovery implementation still allocated full-frame orientation index maps. Now orientation work scales with retained pixels, without those full-frame maps. Tested against the existing image orientation routine for every orientation on a nonsquare image with indices above 65,535. |
| Filter photo-owned metadata on the initial preset click and both preview-generation paths | Filtering only the intensity-mixing path left the first click able to replace a photo's pipeline/recovery/interpretation. Preview rendering now retains the target photo's own metadata too. |
| Whitelist paste fields, including for old persisted copy settings | Old settings or clipboard data cannot reintroduce excluded photo metadata. Missing copy settings are handled safely. |
| Include installed P3 capture in the input-report example | The diagnostic now loads the same optional input capture as the application. |
| Report saved and effective input policies and their mismatch | Exposes the existing compatibility problem without silently changing current pixels. This is diagnostic reporting, not a migration or a UI warning. |

Preset-transfer fixes intentionally stop invalid foreign metadata from affecting
a target image; they are correctness fixes, not a claim that the buggy transfer
operation produces the same result. The render optimizations preserve output
for the same valid source, transforms, and adjustments.

## Before/after measurements

Same machine, optimized-dependency development builds, 1,600-pixel maximum
dimension, installed Resolve capture pair pinned into temporary audit storage.
Seven warm renders per case; times below are medians. A pre-change executable
was retained before modifying the renderer. Source photos and sidecars were not
modified. These are render timings, not end-to-end UI latency or release-build
performance promises; first decode, full-resolution export, disk save, and all
possible images were not benchmarked.

| Source / case | Before ms | After ms |
| --- | ---: | ---: |
| Tahiti `IMG_0716.jpeg` / neutral | 18.61 | 12.72 |
| JPEG / exposure + shadows + highlights | 27.13 | 18.19 |
| JPEG / clarity + texture | 19.04 | 11.52 |
| JPEG / color-range mask | 57.85 | 43.51 |
| `DSC03520.ARW` / neutral | 15.84 | 11.34 |
| RAW / exposure + shadows + highlights | 21.42 | 16.03 |
| RAW / clarity + texture | 15.17 | 10.44 |
| RAW / color-range mask | 61.83 | 36.41 |

All eight before/after BLAKE3 hashes of the entire encoded-sRGB **float** output
matched exactly. Every repeated render also matched its first render. This is
stronger than a visual comparison for these cases, not proof for every possible
combination. The synthetic P3 test additionally checks exact equality against
the previous copying implementation, including beyond-P3 values.

Reproduce with `src-tauri/examples/color_v3_audit.rs`: supply the photo path,
transform directory, and a disposable asset directory. It prints output hashes
and timings. Compare equivalent builds on the same machine; do not compare
development and release timings as though they measured the same change.

## Remaining findings — deliberately not hidden by this optimization

1. **Old pinned wide-gamut edits are not truly frozen.** The older
   `profiled-display-cube-or-wide-gamut-bypass-1` label is accepted but resolves
   using the current P3/compression behavior. This was already true at the audit
   baseline. Restoring old rendering would change those images relative to
   today's build, so this pass only exposes the mismatch. Recommended follow-up:
   implement legacy routing behind the old policy and offer explicit adoption
   of the new policy. Resolve that versioning decision before promising archival
   reproducibility. Newly calibrated sets should record the effective policy.
2. **Compression is a deliberate color transformation, not a lossless recovery
   of Resolve's wide-gamut interpretation.** It scales RGB chroma toward a
   luminance axis; it is not a guarantee of perceptual hue preservation or
   saturation ordering across arbitrary colors. The beyond-P3 fallback also
   uses the sRGB luminance coefficients after conversion to P3. Changing that
   would change pixels and needs a separately versioned color decision.
3. **P3 capture validation is limited.** Matching the gray axis detects transfer
   mismatches, but does not validate chromatic accuracy throughout the cube.
   Use saturated/colorful references in calibration, not only neutral ramps.
4. **Saving is more resilient, not infallible.** Interpretation errors no longer
   abort saving, but permission errors, a full disk, and interrupted writes can
   still fail. Sidecar writing is still a direct file write, not an atomic
   replacement. Atomic saving is a worthwhile separate reliability task across
   all sidecar-writing paths.
5. **GPU allocations/uploads remain.** Sharing CPU cubes does not eliminate
   their GPU uploads. Reusable GPU buffers and immutable GPU-LUT caching are
   possible next optimizations, but require device-lifetime/concurrent-render
   safeguards and another exact-output comparison. No precision reduction,
   preview-quality reduction, or approximation was introduced here.
6. File-version caching assumes ordinary filesystem change tracking. It is not
   an adversarial integrity mechanism or a transactional snapshot of files being
   edited concurrently. Explicit audits force byte reads.

## Validation

- Backend library, Basic parity, v3 contracts, and shader-layout tests pass:
  302 passed, 7 intentionally ignored, none failed.
- New coverage: preserved-timestamp replacement, all sparse orientations,
  in-place P3 parity, shared-lattice retention/fingerprint parity, and policy
  mismatch reporting with cached/fresh snapshot parity.
- New preset-transfer tests and existing v3 control tests pass (9 tests).
- Production frontend build passes; it still warns about the large bundle and
  ineffective dynamic import.
- Targeted Rust Clippy passes with warnings denied.
- Full frontend type checking does **not** pass: repository-wide errors remain
  in components, navigation, image processing, dependency declarations, etc.
  The final filtered check reports no errors in this audit's three changed
  frontend files. A successful Vite build is not a substitute for that check.

No calibration values or reference photographs were changed. Unrelated
film-processor documents were left untouched.
