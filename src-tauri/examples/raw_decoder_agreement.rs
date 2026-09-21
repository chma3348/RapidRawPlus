//! Do the previous engine's RAW decode (what heal, fill and Sky Replace
//! patches are made from) and v3's RAW decode agree? If not, a patch on a
//! RAW file lands off in v3.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("a RAW file");
    let bytes = std::fs::read(&path)?;
    let settings = rapidraw_lib::AppSettings::default();
    let legacy = rapidraw_lib::image_loader::load_base_image_from_bytes(
        &bytes, &path, false, &settings, None,
    )?
    .to_rgb32f();
    let v3 = rapidraw_lib::color_engine::raw::decode_raw(&bytes, false, || Ok(()))?;
    println!(
        "legacy {}x{}  v3 {}x{}  v3 space {:?}",
        legacy.width(),
        legacy.height(),
        v3.pixels.width(),
        v3.pixels.height(),
        v3.color
    );
    let (w, h) = (
        legacy.width().min(v3.pixels.width()),
        legacy.height().min(v3.pixels.height()),
    );
    // Compare on a coarse grid of block means, robust to a pixel of offset.
    let block = 64;
    let mut ratios = [Vec::new(), Vec::new(), Vec::new()];
    for by in (0..h - block).step_by(block as usize * 2) {
        for bx in (0..w - block).step_by(block as usize * 2) {
            let mut a = [0.0f64; 3];
            let mut b = [0.0f64; 3];
            for y in by..by + block {
                for x in bx..bx + block {
                    let p = legacy.get_pixel(x, y);
                    let q = v3.pixels.get_pixel(x, y);
                    for c in 0..3 {
                        a[c] += p[c] as f64;
                        b[c] += q[c] as f64;
                    }
                }
            }
            for c in 0..3 {
                if a[c] > 1e-3 * (block * block) as f64 {
                    ratios[c].push(b[c] / a[c]);
                }
            }
        }
    }
    for (c, r) in ratios.iter_mut().enumerate() {
        r.sort_by(|x, y| x.total_cmp(y));
        let med = r[r.len() / 2];
        let spread = r[r.len() * 9 / 10] / r[r.len() / 10];
        println!(
            "{}: v3/legacy median {:.3}  (p90/p10 {:.3})",
            "RGB".as_bytes()[c] as char,
            med,
            spread
        );
    }
    Ok(())
}
