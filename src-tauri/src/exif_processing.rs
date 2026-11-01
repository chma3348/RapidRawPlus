use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};

use crate::formats::is_raw_file;
use crate::image_processing::ImageMetadata;
use chrono::{DateTime, NaiveDateTime, Utc};
use exif::{Exif, In, Value};
use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata;
use little_exif::rational::{iR64, uR64};
use rawler::decoders::RawMetadata;

pub fn truncate_large_exif(value: &str) -> String {
    if value.len() <= 500 {
        return value.to_string();
    }

    let mut start_idx = 200;
    while !value.is_char_boundary(start_idx) {
        start_idx -= 1;
    }

    let mut end_idx = value.len() - 200;
    while !value.is_char_boundary(end_idx) {
        end_idx += 1;
    }

    if start_idx < end_idx {
        let start_str = &value[..start_idx];
        let end_str = &value[end_idx..];
        return format!("{}...{}", start_str, end_str);
    }

    value.to_string()
}

/// Write `bytes` to `path` so a reader only ever sees the old file or the
/// whole new one: written beside it under a name of its own, then renamed
/// over it. Two writers at once each rename a complete file, so neither
/// leaves the other's half-written. `durable` also flushes it to the disk
/// first, for files written rarely (on macOS that flush is a full sync,
/// too slow for a batch of sidecars).
pub fn write_atomically(path: &Path, bytes: &[u8], durable: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// One lock per sidecar, held from reading it to writing it back, so two
/// changes at once (a rating while a paste is saving) both land instead of
/// the later write dropping the earlier one.
fn sidecar_mutex(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, LazyLock, Mutex};
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
        LazyLock::new(Default::default);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if locks.len() > 4096 {
        // Forget the locks nobody holds, so the table stays small.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
    locks.entry(path.to_path_buf()).or_default().clone()
}

thread_local! {
    /// Sidecars this thread holds the lock for, so a nested update of the
    /// same sidecar (EXIF migrated while it is being edited) does not wait
    /// on itself.
    static HELD: std::cell::RefCell<std::collections::HashSet<PathBuf>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Run `f` holding `path`'s sidecar lock.
pub fn with_sidecar_lock<R>(path: &Path, f: impl FnOnce() -> R) -> R {
    if HELD.with(|held| held.borrow().contains(path)) {
        return f();
    }
    let lock = sidecar_mutex(path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    HELD.with(|held| held.borrow_mut().insert(path.to_path_buf()));
    // Released even if `f` panics, so the thread can take it again later.
    struct Release<'a>(&'a Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            HELD.with(|held| held.borrow_mut().remove(self.0));
        }
    }
    let _release = Release(path);
    f()
}

/// Read a sidecar as it is on disk. Missing (or empty: nothing in it to
/// lose) is a new photo's defaults; anything else that cannot be read or
/// understood is an error, so it is never replaced by defaults. Returns
/// whether over-long EXIF values were trimmed on the way.
fn read_sidecar(sidecar_path: &Path) -> Result<(ImageMetadata, bool), String> {
    let content = match fs::read_to_string(sidecar_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ImageMetadata::default(), false));
        }
        Err(e) => return Err(format!("Could not read {}: {e}", sidecar_path.display())),
    };
    if content.trim().is_empty() {
        return Ok((ImageMetadata::default(), false));
    }
    let mut meta = serde_json::from_str::<ImageMetadata>(&content)
        .map_err(|e| format!("Could not understand {}: {e}", sidecar_path.display()))?;

    let mut healed = false;
    if let Some(ref mut exif_map) = meta.exif {
        for val in exif_map.values_mut() {
            if val.len() > 500 {
                *val = truncate_large_exif(val);
                healed = true;
            }
        }
    }
    Ok((meta, healed))
}

/// Adopt development-era settings in memory. Disk persistence remains tied
/// to an ordinary save; opening a library does not rewrite every sidecar.
fn migrate_in_memory(sidecar_path: &Path, meta: &mut ImageMetadata) {
    match crate::color_engine::migration::normalize(&meta.adjustments) {
        Ok(edits) => meta.adjustments = edits.into_owned(),
        Err(error) => log::warn!("Cannot migrate sidecar {}: {error}", sidecar_path.display()),
    }
}

/// A photo's sidecar, or why it could not be read. A missing sidecar is
/// the defaults.
pub fn try_load_sidecar(sidecar_path: &Path) -> Result<ImageMetadata, String> {
    let (mut meta, healed) = read_sidecar(sidecar_path)?;
    if healed {
        // Re-read under the lock so the trimmed copy never replaces a
        // change written since.
        let healed_on_disk = with_sidecar_lock(sidecar_path, || {
            let (fresh, still_bloated) = read_sidecar(sidecar_path)?;
            if still_bloated {
                write_sidecar(sidecar_path, &fresh)?;
            }
            Ok::<_, String>(())
        });
        match healed_on_disk {
            Ok(()) => log::info!(
                "Auto-healed bloated sidecar for: {}",
                sidecar_path.display()
            ),
            Err(e) => log::warn!("Could not heal {}: {e}", sidecar_path.display()),
        }
    }
    migrate_in_memory(sidecar_path, &mut meta);
    Ok(meta)
}

/// A photo's sidecar for reading only: one that cannot be read shows as
/// the defaults. Anything that writes the sidecar back uses
/// `modify_sidecar` instead, which refuses to replace an unreadable one.
pub fn load_sidecar(sidecar_path: &Path) -> ImageMetadata {
    try_load_sidecar(sidecar_path).unwrap_or_else(|e| {
        log::warn!("{e}");
        ImageMetadata::default()
    })
}

/// Save a sidecar whole (see `write_atomically`).
pub fn write_sidecar(sidecar_path: &Path, metadata: &ImageMetadata) -> Result<(), String> {
    let json = serde_json::to_string_pretty(metadata).map_err(|e| e.to_string())?;
    write_atomically(sidecar_path, json.as_bytes(), false)
        .map_err(|e| format!("Could not save {}: {e}", sidecar_path.display()))
}

/// Read, change and save a sidecar under its lock. `change` returns
/// whether it changed anything; nothing is written when it did not (so a
/// photo without a sidecar does not get one for nothing). A sidecar that
/// exists but cannot be read is left exactly as it is, and the error says
/// why. Returns the sidecar as it now is.
pub fn modify_sidecar(
    sidecar_path: &Path,
    change: impl FnOnce(&mut ImageMetadata) -> bool,
) -> Result<ImageMetadata, String> {
    with_sidecar_lock(sidecar_path, || {
        let (mut meta, healed) = read_sidecar(sidecar_path)?;
        let as_read = healed.then(|| meta.clone());
        migrate_in_memory(sidecar_path, &mut meta);
        if change(&mut meta) {
            write_sidecar(sidecar_path, &meta)?;
        } else if let Some(trimmed) = as_read {
            write_sidecar(sidecar_path, &trimmed)?;
        }
        Ok(meta)
    })
}

fn to_ur64(val: &exif::Rational) -> uR64 {
    uR64 {
        nominator: val.num,
        denominator: val.denom,
    }
}

fn to_ir64(val: &exif::SRational) -> iR64 {
    iR64 {
        nominator: val.num,
        denominator: val.denom,
    }
}

fn clean_creation_datetime_str(s: &str) -> &str {
    s.trim().trim_matches('"').trim_matches('\'').trim()
}

fn fmt_date_str(s: String) -> String {
    if let Some(dt) = parse_creation_datetime(&s) {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    clean_creation_datetime_str(&s).to_string()
}

fn normalize_creation_datetime(s: &str) -> Option<String> {
    let normalized = s.replace('T', " ");
    let (date, time) = normalized.split_once(' ')?;
    Some(format!("{} {}", date.replace(':', "-"), time))
}

fn parse_creation_datetime(s: &str) -> Option<NaiveDateTime> {
    let clean = clean_creation_datetime_str(s);
    if clean.is_empty() {
        return None;
    }

    let normalized = normalize_creation_datetime(clean);
    for candidate in std::iter::once(clean).chain(normalized.as_deref()) {
        for format in [
            "%Y:%m:%d %H:%M:%S",
            "%Y:%m:%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M:%S%.f",
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(candidate, format) {
                return Some(dt);
            }
        }
    }

    None
}

fn parse_creation_field(field: &exif::Field) -> Option<DateTime<Utc>> {
    parse_creation_datetime(&field.display_value().to_string())
        .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
}

fn parse_raw_creation_date(date_str: Option<&str>) -> Option<DateTime<Utc>> {
    parse_creation_datetime(date_str?).map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
}

pub fn read_exif(file_bytes: &[u8]) -> Option<Exif> {
    let exifreader = exif::Reader::new();
    exifreader
        .read_from_container(&mut Cursor::new(file_bytes))
        .ok()
}

pub fn read_raw_metadata(file_bytes: &[u8]) -> Option<RawMetadata> {
    let loader = rawler::RawLoader::new();
    let raw_source = rawler::rawsource::RawSource::new_from_slice(file_bytes);
    let decoder = loader.get_decoder(&raw_source).ok()?;
    decoder.raw_metadata(&raw_source, &Default::default()).ok()
}

/// ShutterSpeedValue is in APEX units, log2(1/t), not seconds: 8 is 1/256 s.
fn seconds_from_apex_shutter(value: f32) -> Option<f32> {
    let secs = (-value).exp2();
    (secs.is_finite() && secs > 0.0).then_some(secs)
}

/// The exposure time from a sidecar's EXIF: ExposureTime ("1/250 s") when
/// recorded, else ShutterSpeedValue ("7.97 EV", or a bare APEX number).
fn exposure_secs_from_sidecar(map: &HashMap<String, String>) -> Option<f32> {
    if let Some(val_str) = map.get("ExposureTime") {
        let cleaned = val_str.replace(" s", "");
        let cleaned = cleaned.trim();
        if let Some((num, den)) = cleaned.split_once('/') {
            if let (Ok(num), Ok(den)) = (num.trim().parse::<f32>(), den.trim().parse::<f32>())
                && den != 0.0
            {
                return Some(num / den);
            }
        } else if let Ok(val) = cleaned.parse::<f32>() {
            return Some(val);
        }
    }
    let apex = map.get("ShutterSpeedValue")?;
    let apex = apex
        .trim()
        .trim_end_matches("EV")
        .trim()
        .parse::<f32>()
        .ok()?;
    seconds_from_apex_shutter(apex)
}

pub fn read_exposure_time_secs(path: &str, file_bytes: &[u8]) -> Option<f32> {
    if let Some(map) = read_exif_sidecar(Path::new(path))
        && let Some(secs) = exposure_secs_from_sidecar(&map)
    {
        return Some(secs);
    }

    if is_raw_file(path)
        && let Some(meta) = read_raw_metadata(file_bytes)
    {
        if let Some(r) = meta.exif.exposure_time {
            return if r.d == 0 {
                None
            } else {
                Some(r.n as f32 / r.d as f32)
            };
        } else if let Some(r) = meta.exif.shutter_speed_value {
            return if r.d == 0 {
                None
            } else {
                seconds_from_apex_shutter(r.n as f32 / r.d as f32)
            };
        }
    }

    if let Some(exif) = read_exif(file_bytes) {
        if let Some(exposure) = exif.get_field(exif::Tag::ExposureTime, In::PRIMARY) {
            if let Value::Rational(ref r) = exposure.value {
                if r.is_empty() {
                    return None;
                }

                let val = r.first()?;

                return if val.denom == 0 {
                    None
                } else {
                    Some(val.num as f32 / val.denom as f32)
                };
            }
        } else if let Some(shutter_speed) =
            exif.get_field(exif::Tag::ShutterSpeedValue, In::PRIMARY)
        {
            // Signed by the standard; some writers use unsigned.
            let apex = match shutter_speed.value {
                Value::SRational(ref r) => r
                    .first()
                    .filter(|v| v.denom != 0)
                    .map(|v| v.num as f32 / v.denom as f32),
                Value::Rational(ref r) => r
                    .first()
                    .filter(|v| v.denom != 0)
                    .map(|v| v.num as f32 / v.denom as f32),
                _ => None,
            };
            return apex.and_then(seconds_from_apex_shutter);
        }
    }
    None
}

pub fn read_iso(path: &str, file_bytes: &[u8]) -> Option<u32> {
    if let Some(map) = read_exif_sidecar(Path::new(path))
        && let Some(val_str) = map
            .get("ISOSpeed")
            .or(map.get("PhotographicSensitivity"))
            .or(map.get("ISOSpeedRatings"))
        && let Ok(val) = val_str.parse::<u32>()
    {
        return Some(val);
    }

    if is_raw_file(path)
        && let Some(meta) = read_raw_metadata(file_bytes)
    {
        if let Some(r) = meta.exif.iso_speed {
            return Some(r);
        } else if let Some(r) = meta.exif.iso_speed_ratings {
            return Some(r as u32);
        }
    }

    if let Some(exif) = read_exif(file_bytes) {
        if let Some(r) = exif.get_field(exif::Tag::ISOSpeed, In::PRIMARY) {
            return r.value.get_uint(0);
        } else if let Some(r) = exif.get_field(exif::Tag::PhotographicSensitivity, In::PRIMARY) {
            return r.value.get_uint(0);
        }
    }
    None
}

pub fn extract_metadata(file_bytes: &[u8]) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();

    if let Some(exif_obj) = read_exif(file_bytes) {
        for field in exif_obj.fields() {
            match field.tag {
                exif::Tag::ExposureTime => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let r = &v[0];
                        if r.num == 1 && r.denom > 1 {
                            map.insert("ExposureTime".to_string(), format!("1/{} s", r.denom));
                        } else {
                            let val = r.num as f32 / r.denom as f32;
                            if val < 1.0 && val > 0.0 {
                                map.insert(
                                    "ExposureTime".to_string(),
                                    format!("1/{} s", (1.0 / val).round()),
                                );
                            } else {
                                map.insert("ExposureTime".to_string(), format!("{} s", val));
                            }
                        }
                    }
                }
                exif::Tag::ShutterSpeedValue => {
                    if let exif::Value::SRational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("ShutterSpeedValue".to_string(), val.to_string());
                    }
                }
                exif::Tag::FNumber => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("FNumber".to_string(), format!("f/{}", val));
                    }
                }
                exif::Tag::ApertureValue => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("ApertureValue".to_string(), format!("f/{}", val));
                    }
                }
                exif::Tag::FocalLength => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("FocalLength".to_string(), val.to_string());
                        map.insert("FocalLengthIn35mmFilm".to_string(), val.to_string());
                    }
                }
                exif::Tag::PhotographicSensitivity | exif::Tag::ISOSpeed => {
                    map.insert(
                        "PhotographicSensitivity".to_string(),
                        field.display_value().to_string(),
                    );
                    map.insert("ISOSpeed".to_string(), field.display_value().to_string());
                }
                exif::Tag::DateTimeOriginal => {
                    map.insert(
                        "DateTimeOriginal".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                exif::Tag::DateTime => {
                    map.insert(
                        "CreateDate".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                exif::Tag::DateTimeDigitized => {
                    map.insert(
                        "ModifyDate".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                _ => {
                    let val = field.display_value().with_unit(&exif_obj).to_string();
                    if !val.trim().is_empty() {
                        map.insert(field.tag.to_string(), val);
                    }
                }
            }
        }
    }

    if !map.is_empty() {
        return Some(map);
    }

    let metadata = read_raw_metadata(file_bytes)?;

    let exif = metadata.exif;

    let fmt_rat = |r: &rawler::formats::tiff::Rational| -> f32 {
        if r.d == 0 {
            0.0
        } else {
            r.n as f32 / r.d as f32
        }
    };

    let fmt_srat = |r: &rawler::formats::tiff::SRational| -> f32 {
        if r.d == 0 {
            0.0
        } else {
            r.n as f32 / r.d as f32
        }
    };

    let mut insert_if_present = |key: &str, val: String| {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            map.insert(key.to_string(), truncate_large_exif(trimmed));
        }
    };

    insert_if_present("Make", metadata.make);
    insert_if_present("Model", metadata.model);

    if let Some(v) = exif.artist {
        insert_if_present("Artist", v);
    }
    if let Some(v) = exif.copyright {
        insert_if_present("Copyright", v);
    }
    if let Some(v) = exif.owner_name {
        insert_if_present("OwnerName", v);
    }
    if let Some(v) = exif.serial_number {
        insert_if_present("SerialNumber", v);
    }
    if let Some(v) = exif.image_number {
        insert_if_present("ImageNumber", v.to_string());
    }
    if let Some(v) = exif.user_comment {
        insert_if_present("UserComment", v);
    }

    if let Some(v) = exif.date_time_original {
        insert_if_present("DateTimeOriginal", fmt_date_str(v));
    }
    if let Some(v) = exif.create_date {
        insert_if_present("CreateDate", fmt_date_str(v));
    }
    if let Some(v) = exif.modify_date {
        insert_if_present("ModifyDate", fmt_date_str(v));
    }

    if let Some(v) = exif.offset_time {
        insert_if_present("OffsetTime", v);
    }
    if let Some(v) = exif.offset_time_original {
        insert_if_present("OffsetTimeOriginal", v);
    }
    if let Some(v) = exif.offset_time_digitized {
        insert_if_present("OffsetTimeDigitized", v);
    }
    if let Some(v) = exif.sub_sec_time {
        insert_if_present("SubSecTime", v);
    }
    if let Some(v) = exif.sub_sec_time_original {
        insert_if_present("SubSecTimeOriginal", v);
    }
    if let Some(v) = exif.sub_sec_time_digitized {
        insert_if_present("SubSecTimeDigitized", v);
    }

    if let Some(v) = exif.lens_model {
        insert_if_present("LensModel", v);
    } else if let Some(lens_desc) = &metadata.lens {
        insert_if_present("LensModel", lens_desc.lens_model.clone());
    }

    if let Some(v) = exif.lens_make {
        insert_if_present("LensMake", v);
    } else if let Some(lens_desc) = &metadata.lens {
        insert_if_present("LensMake", lens_desc.lens_make.clone());
    }

    if let Some(v) = exif.lens_serial_number {
        insert_if_present("LensSerialNumber", v);
    }

    if let Some(v) = exif.orientation {
        insert_if_present("Orientation", v.to_string());
    }

    if let Some(r) = exif.fnumber {
        let val = fmt_rat(&r);
        insert_if_present("FNumber", format!("f/{}", val));
    }

    if let Some(r) = exif.aperture_value {
        let val = fmt_rat(&r);
        insert_if_present("ApertureValue", format!("f/{}", val));
    }

    if let Some(r) = exif.max_aperture_value {
        insert_if_present("MaxApertureValue", fmt_rat(&r).to_string());
    }

    if let Some(r) = exif.exposure_time {
        if r.n == 1 && r.d > 1 {
            insert_if_present("ExposureTime", format!("1/{} s", r.d));
        } else {
            let val = fmt_rat(&r);
            if val < 1.0 && val > 0.0 {
                insert_if_present("ExposureTime", format!("1/{} s", (1.0 / val).round()));
            } else {
                insert_if_present("ExposureTime", format!("{} s", val));
            }
        }
    }

    if let Some(r) = exif.shutter_speed_value {
        insert_if_present("ShutterSpeedValue", fmt_srat(&r).to_string());
    }

    if let Some(v) = exif.iso_speed {
        insert_if_present("PhotographicSensitivity", v.to_string());
        insert_if_present("ISOSpeed", v.to_string());
    } else if let Some(v) = exif.iso_speed_ratings {
        insert_if_present("PhotographicSensitivity", v.to_string());
        insert_if_present("ISOSpeedRatings", v.to_string());
    }

    if let Some(v) = exif.recommended_exposure_index {
        insert_if_present("RecommendedExposureIndex", v.to_string());
    }
    if let Some(v) = exif.sensitivity_type {
        insert_if_present("SensitivityType", v.to_string());
    }

    if let Some(r) = exif.focal_length {
        let val = fmt_rat(&r);
        insert_if_present("FocalLength", val.to_string());
        insert_if_present("FocalLengthIn35mmFilm", val.to_string());
    }

    if let Some(r) = exif.exposure_bias {
        insert_if_present("ExposureBiasValue", fmt_srat(&r).to_string());
    }

    if let Some(v) = exif.metering_mode {
        insert_if_present("MeteringMode", v.to_string());
    }
    if let Some(v) = exif.light_source {
        insert_if_present("LightSource", v.to_string());
    }
    if let Some(v) = exif.flash {
        insert_if_present("Flash", v.to_string());
    }
    if let Some(v) = exif.white_balance {
        insert_if_present("WhiteBalance", v.to_string());
    }
    if let Some(v) = exif.exposure_program {
        insert_if_present("ExposureProgram", v.to_string());
    }
    if let Some(v) = exif.exposure_mode {
        insert_if_present("ExposureMode", v.to_string());
    }
    if let Some(v) = exif.scene_capture_type {
        insert_if_present("SceneCaptureType", v.to_string());
    }
    if let Some(v) = exif.color_space {
        insert_if_present("ColorSpace", v.to_string());
    }
    if let Some(r) = exif.flash_energy {
        insert_if_present("FlashEnergy", fmt_rat(&r).to_string());
    }
    if let Some(r) = exif.brightness_value {
        insert_if_present("BrightnessValue", fmt_srat(&r).to_string());
    }

    if let Some(r) = exif.subject_distance {
        insert_if_present("SubjectDistance", fmt_rat(&r).to_string());
    }
    if let Some(v) = exif.subject_distance_range {
        insert_if_present("SubjectDistanceRange", v.to_string());
    }

    if let Some(gps) = exif.gps {
        let fmt_gps_coord = |coords: &[rawler::formats::tiff::Rational; 3]| -> String {
            format!(
                "{} deg {} min {} sec",
                fmt_rat(&coords[0]),
                fmt_rat(&coords[1]),
                fmt_rat(&coords[2])
            )
        };

        if let Some(lat) = gps.gps_latitude {
            insert_if_present("GPSLatitude", fmt_gps_coord(&lat));
        }
        if let Some(lat_ref) = gps.gps_latitude_ref {
            insert_if_present("GPSLatitudeRef", lat_ref);
        }
        if let Some(lon) = gps.gps_longitude {
            insert_if_present("GPSLongitude", fmt_gps_coord(&lon));
        }
        if let Some(lon_ref) = gps.gps_longitude_ref {
            insert_if_present("GPSLongitudeRef", lon_ref);
        }
        if let Some(alt) = gps.gps_altitude {
            insert_if_present("GPSAltitude", fmt_rat(&alt).to_string());
        }
        if let Some(alt_ref) = gps.gps_altitude_ref {
            insert_if_present("GPSAltitudeRef", alt_ref.to_string());
        }
        if let Some(v) = gps.gps_img_direction {
            insert_if_present("GPSImgDirection", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_img_direction_ref {
            insert_if_present("GPSImgDirectionRef", v);
        }
        if let Some(v) = gps.gps_speed {
            insert_if_present("GPSSpeed", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_speed_ref {
            insert_if_present("GPSSpeedRef", v);
        }
        if let Some(v) = gps.gps_status {
            insert_if_present("GPSStatus", v);
        }
        if let Some(v) = gps.gps_measure_mode {
            insert_if_present("GPSMeasureMode", v);
        }
        if let Some(v) = gps.gps_dop {
            insert_if_present("GPSDOP", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_map_datum {
            insert_if_present("GPSMapDatum", v);
        }
    }

    Some(map)
}

pub fn get_creation_date_from_path(path: &Path) -> DateTime<Utc> {
    if let Some(map) = read_exif_sidecar(path)
        && let Some(dt_str) = map.get("DateTimeOriginal").or(map.get("CreateDate"))
        && let Some(dt) = parse_creation_datetime(dt_str)
    {
        return DateTime::from_naive_utc_and_offset(dt, Utc);
    }

    if let Ok(file) = std::fs::File::open(path) {
        let mut bufreader = BufReader::new(&file);
        let exifreader = exif::Reader::new();

        if let Ok(exif_obj) = exifreader.read_from_container(&mut bufreader) {
            for tag in [exif::Tag::DateTimeOriginal, exif::Tag::DateTime] {
                if let Some(field) = exif_obj.get_field(tag, exif::In::PRIMARY)
                    && let Some(dt) = parse_creation_field(field)
                {
                    return dt;
                }
            }
        }
    }

    if is_raw_file(path.to_string_lossy().as_ref()) {
        let loader = rawler::RawLoader::new();
        if let Ok(raw_source) = rawler::rawsource::RawSource::new(path)
            && let Ok(decoder) = loader.get_decoder(&raw_source)
            && let Ok(metadata) = decoder.raw_metadata(&raw_source, &Default::default())
        {
            if let Some(dt) = parse_raw_creation_date(metadata.exif.date_time_original.as_deref()) {
                return dt;
            }
            if let Some(dt) = parse_raw_creation_date(metadata.exif.create_date.as_deref()) {
                return dt;
            }
        }
    }

    fs::metadata(path)
        .ok()
        .and_then(|m| m.created().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now)
}

#[cfg(target_os = "android")]
pub fn get_creation_date_from_bytes(path_hint: &str, file_bytes: &[u8]) -> DateTime<Utc> {
    if let Some(exif_obj) = read_exif(file_bytes) {
        for tag in [exif::Tag::DateTimeOriginal, exif::Tag::DateTime] {
            if let Some(field) = exif_obj.get_field(tag, exif::In::PRIMARY)
                && let Some(dt) = parse_creation_field(field)
            {
                return dt;
            }
        }
    }

    if is_raw_file(path_hint)
        && let Some(metadata) = read_raw_metadata(file_bytes)
    {
        if let Some(dt) = parse_raw_creation_date(metadata.exif.date_time_original.as_deref()) {
            return dt;
        }
        if let Some(dt) = parse_raw_creation_date(metadata.exif.create_date.as_deref()) {
            return dt;
        }
    }

    Utc::now()
}

/// The location in a sidecar's EXIF, which holds it as display text:
/// "37 deg 46 min 29.64 sec" (sometimes with its hemisphere after),
/// "N", "15.2 meters above sea level" or "15.2", "above sea level" or "0".
#[derive(Debug, Default, PartialEq)]
struct SidecarGps {
    latitude: Option<(Vec<uR64>, char)>,
    longitude: Option<(Vec<uR64>, char)>,
    /// Metres, and whether below sea level.
    altitude: Option<(uR64, bool)>,
}

impl SidecarGps {
    fn from_map(map: &HashMap<String, String>) -> Self {
        let coordinate = |key: &str, hemispheres: [char; 2]| {
            let text = map.get(key)?;
            let dms = parse_dms(text)?;
            // The reference field, else a hemisphere letter after the value.
            let hemisphere = map
                .get(&format!("{key}Ref"))
                .and_then(|r| r.trim().trim_matches('"').chars().next())
                .or_else(|| text.trim().chars().last())
                .map(|c| c.to_ascii_uppercase())
                .filter(|c| hemispheres.contains(c))?;
            Some((dms, hemisphere))
        };
        let altitude = map.get("GPSAltitude").and_then(|text| {
            let metres = first_number(text).filter(|m| *m >= 0.0)?;
            let reference = map.get("GPSAltitudeRef").map(|r| r.trim().to_lowercase());
            let below = match reference.as_deref() {
                Some(r) => r == "1" || r.contains("below"),
                None => text.to_lowercase().contains("below"),
            };
            Some((to_rational(metres)?, below))
        });
        SidecarGps {
            latitude: coordinate("GPSLatitude", ['N', 'S']),
            longitude: coordinate("GPSLongitude", ['E', 'W']),
            altitude,
        }
    }
}

fn first_number(text: &str) -> Option<f64> {
    text.split_whitespace().find_map(|t| t.parse::<f64>().ok())
}

fn to_rational(value: f64) -> Option<uR64> {
    let scaled = (value * 10_000.0).round();
    (value.is_finite() && (0.0..=u32::MAX as f64).contains(&scaled)).then_some(uR64 {
        nominator: scaled as u32,
        denominator: 10_000,
    })
}

/// "37 deg 46 min 29.64 sec" (or plain degrees, "37.7749") as the three
/// rationals EXIF stores.
fn parse_dms(text: &str) -> Option<Vec<uR64>> {
    let numbers: Vec<f64> = text
        .split_whitespace()
        .filter_map(|t| t.parse::<f64>().ok())
        .collect();
    if numbers.is_empty() || numbers.len() > 3 || numbers.iter().any(|n| *n < 0.0) {
        return None;
    }
    (0..3)
        .map(|i| to_rational(numbers.get(i).copied().unwrap_or(0.0)))
        .collect()
}

pub fn write_image_with_metadata(
    image_bytes: &mut Vec<u8>,
    original_path_str: &str,
    output_format: &str,
    keep_metadata: bool,
    strip_gps: bool,
    space: crate::color_engine::config::OutputSpace,
) -> Result<(), String> {
    // FIXME: temporary solution until I find a way to write metadata to TIFF
    if !keep_metadata || output_format.to_lowercase() == "tiff" {
        return Ok(());
    }

    let original_path = Path::new(original_path_str);
    if !original_path.exists() {
        return Ok(());
    }

    // Skip TIFF sources to avoid potential tag corruption issues
    let original_ext = original_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if original_ext == "tiff" || original_ext == "tif" {
        return Ok(());
    }

    let file_type = match output_format.to_lowercase().as_str() {
        "jpg" | "jpeg" => FileExtension::JPEG,
        "png" => FileExtension::PNG {
            as_zTXt_chunk: true,
        },
        "tiff" => FileExtension::TIFF,
        _ => return Ok(()),
    };

    let mut metadata = Metadata::new();
    let mut source_read_success = false;

    if let Some(map) = read_exif_sidecar(original_path) {
        source_read_success = true;

        let clean_s = |s: &String| s.replace('"', "").trim().to_string();

        let parse_ur64 = |s: &str| -> Option<uR64> {
            let cleaned_string = s
                .replace("f/", "")
                .replace(" s", "")
                .replace(" mm", "")
                .replace("\"", "");

            let val = cleaned_string.trim();

            if val.contains('/') {
                let parts: Vec<&str> = val.split('/').collect();
                if parts.len() == 2
                    && let (Ok(n), Ok(d)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                {
                    return Some(uR64 {
                        nominator: n,
                        denominator: d,
                    });
                }
            } else if let Ok(f) = val.parse::<f32>() {
                return Some(uR64 {
                    nominator: (f * 1000.0) as u32,
                    denominator: 1000,
                });
            }
            None
        };
        if let Some(val) = map.get("Make") {
            metadata.set_tag(ExifTag::Make(clean_s(val)));
        }
        if let Some(val) = map.get("Model") {
            metadata.set_tag(ExifTag::Model(clean_s(val)));
        }
        if let Some(val) = map.get("LensMake") {
            metadata.set_tag(ExifTag::LensMake(clean_s(val)));
        }
        if let Some(val) = map.get("LensModel") {
            metadata.set_tag(ExifTag::LensModel(clean_s(val)));
        }
        if let Some(val) = map.get("Artist") {
            metadata.set_tag(ExifTag::Artist(clean_s(val)));
        }
        if let Some(val) = map.get("Copyright") {
            metadata.set_tag(ExifTag::Copyright(clean_s(val)));
        }
        if let Some(val) = map.get("UserComment") {
            metadata.set_tag(ExifTag::UserComment(clean_s(val).into_bytes()));
        }
        if let Some(val) = map.get("ImageDescription") {
            metadata.set_tag(ExifTag::ImageDescription(clean_s(val)));
        }
        if let Some(val) = map.get("DateTimeOriginal") {
            metadata.set_tag(ExifTag::DateTimeOriginal(clean_s(val)));
        }
        if let Some(val) = map.get("CreateDate") {
            metadata.set_tag(ExifTag::CreateDate(clean_s(val)));
        }
        if let Some(val) = map.get("FNumber")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::FNumber(vec![ur]));
        }
        if let Some(val) = map.get("ExposureTime")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::ExposureTime(vec![ur]));
        }
        if let Some(val) = map.get("FocalLength")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::FocalLength(vec![ur]));
        }
        if let Some(val) = map.get("FocalLengthIn35mmFilm") {
            let cleaned = val.replace(" mm", "").replace("\"", "");
            let trimmed = cleaned.trim();
            if let Ok(f_val) = trimmed.parse::<f32>() {
                metadata.set_tag(ExifTag::FocalLengthIn35mmFormat(vec![f_val.round() as u16]));
            }
        }
        if let Some(val) = map.get("ISOSpeed").or(map.get("PhotographicSensitivity"))
            && let Ok(iso) = val.replace('"', "").trim().parse::<u16>()
        {
            metadata.set_tag(ExifTag::ISO(vec![iso]));
        }
        if !strip_gps {
            let gps = SidecarGps::from_map(&map);
            if let Some((dms, hemisphere)) = gps.latitude {
                metadata.set_tag(ExifTag::GPSLatitude(dms));
                metadata.set_tag(ExifTag::GPSLatitudeRef(hemisphere.to_string()));
            }
            if let Some((dms, hemisphere)) = gps.longitude {
                metadata.set_tag(ExifTag::GPSLongitude(dms));
                metadata.set_tag(ExifTag::GPSLongitudeRef(hemisphere.to_string()));
            }
            if let Some((altitude, below_sea_level)) = gps.altitude {
                metadata.set_tag(ExifTag::GPSAltitude(vec![altitude]));
                metadata.set_tag(ExifTag::GPSAltitudeRef(vec![below_sea_level as u8]));
            }
        }
    }

    if !source_read_success && let Ok(file) = std::fs::File::open(original_path) {
        let mut bufreader = std::io::BufReader::new(&file);
        let exifreader = exif::Reader::new();

        if let Ok(exif_obj) = exifreader.read_from_container(&mut bufreader) {
            source_read_success = true;

            let get_string_val = |field: &exif::Field| -> String {
                match &field.value {
                    exif::Value::Ascii(vec) => vec
                        .iter()
                        .map(|v| {
                            String::from_utf8_lossy(v)
                                .trim_matches(char::from(0))
                                .to_string()
                        })
                        .collect::<Vec<String>>()
                        .join(" "),
                    _ => field
                        .display_value()
                        .to_string()
                        .replace("\"", "")
                        .trim()
                        .to_string(),
                }
            };

            if let Some(f) = exif_obj.get_field(exif::Tag::Make, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Make(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Model, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Model(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::LensMake, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::LensMake(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::LensModel, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::LensModel(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Artist, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Artist(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Copyright, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Copyright(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::DateTimeOriginal(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::DateTime, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::CreateDate(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FNumber, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::FNumber(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::ExposureTime(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FocalLength, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::FocalLength(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::ExposureBiasValue, exif::In::PRIMARY) {
                match &f.value {
                    exif::Value::SRational(v) if !v.is_empty() => {
                        metadata.set_tag(ExifTag::ExposureCompensation(vec![to_ir64(&v[0])]));
                    }
                    exif::Value::Rational(v) if !v.is_empty() => {
                        metadata.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
                            nominator: v[0].num as i32,
                            denominator: v[0].denom as i32,
                        }]));
                    }
                    _ => {}
                }
            }
            if let Some(f) =
                exif_obj.get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            {
                if let Some(val) = f.value.get_uint(0) {
                    metadata.set_tag(ExifTag::ISO(vec![val as u16]));
                }
            } else if let Some(f) = exif_obj.get_field(exif::Tag::ISOSpeed, exif::In::PRIMARY)
                && let Some(val) = f.value.get_uint(0)
            {
                metadata.set_tag(ExifTag::ISO(vec![val as u16]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FocalLengthIn35mmFilm, exif::In::PRIMARY)
                && let Some(val) = f.value.get_uint(0)
            {
                metadata.set_tag(ExifTag::FocalLengthIn35mmFormat(vec![val as u16]));
            }
            if !strip_gps {
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
                    && let exif::Value::Rational(v) = &f.value
                    && v.len() >= 3
                {
                    metadata.set_tag(ExifTag::GPSLatitude(vec![
                        to_ur64(&v[0]),
                        to_ur64(&v[1]),
                        to_ur64(&v[2]),
                    ]));
                }
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY) {
                    metadata.set_tag(ExifTag::GPSLatitudeRef(get_string_val(f)));
                }
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY)
                    && let exif::Value::Rational(v) = &f.value
                    && v.len() >= 3
                {
                    metadata.set_tag(ExifTag::GPSLongitude(vec![
                        to_ur64(&v[0]),
                        to_ur64(&v[1]),
                        to_ur64(&v[2]),
                    ]));
                }
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY) {
                    metadata.set_tag(ExifTag::GPSLongitudeRef(get_string_val(f)));
                }
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSAltitude, exif::In::PRIMARY)
                    && let exif::Value::Rational(v) = &f.value
                    && !v.is_empty()
                {
                    metadata.set_tag(ExifTag::GPSAltitude(vec![to_ur64(&v[0])]));
                }
                if let Some(f) = exif_obj.get_field(exif::Tag::GPSAltitudeRef, exif::In::PRIMARY) {
                    let alt_ref = f.value.get_uint(0).unwrap_or(0) as u8;
                    metadata.set_tag(ExifTag::GPSAltitudeRef(vec![alt_ref]));
                }
            }
        }
    }

    if !source_read_success && is_raw_file(original_path_str) {
        let loader = rawler::RawLoader::new();
        if let Ok(raw_source) = rawler::rawsource::RawSource::new(Path::new(original_path_str))
            && let Ok(decoder) = loader.get_decoder(&raw_source)
            && let Ok(meta) = decoder.raw_metadata(&raw_source, &Default::default())
        {
            if !meta.make.is_empty() {
                metadata.set_tag(ExifTag::Make(meta.make.clone()));
            }
            if !meta.model.is_empty() {
                metadata.set_tag(ExifTag::Model(meta.model.clone()));
            }
            let exif = meta.exif;
            if let Some(artist) = exif.artist {
                metadata.set_tag(ExifTag::Artist(artist));
            }
            if let Some(copyright) = exif.copyright {
                metadata.set_tag(ExifTag::Copyright(copyright));
            }
            if let Some(dt) = exif.date_time_original {
                metadata.set_tag(ExifTag::DateTimeOriginal(dt));
            }
            if let Some(dt) = exif.create_date {
                metadata.set_tag(ExifTag::CreateDate(dt));
            }
            if let Some(lens_make) = exif.lens_make {
                metadata.set_tag(ExifTag::LensMake(lens_make));
            }
            if let Some(lens_model) = exif.lens_model {
                metadata.set_tag(ExifTag::LensModel(lens_model));
            }
            if let Some(f) = exif.fnumber {
                metadata.set_tag(ExifTag::FNumber(vec![uR64 {
                    nominator: f.n,
                    denominator: f.d,
                }]));
            }
            if let Some(t) = exif.exposure_time {
                metadata.set_tag(ExifTag::ExposureTime(vec![uR64 {
                    nominator: t.n,
                    denominator: t.d,
                }]));
            }
            if let Some(fl) = exif.focal_length {
                metadata.set_tag(ExifTag::FocalLength(vec![uR64 {
                    nominator: fl.n,
                    denominator: fl.d,
                }]));
            }
            if let Some(iso) = exif.iso_speed {
                metadata.set_tag(ExifTag::ISO(vec![iso as u16]));
            } else if let Some(iso) = exif.iso_speed_ratings {
                metadata.set_tag(ExifTag::ISO(vec![iso]));
            }
            if let Some(ev) = exif.exposure_bias {
                metadata.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
                    nominator: ev.n,
                    denominator: ev.d,
                }]));
            }
            if let Some(flash) = exif.flash {
                metadata.set_tag(ExifTag::Flash(vec![flash]));
            }
            if let Some(metering) = exif.metering_mode {
                metadata.set_tag(ExifTag::MeteringMode(vec![metering]));
            }
            if let Some(wb) = exif.white_balance {
                metadata.set_tag(ExifTag::WhiteBalance(vec![wb]));
            }
            if let Some(prog) = exif.exposure_program {
                metadata.set_tag(ExifTag::ExposureProgram(vec![prog]));
            }
            if !strip_gps && let Some(gps) = exif.gps {
                if let Some(lat) = gps.gps_latitude {
                    metadata.set_tag(ExifTag::GPSLatitude(vec![
                        uR64 {
                            nominator: lat[0].n,
                            denominator: lat[0].d,
                        },
                        uR64 {
                            nominator: lat[1].n,
                            denominator: lat[1].d,
                        },
                        uR64 {
                            nominator: lat[2].n,
                            denominator: lat[2].d,
                        },
                    ]));
                }
                if let Some(lat_ref) = gps.gps_latitude_ref {
                    metadata.set_tag(ExifTag::GPSLatitudeRef(lat_ref));
                }
                if let Some(lon) = gps.gps_longitude {
                    metadata.set_tag(ExifTag::GPSLongitude(vec![
                        uR64 {
                            nominator: lon[0].n,
                            denominator: lon[0].d,
                        },
                        uR64 {
                            nominator: lon[1].n,
                            denominator: lon[1].d,
                        },
                        uR64 {
                            nominator: lon[2].n,
                            denominator: lon[2].d,
                        },
                    ]));
                }
                if let Some(lon_ref) = gps.gps_longitude_ref {
                    metadata.set_tag(ExifTag::GPSLongitudeRef(lon_ref));
                }
                if let Some(alt) = gps.gps_altitude {
                    metadata.set_tag(ExifTag::GPSAltitude(vec![uR64 {
                        nominator: alt.n,
                        denominator: alt.d,
                    }]));
                }
                if let Some(alt_ref) = gps.gps_altitude_ref {
                    metadata.set_tag(ExifTag::GPSAltitudeRef(vec![alt_ref]));
                }
            }
        }
    }

    metadata.set_tag(ExifTag::Software("Darkroom Index".to_string()));
    metadata.set_tag(ExifTag::Orientation(vec![1u16]));
    // sRGB, or "uncalibrated" for Display P3 so viewers follow the embedded
    // profile rather than a camera's copied sRGB tag.
    metadata.set_tag(ExifTag::ColorSpace(vec![space.exif_color_space()]));

    if let Err(e) = metadata.write_to_vec(image_bytes, file_type) {
        log::warn!("Failed to write metadata: {}", e);
    }

    Ok(())
}

pub fn get_primary_sidecar_path(image_path: &Path) -> PathBuf {
    let mut filename = image_path.file_name().unwrap_or_default().to_os_string();
    filename.push(".dri");
    image_path.with_file_name(filename)
}

/// RapidRAW's older separate EXIF file (`.rrexif`), read once and folded
/// into the edit file, then removed.
pub fn legacy_exif_path(image_path: &Path) -> PathBuf {
    let mut filename = image_path.file_name().unwrap_or_default().to_os_string();
    filename.push(".rrexif");
    image_path.with_file_name(filename)
}

fn load_primary_metadata(image_path: &Path) -> ImageMetadata {
    let primary = get_primary_sidecar_path(image_path);
    load_sidecar(&primary)
}

/// Store `exif` in the photo's own sidecar, under its lock. With
/// `only_if_missing`, EXIF already there (perhaps saved by another thread
/// meanwhile) is kept.
fn store_primary_exif(
    image_path: &Path,
    exif: HashMap<String, String>,
    only_if_missing: bool,
) -> Result<(), String> {
    let primary = get_primary_sidecar_path(image_path);
    modify_sidecar(&primary, |metadata| {
        if only_if_missing && metadata.exif.is_some() {
            return false;
        }
        metadata.exif = Some(exif);
        true
    })
    .map(|_| ())
}

pub fn read_exif_sidecar(image_path: &Path) -> Option<HashMap<String, String>> {
    let metadata = load_primary_metadata(image_path);
    if let Some(exif) = metadata.exif {
        return Some(exif);
    }

    let legacy = legacy_exif_path(image_path);
    if legacy.exists()
        && let Ok(content) = fs::read_to_string(&legacy)
        && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&content)
    {
        if store_primary_exif(image_path, map.clone(), false).is_ok() {
            let _ = fs::remove_file(&legacy);
        }
        return Some(map);
    }

    None
}

pub fn read_exif_data_from_bytes(path: &str, file_bytes: &[u8]) -> HashMap<String, String> {
    if is_raw_file(path)
        && let Some(map) = extract_metadata(file_bytes)
    {
        return map;
    }

    let mut exif_data = HashMap::new();
    if let Some(exif) = read_exif(file_bytes) {
        for field in exif.fields() {
            let raw_val = field.display_value().with_unit(&exif).to_string();
            exif_data.insert(field.tag.to_string(), truncate_large_exif(&raw_val));
        }
    }
    exif_data
}

pub fn read_exif_data(path: &str, file_bytes: &[u8]) -> HashMap<String, String> {
    let source_path = Path::new(path);
    if let Some(sidecar_exif) = read_exif_sidecar(source_path) {
        return sidecar_exif;
    }

    let exif_map = read_exif_data_from_bytes(path, file_bytes);
    if !exif_map.is_empty()
        && let Err(e) = store_primary_exif(source_path, exif_map.clone(), true)
    {
        log::warn!("{e}");
    }
    exif_map
}

pub fn persist_exif_if_missing(source_path: &Path, source_path_str: &str, file_bytes: &[u8]) {
    {
        let metadata = load_primary_metadata(source_path);
        if metadata.exif.is_some() {
            return;
        }
    }

    let legacy = legacy_exif_path(source_path);
    if legacy.exists()
        && let Ok(content) = fs::read_to_string(&legacy)
        && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&content)
    {
        if store_primary_exif(source_path, map, false).is_ok() {
            let _ = fs::remove_file(&legacy);
        }
        return;
    }

    let exif_map = read_exif_data_from_bytes(source_path_str, file_bytes);
    if exif_map.is_empty() {
        return;
    }

    if let Err(e) = store_primary_exif(source_path, exif_map, true) {
        log::warn!("{e}");
    }
}

pub fn write_exif_sidecar(source_path_str: &str, target_image_path: &Path) -> Result<(), String> {
    let source_path = Path::new(source_path_str);

    let exif_data = if let Some(existing) = read_exif_sidecar(source_path) {
        existing
    } else if let Ok(bytes) = fs::read(source_path) {
        read_exif_data_from_bytes(source_path_str, &bytes)
    } else {
        return Ok(());
    };

    if exif_data.is_empty() {
        return Ok(());
    }

    store_primary_exif(target_image_path, exif_data, false)
        .map_err(|e| format!("Failed to write sidecar: {}", e))
}

#[cfg(test)]
mod sidecar_tests {
    use super::*;

    /// Missing is a new photo; unreadable is an error, and a change to it
    /// is refused with the file left exactly as it was.
    #[test]
    fn an_unreadable_sidecar_is_never_replaced_by_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("new.jpg.dri");
        assert_eq!(try_load_sidecar(&missing).unwrap().rating, 0);

        let empty = dir.path().join("empty.jpg.dri");
        fs::write(&empty, "").unwrap();
        assert!(try_load_sidecar(&empty).is_ok());

        let torn = dir.path().join("torn.jpg.dri");
        let half = r#"{"version": 1, "rating": 5, "adjustm"#;
        fs::write(&torn, half).unwrap();
        assert!(try_load_sidecar(&torn).is_err());
        let refused = modify_sidecar(&torn, |m| {
            m.rating = 1;
            true
        });
        assert!(refused.is_err());
        assert_eq!(fs::read_to_string(&torn).unwrap(), half);
    }

    /// Changes made at once from many threads all land.
    #[test]
    fn concurrent_changes_all_land() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG.jpg.dri");
        std::thread::scope(|s| {
            for i in 0..16 {
                let path = &path;
                s.spawn(move || {
                    modify_sidecar(path, |m| {
                        m.tags.get_or_insert_with(Vec::new).push(format!("t{i}"));
                        true
                    })
                    .unwrap();
                });
            }
        });
        let tags = try_load_sidecar(&path).unwrap().tags.unwrap();
        assert_eq!(tags.len(), 16);
        // No temporary files are left beside it.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// An unchanged sidecar is not written, so listing never creates one.
    #[test]
    fn no_change_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG.jpg.dri");
        modify_sidecar(&path, |_| false).unwrap();
        assert!(!path.exists());
    }

    /// A nested change to the same sidecar on one thread does not wait on
    /// itself.
    #[test]
    fn nested_changes_do_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG.jpg.dri");
        modify_sidecar(&path, |outer| {
            modify_sidecar(&path, |inner| {
                inner.rating = 2;
                true
            })
            .unwrap();
            outer.flag = Some("pick".into());
            true
        })
        .unwrap();
        assert_eq!(
            try_load_sidecar(&path).unwrap().flag.as_deref(),
            Some("pick")
        );
    }

    /// ShutterSpeedValue is log2(1/t): 8 is 1/256 s, -1 is 2 s.
    #[test]
    fn shutter_speed_value_is_apex_not_seconds() {
        let map = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let close = |a: Option<f32>, b: f32| (a.unwrap() - b).abs() < 1e-6;
        assert!(close(
            exposure_secs_from_sidecar(&map(&[("ShutterSpeedValue", "8")])),
            1.0 / 256.0
        ));
        assert!(close(
            exposure_secs_from_sidecar(&map(&[("ShutterSpeedValue", "-1 EV")])),
            2.0
        ));
        // ExposureTime, when there, is used as it is.
        assert!(close(
            exposure_secs_from_sidecar(&map(&[
                ("ExposureTime", "1/250 s"),
                ("ShutterSpeedValue", "3")
            ])),
            1.0 / 250.0
        ));
        assert_eq!(exposure_secs_from_sidecar(&map(&[])), None);
    }

    /// The location as both readers store it in a sidecar.
    #[test]
    fn sidecar_gps_reads_both_ways_it_is_stored() {
        let map = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let r = |n: u32| uR64 {
            nominator: n,
            denominator: 10_000,
        };
        // As the file's EXIF reads it (with units).
        let from_file = SidecarGps::from_map(&map(&[
            ("GPSLatitude", "17 deg 32 min 6.5 sec S"),
            ("GPSLatitudeRef", "S"),
            ("GPSLongitude", "149 deg 34 min 0 sec W"),
            ("GPSLongitudeRef", "W"),
            ("GPSAltitude", "12.5 meters below sea level"),
            ("GPSAltitudeRef", "below sea level"),
        ]));
        assert_eq!(
            from_file.latitude,
            Some((vec![r(170_000), r(320_000), r(65_000)], 'S'))
        );
        assert_eq!(
            from_file.longitude,
            Some((vec![r(1_490_000), r(340_000), r(0)], 'W'))
        );
        assert_eq!(from_file.altitude, Some((r(125_000), true)));

        // As a RAW's metadata reads it.
        let from_raw = SidecarGps::from_map(&map(&[
            ("GPSLatitude", "40 deg 45 min 0 sec"),
            ("GPSLatitudeRef", "N"),
            ("GPSAltitude", "10"),
            ("GPSAltitudeRef", "0"),
        ]));
        assert_eq!(from_raw.latitude.unwrap().1, 'N');
        assert_eq!(from_raw.longitude, None);
        assert_eq!(from_raw.altitude, Some((r(100_000), false)));

        assert_eq!(SidecarGps::from_map(&map(&[])), SidecarGps::default());
    }
}
