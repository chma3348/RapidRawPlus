//! Heal spots on real photos with made-up damage, where the undamaged photo
//! is the answer: dust, bright specks, hairs and small objects, some on open
//! ground and some beside an edge. Each case is healed the old way (the
//! source picker and blend the app shipped with, composited as the app did)
//! and the new way (`heal_engine`), and scored inside the stroke against the
//! clean photo.
//!
//! cargo run --release --example heal_lab -- <out dir> <photo> [<photo> ...]
//!
//! Writes `<photo stem>_sheet.jpg` (clean | damaged with the stroke outlined |
//! old | new, one case a row) and prints one line per case plus totals.
use darkroom_index::heal_blend::heal_blend;
use darkroom_index::heal_engine::{Spot, find_source, heal_in_place, legacy_find_source};
use darkroom_index::mask_generation::{MaskDefinition, generate_mask_bitmap};
use image::{DynamicImage, GrayImage, ImageReader, Rgb, RgbImage, RgbaImage};
use serde_json::json;

const WORK_LONG: u32 = 3000;
const FEATHER: f32 = 0.5;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.next()
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Dust,
    Speck,
    Hair,
    Object,
}

struct Case {
    kind: Kind,
    near_edge: bool,
    /// Stroke points and brush diameter, image pixels.
    points: Vec<(f64, f64)>,
    brush: f32,
}

fn brush_mask(w: u32, h: u32, points: &[(f64, f64)], brush: f32) -> GrayImage {
    let pts: Vec<_> = points
        .iter()
        .map(|(x, y)| json!({"x": x, "y": y}))
        .collect();
    let def: MaskDefinition = serde_json::from_value(json!({
        "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100,
        "adjustments": {},
        "subMasks": [{"id": "s", "type": "brush", "visible": true, "mode": "additive",
            "parameters": {"lines": [{"tool": "brush", "brushSize": brush, "points": pts,
                "feather": FEATHER}]}}],
    }))
    .unwrap();
    generate_mask_bitmap(&def, w, h, 1.0, (0.0, 0.0), None).unwrap()
}

fn luma(p: &Rgb<u8>) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Pixels on strong edges, for placing damage beside one.
fn edge_points(img: &RgbImage) -> Vec<(u32, u32)> {
    let (w, h) = img.dimensions();
    let mut v = Vec::new();
    for y in (8..h - 8).step_by(3) {
        for x in (8..w - 8).step_by(3) {
            let gx = luma(img.get_pixel(x + 1, y)) - luma(img.get_pixel(x - 1, y));
            let gy = luma(img.get_pixel(x, y + 1)) - luma(img.get_pixel(x, y - 1));
            v.push(((gx * gx + gy * gy).sqrt(), x, y));
        }
    }
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.truncate(v.len() / 25);
    v.into_iter().map(|(_, x, y)| (x, y)).collect()
}

/// Draws one piece of damage into `img` and returns the stroke a person
/// would paint over it.
fn damage(
    img: &mut RgbImage,
    rng: &mut Rng,
    kind: Kind,
    centre: (f32, f32),
) -> (Vec<(f64, f64)>, f32) {
    let (w, h) = img.dimensions();
    let (cx, cy) = centre;
    let mut paint = |x: i32, y: i32, f: &dyn Fn([f32; 3]) -> [f32; 3]| {
        if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 {
            let p = img.get_pixel_mut(x as u32, y as u32);
            let v = f([p[0] as f32, p[1] as f32, p[2] as f32]);
            *p = Rgb(v.map(|c| c.clamp(0.0, 255.0).round() as u8));
        }
    };
    match kind {
        Kind::Dust | Kind::Speck => {
            let (r, k, bright) = match kind {
                Kind::Dust => (rng.range(3.0, 10.0), rng.range(0.25, 0.55), false),
                _ => (rng.range(1.5, 3.5), rng.range(70.0, 130.0), true),
            };
            let s = r / 2.0;
            let ext = (r * 2.0).ceil() as i32;
            for dy in -ext..=ext {
                for dx in -ext..=ext {
                    let d2 = (dx * dx + dy * dy) as f32;
                    let g = (-d2 / (2.0 * s * s)).exp();
                    paint(cx as i32 + dx, cy as i32 + dy, &|p| {
                        if bright {
                            p.map(|c| c + k * g)
                        } else {
                            p.map(|c| c * (1.0 - k * g))
                        }
                    });
                }
            }
            (vec![(cx as f64, cy as f64)], 2.0 * (r * 1.35 + 3.0))
        }
        Kind::Hair => {
            let len = rng.range(40.0, 140.0);
            let width = rng.range(1.0, 2.2);
            let mut ang = rng.range(0.0, std::f32::consts::TAU);
            let mut pts = vec![(cx - ang.cos() * len / 2.0, cy - ang.sin() * len / 2.0)];
            for _ in 0..5 {
                ang += rng.range(-0.35, 0.35);
                let (lx, ly) = *pts.last().unwrap();
                pts.push((lx + ang.cos() * len / 5.0, ly + ang.sin() * len / 5.0));
            }
            let dark = rng.range(0.45, 0.75);
            for seg in pts.windows(2) {
                let ((ax, ay), (bx, by)) = (seg[0], seg[1]);
                let (x0, x1) = (ax.min(bx) as i32 - 4, ax.max(bx) as i32 + 4);
                let (y0, y1) = (ay.min(by) as i32 - 4, ay.max(by) as i32 + 4);
                let (dx, dy) = (bx - ax, by - ay);
                let l2 = dx * dx + dy * dy;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let t =
                            (((x as f32 - ax) * dx + (y as f32 - ay) * dy) / l2).clamp(0.0, 1.0);
                        let d = ((x as f32 - ax - t * dx).powi(2)
                            + (y as f32 - ay - t * dy).powi(2))
                        .sqrt();
                        let cov = (width / 2.0 + 0.5 - d).clamp(0.0, 1.0);
                        if cov > 0.0 {
                            paint(x, y, &|p| p.map(|c| c * (1.0 - dark * cov)));
                        }
                    }
                }
            }
            let stroke = pts.iter().map(|(x, y)| (*x as f64, *y as f64)).collect();
            (stroke, width + 10.0)
        }
        Kind::Object => {
            let r = rng.range(12.0, 26.0);
            let col = if rng.next() < 0.5 {
                [
                    rng.range(10.0, 50.0),
                    rng.range(10.0, 50.0),
                    rng.range(10.0, 50.0),
                ]
            } else {
                [
                    rng.range(200.0, 250.0),
                    rng.range(150.0, 250.0),
                    rng.range(60.0, 200.0),
                ]
            };
            let ext = r.ceil() as i32 + 2;
            for dy in -ext..=ext {
                for dx in -ext..=ext {
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    let a = (r - d + 0.5).clamp(0.0, 1.0);
                    let n = ((dx * 7 + dy * 13) % 9) as f32 * 2.0;
                    paint(cx as i32 + dx, cy as i32 + dy, &|p| {
                        [0, 1, 2].map(|c| p[c] * (1.0 - a) + (col[c] + n) * a)
                    });
                }
            }
            (vec![(cx as f64, cy as f64)], 2.0 * (r + 5.0))
        }
    }
}

fn to_rgba(img: &RgbImage) -> RgbaImage {
    DynamicImage::ImageRgb8(img.clone()).to_rgba8()
}

/// Mean absolute error against the clean photo over the stroke's footprint.
fn error(out: &RgbaImage, clean: &RgbImage, mask: &GrayImage) -> f32 {
    let (mut s, mut n) = (0.0f64, 0u64);
    for (x, y, m) in mask.enumerate_pixels() {
        if m[0] > 0 {
            let (a, b) = (out.get_pixel(x, y), clean.get_pixel(x, y));
            for c in 0..3 {
                s += (a[c] as f64 - b[c] as f64).abs();
            }
            n += 3;
        }
    }
    (s / n.max(1) as f64) as f32
}

/// Tone error: the stroke's 9x9 local means against the clean photo's, so a
/// blotch or ghost counts and a different-but-plausible grain does not.
fn blotch(out: &RgbaImage, clean: &RgbImage, mask: &GrayImage, spot: &Spot) -> f32 {
    let (w, h) = clean.dimensions();
    let local = |f: &dyn Fn(u32, u32, usize) -> f32, x: i32, y: i32, c: usize| -> f32 {
        let (mut s, mut n) = (0.0, 0.0);
        for dy in -4..=4 {
            for dx in -4..=4 {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    s += f(sx as u32, sy as u32, c);
                    n += 1.0;
                }
            }
        }
        s / n
    };
    let (mut s, mut n) = (0.0f64, 0u64);
    for y in spot.y0..spot.y0 + spot.h as i32 {
        for x in spot.x0..spot.x0 + spot.w as i32 {
            if mask.get_pixel(x as u32, y as u32)[0] == 0 {
                continue;
            }
            for c in 0..3 {
                let a = local(&|x, y, c| out.get_pixel(x, y)[c] as f32, x, y, c);
                let b = local(&|x, y, c| clean.get_pixel(x, y)[c] as f32, x, y, c);
                s += (a - b).abs() as f64;
                n += 1;
            }
        }
    }
    (s / n.max(1) as f64) as f32
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out_dir = std::path::PathBuf::from(&args[0]);
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut totals: std::collections::BTreeMap<String, (f32, f32, u32, u32)> = Default::default();
    for (pi, path) in args[1..].iter().enumerate() {
        let mut decoder = ImageReader::open(path).unwrap().into_decoder().unwrap();
        let orientation = image::ImageDecoder::orientation(&mut decoder).unwrap();
        let mut photo = DynamicImage::from_decoder(decoder).unwrap();
        photo.apply_orientation(orientation);
        let photo = photo.resize(WORK_LONG, WORK_LONG, image::imageops::FilterType::Lanczos3);
        let clean = photo.to_rgb8();
        let (w, h) = clean.dimensions();
        let edges = edge_points(&clean);
        let mut rng = Rng(0x9E3779B97F4A7C15 ^ ((pi as u64 + 1) * 0x2545F4914F6CDD1D));
        let plan = [
            (Kind::Dust, false),
            (Kind::Dust, false),
            (Kind::Dust, true),
            (Kind::Dust, true),
            (Kind::Speck, false),
            (Kind::Speck, true),
            (Kind::Hair, false),
            (Kind::Hair, true),
            (Kind::Object, false),
            (Kind::Object, true),
        ];
        let stem = std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let mut rows: Vec<RgbImage> = Vec::new();
        for (ci, &(kind, near_edge)) in plan.iter().enumerate() {
            let mut damaged = clean.clone();
            let centre = if near_edge {
                let (ex, ey) = edges[(rng.next() * edges.len() as f32) as usize % edges.len()];
                let a = rng.range(0.0, std::f32::consts::TAU);
                let off = match kind {
                    Kind::Object => 30.0,
                    Kind::Hair => 20.0,
                    _ => 12.0,
                };
                (ex as f32 + a.cos() * off, ey as f32 + a.sin() * off)
            } else {
                (
                    rng.range(0.1, 0.9) * w as f32,
                    rng.range(0.1, 0.9) * h as f32,
                )
            };
            let (points, brush) = damage(&mut damaged, &mut rng, kind, centre);
            let case = Case {
                kind,
                near_edge,
                points,
                brush,
            };
            let mask = brush_mask(w, h, &case.points, case.brush);
            let damaged_rgba = to_rgba(&damaged);

            // Old: boosted mask, bounding-box luma source, Poisson from the
            // stroke's edge pixels, composited again through the mask.
            let mut boosted = mask.clone();
            for p in boosted.pixels_mut() {
                let v = p[0] as f32 / 255.0;
                p[0] = (((v - 0.06) / 0.24).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            let spot = Spot::from_mask(&boosted).unwrap();
            let bounds = (
                spot.x0 as u32,
                spot.y0 as u32,
                (spot.x0 + spot.w as i32 - 1) as u32,
                (spot.y0 + spot.h as i32 - 1) as u32,
            );
            let t = std::time::Instant::now();
            let old_off = legacy_find_source(&damaged_rgba, &boosted, bounds, &[]);
            let healed = heal_blend(&damaged_rgba, &boosted, old_off.0, old_off.1);
            let mut old = damaged_rgba.clone();
            for (x, y, m) in boosted.enumerate_pixels() {
                if m[0] > 0 {
                    let a = m[0] as f32 / 255.0;
                    let (o, hp) = (old.get_pixel_mut(x, y), healed.get_pixel(x, y));
                    for c in 0..3 {
                        o[c] = (o[c] as f32 * (1.0 - a) + hp[c] as f32 * a).round() as u8;
                    }
                }
            }
            let ms_old = t.elapsed().as_millis();

            let t = std::time::Instant::now();
            let new_off = find_source(&damaged_rgba, &spot, Some(&boosted), &[]);
            let mut new = damaged_rgba.clone();
            heal_in_place(&mut new, &spot, new_off, Some(&boosted));
            let ms_new = t.elapsed().as_millis();
            if std::env::var("HEAL_DEBUG").ok().as_deref() == Some(&format!("{stem}#{ci}")) {
                let cy = spot.y0 + spot.h as i32 / 2;
                for x in (spot.x0 - 6..spot.x0 + spot.w as i32 + 6).step_by(3) {
                    let g = |img: &RgbaImage| img.get_pixel(x as u32, cy as u32)[0];
                    let s = new_off;
                    println!(
                        "x{x} a{:.2} clean {} dmg {} old {} new {} src {}",
                        spot.alpha_at(x, cy),
                        clean.get_pixel(x as u32, cy as u32)[0],
                        g(&damaged_rgba),
                        g(&old),
                        g(&new),
                        damaged_rgba.get_pixel((x + s.0) as u32, (cy + s.1) as u32)[0]
                    );
                }
            }

            let (e_dmg, e_old, e_new) = (
                error(&damaged_rgba, &clean, &boosted),
                blotch(&old, &clean, &boosted, &spot),
                blotch(&new, &clean, &boosted, &spot),
            );
            println!(
                "{stem} #{ci} {:?}{} brush {:.0}: damaged {e_dmg:.1} | old {e_old:.1} src {old_off:?} {ms_old}ms | new {e_new:.1} src {new_off:?} {ms_new}ms",
                case.kind,
                if case.near_edge { " @edge" } else { "" },
                case.brush
            );
            let key = format!(
                "{:?}{}",
                case.kind,
                if case.near_edge { " @edge" } else { "" }
            );
            let e = totals.entry(key).or_default();
            e.0 += e_old;
            e.1 += e_new;
            e.2 += 1;
            e.3 += (e_new < e_old) as u32;

            // Sheet row: a window around the stroke, four panels.
            let size = spot.w.max(spot.h) as i32;
            let half = (size + 40).max(48);
            let (cx, cy) = (spot.x0 + spot.w as i32 / 2, spot.y0 + spot.h as i32 / 2);
            let wx0 = (cx - half).clamp(0, w as i32 - 2 * half);
            let wy0 = (cy - half).clamp(0, h as i32 - 2 * half);
            let side = (2 * half) as u32;
            let crop = |img: &RgbImage| -> RgbImage {
                image::imageops::crop_imm(img, wx0 as u32, wy0 as u32, side, side).to_image()
            };
            let mut outlined = damaged.clone();
            for (x, y, m) in mask.enumerate_pixels() {
                if m[0] > 0 {
                    let edge = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .any(|(dx, dy)| {
                            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                            nx < 0
                                || ny < 0
                                || nx >= w as i32
                                || ny >= h as i32
                                || mask.get_pixel(nx as u32, ny as u32)[0] == 0
                        });
                    if edge {
                        outlined.put_pixel(x, y, Rgb([255, 40, 40]));
                    }
                }
            }
            let panels = [
                crop(&clean),
                crop(&outlined),
                crop(&DynamicImage::ImageRgba8(old).to_rgb8()),
                crop(&DynamicImage::ImageRgba8(new).to_rgb8()),
            ];
            let tile = 300u32;
            let mut row = RgbImage::from_pixel(4 * (tile + 6), tile + 6, Rgb([255, 255, 255]));
            for (k, p) in panels.iter().enumerate() {
                let r =
                    image::imageops::resize(p, tile, tile, image::imageops::FilterType::Nearest);
                image::imageops::overlay(&mut row, &r, k as i64 * (tile as i64 + 6), 3);
            }
            rows.push(row);
        }
        let rw = rows[0].width();
        let mut sheet = RgbImage::from_pixel(
            rw,
            rows.iter().map(|r| r.height()).sum(),
            Rgb([255, 255, 255]),
        );
        let mut y = 0;
        for r in &rows {
            image::imageops::overlay(&mut sheet, r, 0, y);
            y += r.height() as i64;
        }
        sheet
            .save(out_dir.join(format!("{stem}_sheet.jpg")))
            .unwrap();
    }
    println!(
        "\ntone error (9px local means) inside the stroke, lower is better, cases where new beats old:"
    );
    let (mut so, mut sn, mut n, mut wins) = (0.0, 0.0, 0, 0);
    for (k, (o, nw, c, wn)) in &totals {
        println!(
            "  {k:<14} old {:.1}  new {:.1}  new better in {wn}/{c}",
            o / *c as f32,
            nw / *c as f32
        );
        so += o;
        sn += nw;
        n += c;
        wins += wn;
    }
    println!(
        "  ALL            old {:.1}  new {:.1}  new better in {wins}/{n}",
        so / n as f32,
        sn / n as f32
    );
}
