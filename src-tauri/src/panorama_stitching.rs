use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use image::{DynamicImage, GrayImage, Rgb32FImage};
use nalgebra::{Matrix3, Vector3};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

use crate::formats::is_raw_file;
use crate::image_processing::apply_cpu_default_raw_processing;
use crate::panorama_utils::{finishing, processing, stitching};

pub const BRIEF_DESCRIPTOR_SIZE: usize = 256;
pub type Descriptor = [u8; BRIEF_DESCRIPTOR_SIZE / 8];

#[derive(Debug, Clone, Copy)]
pub struct KeyPoint {
    pub x: u32,
    pub y: u32,
}

pub struct Feature {
    pub keypoint: KeyPoint,
    pub descriptor: Descriptor,
}

#[derive(Debug, Clone, Copy)]
pub struct Match {
    pub index1: usize,
    pub index2: usize,
}

pub struct ImageInfo {
    pub id: usize,
    pub filename: String,
    pub image: Rgb32FImage,
    /// Every colour of the frame lies inside sRGB.
    pub fits_srgb: bool,
    pub low_detail_mask: GrayImage,
    pub scale_factor: f64,
    pub features: Vec<Feature>,
}

#[derive(Clone)]
pub struct MatchInfo {
    pub homography: Matrix3<f64>,
    pub inliers: usize,
}

#[tauri::command]
pub async fn stitch_panorama(
    paths: Vec<String>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if paths.len() < 2 {
        return Err("Please select at least two images to stitch.".to_string());
    }

    let source_paths: Vec<String> = paths
        .iter()
        .map(|p| parse_virtual_path(p).0.to_string_lossy().into_owned())
        .collect();

    let panorama_result_handle = state.panorama_result.clone();
    // A new panorama replaces an unsaved one (and its temporary file).
    *panorama_result_handle.lock().unwrap() = None;

    let task = tokio::task::spawn_blocking(move || {
        let sources: Vec<std::path::PathBuf> =
            source_paths.iter().map(std::path::PathBuf::from).collect();
        let panorama_result = stitch_images(source_paths, app_handle.clone());

        match panorama_result {
            Ok(panorama_image) => {
                let _ = app_handle.emit("panorama-progress", "Creating preview...");
                // Written as the merge it is (compact JPEG XL, or a
                // compressed 16-bit TIFF), and previewed by rendering that
                // file through the editor's pipeline.
                let state = app_handle.state::<AppState>();
                let finished = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle)
                    .map_err(|e| format!("The preview needs the GPU: {e}"))
                    .and_then(|context| {
                        let settings = load_settings(app_handle.clone()).unwrap_or_default();
                        crate::hdr_output::finish(
                            &context,
                            &state,
                            &panorama_image.into_rgb32f(),
                            &crate::hdr_output::Merged::Rendered,
                            crate::hdr_output::compact_from_settings(&settings),
                            sources,
                            "Pano",
                        )
                        .map_err(|e| format!("Failed to write the panorama: {e:#}"))
                    });
                match finished {
                    Ok((result, final_base64)) => {
                        *panorama_result_handle.lock().unwrap() = Some(result);
                        let _ = app_handle.emit(
                            "panorama-complete",
                            serde_json::json!({
                                "base64": final_base64,
                            }),
                        );
                        Ok(())
                    }
                    Err(e) => {
                        let _ = app_handle.emit("panorama-error", e.clone());
                        Err(e)
                    }
                }
            }
            Err(e) => {
                let _ = app_handle.emit("panorama-error", e.clone());
                Err(e)
            }
        }
    });

    match task.await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(join_err) => Err(format!("Panorama task failed: {}", join_err)),
    }
}

#[tauri::command]
pub async fn save_panorama(
    first_path_str: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let result = state
        .panorama_result
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| {
            "No panorama image found in memory to save. It might have already been saved."
                .to_string()
        })?;
    let output_path = crate::hdr_output::save(&result).map_err(|e| format!("{e:#}"))?;
    let (real_path, _) = parse_virtual_path(&first_path_str);
    let _ = crate::exif_processing::write_exif_sidecar(&real_path.to_string_lossy(), &output_path);
    // One stack in the library: the panorama on top, its frames inside.
    crate::versions::record_merge(&output_path, &result.sources, result.kind);
    Ok(output_path.to_string_lossy().to_string())
}

pub(crate) fn stitch_images(
    image_paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<DynamicImage, String> {
    if image_paths.len() < 2 {
        return Err("At least two images are required for a panorama.".to_string());
    }

    let _ = app_handle.emit("panorama-progress", "Starting panorama process...");
    println!(
        "Starting panorama stitching process for {} images...",
        image_paths.len()
    );

    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let start_time = Instant::now();
    let _ = app_handle.emit("panorama-progress", "Loading and preparing images...");
    println!("Loading and preparing images (in parallel)...");
    let brief_pairs = processing::generate_brief_pairs();

    let image_data_results: Vec<Result<ImageInfo, String>> = image_paths
        .par_iter()
        .enumerate()
        .map(|(i, filename)| {
            let _ = app_handle.emit(
                "panorama-progress",
                format!(
                    "Processing '{}'",
                    Path::new(filename)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
            );
            println!("  - Processing '{}'", filename);

            let file_bytes = fs::read(filename)
                .map_err(|e| format!("Failed to read image {}: {}", filename, e))?;

            // Every frame is stitched in Display P3, the space the finished
            // panorama is made in. Finished pictures are decoded by the
            // colour engine, through their own profile, as HDR merges'
            // frames are: the previous engine's decode reads every file as
            // sRGB, which dulled P3 and Adobe RGB photos.
            let profiled = (!is_raw_file(filename))
                .then(|| crate::hdr_output::load_frame(filename, &file_bytes))
                .and_then(|frame| {
                    frame
                        .inspect_err(|e| log::warn!("Panorama: '{filename}' read as sRGB ({e:#})"))
                        .ok()
                });
            let dynamic_image = if let Some(mut linear) = profiled {
                crate::hdr_output::to_display_p3(&mut linear);
                crate::hdr_output::encode_srgb(&mut linear);
                DynamicImage::ImageRgb32F(linear)
            } else {
                let mut dynamic_image = crate::image_loader::load_base_image_from_bytes(
                    &file_bytes,
                    filename,
                    false,
                    &settings,
                    None,
                )
                .map_err(|e| format!("Failed to load image {}: {}", filename, e))?;
                if is_raw_file(filename) {
                    apply_cpu_default_raw_processing(&mut dynamic_image);
                }
                let mut encoded = dynamic_image.into_rgb32f();
                crate::hdr_output::srgb_encoded_to_display_p3(&mut encoded);
                DynamicImage::ImageRgb32F(encoded)
            };

            let image_f32 = dynamic_image.to_rgb32f();

            let color_full_u8 = dynamic_image.to_rgb8();
            let gray_full = image::imageops::colorops::grayscale(&color_full_u8);

            let (w, h) = gray_full.dimensions();
            let (new_w, new_h, scale_factor) = processing::calculate_downscale_dimensions(w, h);

            let gray_small = image::imageops::resize(
                &gray_full,
                new_w,
                new_h,
                image::imageops::FilterType::Triangle,
            );

            let low_detail_mask = processing::generate_low_detail_mask(&gray_full);

            let features = processing::find_features(&gray_small, &brief_pairs);
            println!("    Found {} features in '{}'", features.len(), filename);

            Ok(ImageInfo {
                id: i,
                filename: filename.to_string(),
                fits_srgb: crate::hdr_output::fits_srgb(&image_f32),
                image: image_f32,
                low_detail_mask,
                scale_factor,
                features,
            })
        })
        .collect();

    let mut image_data = Vec::new();
    for result in image_data_results {
        match result {
            Ok(info) => image_data.push(info),
            Err(e) => return Err(e),
        }
    }

    println!(
        "Image loading and feature detection completed in {:.2?}\n",
        start_time.elapsed()
    );

    let start_time = Instant::now();
    let _ = app_handle.emit("panorama-progress", "Finding image matches...");
    println!("Finding all pairwise matches (in parallel)...");
    let mut pairwise_matches: HashMap<(usize, usize), MatchInfo> = HashMap::new();

    let pairs_to_check: Vec<(usize, usize)> = (0..image_data.len())
        .flat_map(|i| (i + 1..image_data.len()).map(move |j| (i, j)))
        .collect();

    let match_results: Vec<Option<((usize, usize), MatchInfo)>> = pairs_to_check
        .par_iter()
        .map(|&(i, j)| {
            let features1 = &image_data[i].features;
            let features2 = &image_data[j].features;

            let initial_matches = processing::match_features(features1, features2);
            if initial_matches.len() < processing::MIN_INLIERS_FOR_CONNECTION {
                return None;
            }

            let keypoints1: Vec<KeyPoint> = features1.iter().map(|f| f.keypoint).collect();
            let keypoints2: Vec<KeyPoint> = features2.iter().map(|f| f.keypoint).collect();

            let feature_size = {
                let (w, h) = image_data[i].image.dimensions();
                let (sw, sh, _) = processing::calculate_downscale_dimensions(w, h);
                (sw, sh)
            };
            let (h_small, inliers) = processing::find_homography_ransac(
                &initial_matches,
                &keypoints1,
                &keypoints2,
                feature_size,
            )?;
            println!(
                "  - Good match found: '{}' <-> '{}' ({} inliers of {} matches)",
                Path::new(&image_data[i].filename)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                Path::new(&image_data[j].filename)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                inliers.len(),
                initial_matches.len()
            );
            let s1 = image_data[i].scale_factor;
            let s2 = image_data[j].scale_factor;
            let scale_mat_i_inv =
                Matrix3::new(1.0 / s1, 0.0, 0.0, 0.0, 1.0 / s1, 0.0, 0.0, 0.0, 1.0);
            let scale_mat_j = Matrix3::new(s2, 0.0, 0.0, 0.0, s2, 0.0, 0.0, 0.0, 1.0);
            let h_full = scale_mat_j * h_small * scale_mat_i_inv;
            Some((
                (i, j),
                MatchInfo {
                    homography: h_full,
                    inliers: inliers.len(),
                },
            ))
        })
        .collect();

    for result in match_results.into_iter().flatten() {
        pairwise_matches.insert(result.0, result.1);
    }
    println!(
        "Pairwise matching completed in {:.2?}\n",
        start_time.elapsed()
    );

    if pairwise_matches.is_empty() {
        return Err(
            "No suitable matches found between any pair of images. Cannot create a panorama."
                .to_string(),
        );
    }

    let start_time = Instant::now();
    let _ = app_handle.emit("panorama-progress", "Determining stitching order...");
    println!("Determining stitching order...");
    let (ordered_indices, global_homographies) =
        build_stitching_order(&image_data, &pairwise_matches);

    if ordered_indices.len() < 2 {
        return Err("Could not find a connected sequence of at least two images.".to_string());
    }

    let ordered_filenames: Vec<_> = ordered_indices
        .iter()
        .map(|&i| {
            Path::new(&image_data[i].filename)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    println!("Stitching order determined: {:?}", ordered_filenames);
    let _ = app_handle.emit(
        "panorama-progress",
        format!("Stitching order: {}", ordered_filenames.join(" -> ")),
    );

    // Exposure compensation. Frames shot on auto exposure rarely match
    // each other, and a seam between two differently exposed frames reads
    // as a band across the sky however good the blend is. Estimate one
    // gain per frame from brightness ratios measured where frames overlap.
    {
        let _ = app_handle.emit("panorama-progress", "Matching exposure between frames...");
        let mut ratio_pairs: Vec<(usize, usize, f64)> = Vec::new();
        for (position, &i) in ordered_indices.iter().enumerate() {
            for &j in ordered_indices.iter().skip(position + 1) {
                let (Some(h_i), Some(h_j)) =
                    (global_homographies.get(&i), global_homographies.get(&j))
                else {
                    continue;
                };
                let Some(h_j_inv) = h_j.try_inverse() else {
                    continue;
                };
                let map = h_j_inv * h_i;
                let source = &image_data[i].image;
                let target = &image_data[j].image;
                let (sw, sh) = source.dimensions();
                let (tw, th) = target.dimensions();
                const GRID: u32 = 48;
                let mut samples: Vec<(f32, f32)> = Vec::with_capacity((GRID * GRID) as usize);
                for gy in 0..GRID {
                    for gx in 0..GRID {
                        let x = (gx as f64 + 0.5) * sw as f64 / GRID as f64;
                        let y = (gy as f64 + 0.5) * sh as f64 / GRID as f64;
                        let mapped = map * Vector3::new(x, y, 1.0);
                        if mapped.z.abs() < 1e-9 {
                            continue;
                        }
                        let (mx, my) = (mapped.x / mapped.z, mapped.y / mapped.z);
                        if mx < 0.0 || my < 0.0 || mx >= tw as f64 || my >= th as f64 {
                            continue;
                        }
                        let a = source.get_pixel((x as u32).min(sw - 1), (y as u32).min(sh - 1));
                        let b = target.get_pixel(mx as u32, my as u32);
                        let luma =
                            |p: &image::Rgb<f32>| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                        samples.push((luma(a), luma(b)));
                    }
                }
                if let Some(ratio) = finishing::median_ratio(&samples) {
                    ratio_pairs.push((i, j, ratio));
                }
            }
        }
        if ratio_pairs.is_empty() {
            println!("Exposure compensation: no usable overlaps, frames left as shot.");
        } else {
            let gains = finishing::estimate_gains(image_data.len(), &ratio_pairs);
            for (index, gain) in gains.iter().enumerate() {
                if (gain - 1.0).abs() > 0.01 {
                    println!(
                        "  - exposure gain {:.3} for '{}'",
                        gain,
                        Path::new(&image_data[index].filename)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    );
                }
                finishing::apply_gain(&mut image_data[index].image, *gain);
            }
        }
    }

    let stitched_images_info: Vec<&ImageInfo> =
        ordered_indices.iter().map(|&i| &image_data[i]).collect();
    let unstitched_count = image_data.len() - stitched_images_info.len();
    if unstitched_count > 0 {
        let warning_msg = format!(
            "Warning: {} image(s) could not be matched and will be excluded.",
            unstitched_count
        );
        println!("{}", warning_msg);
        let _ = app_handle.emit("panorama-warning", warning_msg);
    }
    println!(
        "Global homography calculation completed in {:.2?}\n",
        start_time.elapsed()
    );

    let start_time = Instant::now();
    let _ = app_handle.emit("panorama-progress", "Warping and blending images...");
    println!("Warping and blending full-resolution images with progressive optimal seams...");

    let panorama = stitching::progressive_seam_stitcher(
        &stitched_images_info,
        &global_homographies,
        app_handle.clone(),
    );

    println!("Stitching completed in {:.2?}\n", start_time.elapsed());

    let _ = app_handle.emit("panorama-progress", "Finalizing panorama...");

    // Trim the canvas the warp never wrote to. Conservative by design:
    // only completely unwritten lines go, so dark scene content is safe.
    let (crop_x, crop_y, crop_w, crop_h) = finishing::trim_empty_borders(&panorama);
    let mut panorama = if crop_w != panorama.width() || crop_h != panorama.height() {
        println!(
            "Trimmed unwritten canvas: {}x{} -> {}x{}",
            panorama.width(),
            panorama.height(),
            crop_w,
            crop_h
        );
        image::imageops::crop_imm(&panorama, crop_x, crop_y, crop_w, crop_h).to_image()
    } else {
        panorama
    };

    // Frames that all fit in sRGB make a panorama that does too: what
    // strays outside was made by the stitch (see `clip_to_srgb`).
    if stitched_images_info.iter().all(|info| info.fits_srgb) {
        crate::hdr_output::clip_to_srgb(&mut panorama);
    }

    Ok(DynamicImage::ImageRgb32F(panorama))
}

/// How much larger than itself a frame may become on the reference frame's
/// plane before it is left out: beyond this the plane is being asked to
/// hold too wide a view, and the frame would be a smear.
const MAX_GLOBAL_AREA_RATIO: f64 = 8.0;

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] == i {
            i
        } else {
            self.parent[i] = self.find(self.parent[i]);
            self.parent[i]
        }
    }

    fn union(&mut self, i: usize, j: usize) {
        let root_i = self.find(i);
        let root_j = self.find(j);
        if root_i != root_j {
            self.parent[root_i] = root_j;
        }
    }
}

fn build_stitching_order(
    images: &[ImageInfo],
    matches: &HashMap<(usize, usize), MatchInfo>,
) -> (Vec<usize>, HashMap<usize, Matrix3<f64>>) {
    if images.is_empty() {
        return (vec![], HashMap::new());
    }
    let n = images.len();
    if n < 2 {
        let mut homographies = HashMap::new();
        if n == 1 {
            homographies.insert(0, Matrix3::identity());
        }
        return ((0..n).collect(), homographies);
    }

    let mut edges = Vec::new();
    for (&(i, j), m) in matches {
        edges.push((m.inliers, i, j));
    }
    edges.sort_by_key(|&(inliers, _, _)| std::cmp::Reverse(inliers));

    let mut mst_adj: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut dsu = Dsu::new(n);
    let mut num_edges = 0;

    for &(_, i, j) in &edges {
        if dsu.find(i) != dsu.find(j) {
            dsu.union(i, j);
            mst_adj.entry(i).or_default().push(j);
            mst_adj.entry(j).or_default().push(i);
            num_edges += 1;
            if num_edges == n - 1 {
                break;
            }
        }
    }

    // The reference frame — the one left undistorted, the others warped
    // onto its plane — is the middle of the largest connected group: the
    // frame fewest hops from the farthest. (A leaf of whichever group came
    // first was used: an end frame, which leaves the far end of a wide
    // panorama stretched most, or a pair while five frames matched
    // elsewhere.)
    let hops_from = |start: usize| -> HashMap<usize, usize> {
        let mut hops = HashMap::from([(start, 0)]);
        let mut q = VecDeque::from([start]);
        while let Some(u) = q.pop_front() {
            for &v in mst_adj.get(&u).into_iter().flatten() {
                if !hops.contains_key(&v) {
                    hops.insert(v, hops[&u] + 1);
                    q.push_back(v);
                }
            }
        }
        hops
    };
    let start_node = (0..n)
        .filter(|i| mst_adj.contains_key(i))
        .map(|i| {
            let hops = hops_from(i);
            let eccentricity = hops.values().copied().max().unwrap_or(0);
            (i, hops.len(), eccentricity)
        })
        .max_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then(b.0.cmp(&a.0)))
        .map_or(0, |(i, _, _)| i);

    let mut ordered_indices = Vec::new();
    let mut global_homographies = HashMap::new();
    let mut q = VecDeque::new();
    let mut visited = HashSet::new();

    q.push_back((start_node, Matrix3::identity()));
    visited.insert(start_node);

    while let Some((u, h_u_global)) = q.pop_front() {
        ordered_indices.push(u);
        global_homographies.insert(u, h_u_global);

        if let Some(neighbors) = mst_adj.get(&u) {
            for &v in neighbors {
                if !visited.contains(&v) {
                    visited.insert(v);

                    let h_vu = if let Some(m) = matches.get(&(v, u)) {
                        Some(m.homography)
                    } else if let Some(m) = matches.get(&(u, v)) {
                        m.homography.try_inverse()
                    } else {
                        None
                    };
                    // Scaled so frames in front of the camera map with z > 0.
                    let Some(h_v_global) = h_vu
                        .map(|h| h_u_global * h)
                        .map(|h: Matrix3<f64>| h / h[(2, 2)])
                        .filter(|h| {
                            processing::is_plausible_warp(
                                h,
                                images[v].image.dimensions(),
                                MAX_GLOBAL_AREA_RATIO,
                            )
                        })
                    else {
                        // Its warp onto the reference would fold or blow
                        // up (too far round for one plane): left out, with
                        // the frames reached through it.
                        println!(
                            "  - '{}' cannot be placed on the reference frame's plane; left out.",
                            Path::new(&images[v].filename)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        );
                        continue;
                    };
                    q.push_back((v, h_v_global));
                }
            }
        }
    }

    (ordered_indices, global_homographies)
}
