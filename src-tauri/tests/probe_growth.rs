//! Diagnostic: iterative ghost-box growth. Click -> low-confidence component
//! containing the click -> padded bbox -> re-decode with box+click+prior.
use ndarray::Array;
use ort::value::Tensor;
use rapidraw_lib::{ai_processing, model_registry::ModelRegistry};
use std::path::PathBuf;
fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k}"))
}

#[test]
#[ignore = "diagnostic"]
fn ghost_box_growth() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let encoder = registry.get_session("sam-vit-b", None).unwrap();
    let decoder = registry.get_session("sam-vit-b", Some("decoder")).unwrap();
    let image = image::open(env("SUBJECT_PHOTO")).unwrap();
    let emb = ai_processing::generate_image_embeddings(&image, &encoder).unwrap();
    let (w, h) = emb.original_size;
    let c: Vec<f64> = env("SUBJECT_CLICK")
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let scale = 1024.0 / w.max(h) as f64;
    let side = 256usize;
    let area = side * side;
    let g = |v: f64| v * scale * side as f64 / 1024.0; // image px -> grid
    let (gcx, gcy) = (
        (g(c[0]) as usize).min(side - 1),
        (g(c[1]) as usize).min(side - 1),
    );
    let vw = ((w as f64 * scale) as usize * side / 1024).max(1);
    let vh = ((h as f64 * scale) as usize * side / 1024).max(1);
    let mut session = decoder.lock().unwrap();
    let mut prior: Option<Vec<f32>> = None;
    let mut bbox: Option<(f32, f32, f32, f32)> = None; // in 1024 space
    let mut prev_mask: Option<Vec<bool>> = None;
    for pass in 0..5 {
        let mut coords = vec![(c[0] * scale) as f32, (c[1] * scale) as f32];
        let mut labels = vec![1.0f32];
        if let Some((x0, y0, x1, y1)) = bbox {
            coords.extend([x0, y0, x1, y1]);
            labels.extend([2.0, 3.0]);
        } else {
            coords.extend([0.0, 0.0]);
            labels.push(-1.0);
        }
        let n = labels.len();
        let has_prior = prior.is_some();
        let mask_in = prior.clone().unwrap_or_else(|| vec![0.0; area]);
        let out = session
            .run(ort::inputs![
                Tensor::from_array(emb.embeddings.clone()).unwrap(),
                Tensor::from_array(Array::from_shape_vec((1, n, 2), coords).unwrap()).unwrap(),
                Tensor::from_array(Array::from_shape_vec((1, n), labels).unwrap()).unwrap(),
                Tensor::from_array(Array::from_shape_vec((1, 1, 256, 256), mask_in).unwrap())
                    .unwrap(),
                Tensor::from_array(Array::from_vec(vec![if has_prior { 1.0f32 } else { 0.0 }]))
                    .unwrap(),
                Tensor::from_array(Array::from_vec(vec![h as f32, w as f32])).unwrap(),
            ])
            .unwrap();
        let data: Vec<f32> = out[2]
            .try_extract_array::<f32>()
            .unwrap()
            .iter()
            .copied()
            .collect();
        let scores: Vec<f32> = out[1]
            .try_extract_array::<f32>()
            .unwrap()
            .iter()
            .copied()
            .collect();
        // choose: with a box, token 0 is the prompt-consistent one; report all
        let cands: Vec<&[f32]> = data.chunks_exact(area).collect();
        let pick = if bbox.is_some() {
            0
        } else {
            // hierarchy rule: largest cand (1..3) containing the top-iou cand
            let ok: Vec<usize> = (1..4)
                .filter(|&i| cands[i][gcy * side + gcx] > 0.0)
                .collect();
            let top = *ok
                .iter()
                .max_by(|&&a, &&b| scores[a].total_cmp(&scores[b]))
                .unwrap_or(&1);
            let ar = |i: usize| cands[i].iter().filter(|&&v| v > 0.0).count();
            let contains = |big: usize, small: usize| {
                cands[big]
                    .iter()
                    .zip(cands[small])
                    .filter(|(b, s)| **b > 0.0 && **s > 0.0)
                    .count() as f32
                    / ar(small).max(1) as f32
            };
            ok.iter()
                .copied()
                .filter(|&i| {
                    scores[i] >= scores[top] - 0.25
                        && ar(i) >= ar(top) * 3 / 2
                        && contains(i, top) >= 0.85
                        && ar(i) < vw * vh * 9 / 10
                })
                .max_by_key(|&i| ar(i))
                .unwrap_or(top)
        };
        let m = cands[pick];
        let pos: Vec<bool> = m.iter().map(|&v| v > 0.0).collect();
        // ghost component (prob>0.2 == logit>-1.386) containing click, union over cands 1..3 (or pick if boxed)
        let mut ghost = vec![false; area];
        for i in if bbox.is_some() { 0..1 } else { 1..4 } {
            for (k, &v) in cands[i].iter().enumerate() {
                if v > -1.386 {
                    ghost[k] = true;
                }
            }
        }
        let mut seen = vec![false; area];
        let mut stack = vec![gcy * side + gcx];
        let (mut x0, mut y0, mut x1, mut y1) = (side, side, 0, 0);
        let mut gn = 0;
        while let Some(k) = stack.pop() {
            if seen[k] || !ghost[k] {
                continue;
            }
            seen[k] = true;
            gn += 1;
            let (x, y) = (k % side, k / side);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            if x > 0 {
                stack.push(k - 1);
            }
            if x + 1 < side {
                stack.push(k + 1);
            }
            if y >= 1 {
                stack.push(k - side);
            }
            if y + 1 < side {
                stack.push(k + side);
            }
        }
        let iou_prev = prev_mask.as_ref().map(|p| {
            let i = p.iter().zip(&pos).filter(|(a, b)| **a && **b).count();
            let u = p
                .iter()
                .zip(&pos)
                .filter(|(a, b)| **a || **b)
                .count()
                .max(1);
            i as f32 / u as f32
        });
        let to_img = |gx: usize| gx as f64 * 1024.0 / side as f64 / scale;
        println!(
            "pass {pass}: pick cand{pick} iou_pred {:.3} area {:.1}%  ghost {:.1}% bbox img x{:.0}-{:.0} y{:.0}-{:.0}  IoU(prev) {}",
            scores[pick],
            100.0 * pos.iter().filter(|&&b| b).count() as f32 / (vw * vh) as f32,
            100.0 * gn as f32 / (vw * vh) as f32,
            to_img(x0),
            to_img(x1 + 1),
            to_img(y0),
            to_img(y1 + 1),
            iou_prev.map(|v| format!("{v:.3}")).unwrap_or("-".into())
        );
        if let Some(v) = iou_prev
            && v > 0.99
        {
            println!("converged");
            break;
        }
        // next prompt: padded ghost bbox in 1024 space
        let pad = 0.05f32;
        let (bx0, by0, bx1, by1) = (
            x0 as f32 * 4.0,
            y0 as f32 * 4.0,
            (x1 + 1) as f32 * 4.0,
            (y1 + 1) as f32 * 4.0,
        );
        let (pw, ph) = ((bx1 - bx0) * pad, (by1 - by0) * pad);
        bbox = Some((
            (bx0 - pw).max(0.0),
            (by0 - ph).max(0.0),
            (bx1 + pw).min(1024.0),
            (by1 + ph).min(1024.0),
        ));
        prior = Some(m.to_vec());
        prev_mask = Some(pos);
        if pass == 4 {
            let mut img = image::GrayImage::new(vw as u32, vh as u32);
            for y in 0..vh {
                for x in 0..vw {
                    let p = 1.0 / (1.0 + (-m[y * side + x]).exp());
                    img.put_pixel(x as u32, y as u32, image::Luma([(p * 255.0) as u8]));
                }
            }
            let d = PathBuf::from(env("SUBJECT_OUTPUT"));
            std::fs::create_dir_all(&d).unwrap();
            img.save(d.join("growth-final.png")).unwrap();
        }
    }
}
