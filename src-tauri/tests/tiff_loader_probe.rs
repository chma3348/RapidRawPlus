//! Loads real files through the app's loader to see which formats fail.
//! Gated on RAPIDRAW_LOADER_PROBE (colon-separated file paths).

#[test]
fn loader_probe() {
    let Ok(list) = std::env::var("RAPIDRAW_LOADER_PROBE") else {
        return;
    };
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("READ-FAIL {path}: {e}");
                continue;
            }
        };
        match rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None) {
            Ok(img) => {
                let rgb = img.to_rgb32f();
                let n = (rgb.width() as f64) * (rgb.height() as f64);
                let mut sums = [0f64; 3];
                for p in rgb.pixels() {
                    for c in 0..3 {
                        sums[c] += p[c] as f64;
                    }
                }
                println!(
                    "OK   {} -> {}x{} meanRGB {:.5} {:.5} {:.5}",
                    path,
                    img.width(),
                    img.height(),
                    sums[0] / n,
                    sums[1] / n,
                    sums[2] / n
                );
            }
            Err(e) => println!("FAIL {path}: {e:#}"),
        }
    }
}
