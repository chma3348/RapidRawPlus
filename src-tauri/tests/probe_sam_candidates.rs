//! Diagnostic: dump every SAM hypothesis for a click so ranking can be judged.
//!   SUBJECT_MODELS=... ORT_DYLIB_PATH=... SUBJECT_PHOTO=... SUBJECT_CLICK=x,y \
//!   SUBJECT_OUTPUT=dir cargo test --test probe_sam_candidates -- --ignored --nocapture
use ndarray::Array;
use ort::value::Tensor;
use rapidraw_lib::{ai_processing, model_registry::ModelRegistry};
use std::path::PathBuf;

#[test]
#[ignore = "diagnostic; needs SAM weights"]
fn dump_candidates() {
    let models = PathBuf::from(std::env::var("SUBJECT_MODELS").expect("SUBJECT_MODELS"));
    let registry = ModelRegistry::new(models);
    let encoder = registry.get_session("sam-vit-b", None).unwrap();
    let decoder = registry.get_session("sam-vit-b", Some("decoder")).unwrap();
    let image = image::open(std::env::var("SUBJECT_PHOTO").unwrap()).unwrap();
    let emb = ai_processing::generate_image_embeddings(&image, &encoder).unwrap();
    let (w, h) = emb.original_size;
    let click: Vec<f64> = std::env::var("SUBJECT_CLICK").unwrap().split(',').map(|s| s.parse().unwrap()).collect();
    let scale = 1024.0 / w.max(h) as f64;
    let mut coords = vec![(click[0] * scale) as f32, (click[1] * scale) as f32];
    let mut labels = vec![1.0f32];
    if let Ok(ex) = std::env::var("SUBJECT_EXCLUDE") {
        let e: Vec<f64> = ex.split(',').map(|s| s.parse().unwrap()).collect();
        coords.extend([(e[0] * scale) as f32, (e[1] * scale) as f32]); labels.push(0.0);
    }
    coords.extend([0.0, 0.0]); labels.push(-1.0);
    let np = labels.len();
    let mut session = decoder.lock().unwrap();
    let out = session.run(ort::inputs![
        Tensor::from_array(emb.embeddings.clone()).unwrap(),
        Tensor::from_array(Array::from_shape_vec((1, np, 2), coords).unwrap()).unwrap(),
        Tensor::from_array(Array::from_shape_vec((1, np), labels).unwrap()).unwrap(),
        Tensor::from_array(Array::from_shape_vec((1, 1, 256, 256), vec![0.0f32; 65536]).unwrap()).unwrap(),
        Tensor::from_array(Array::from_vec(vec![0.0f32])).unwrap(),
        Tensor::from_array(Array::from_vec(vec![h as f32, w as f32])).unwrap(),
    ]).unwrap();
    let low = out[2].try_extract_array::<f32>().unwrap();
    let scores: Vec<f32> = out[1].try_extract_array::<f32>().unwrap().iter().copied().collect();
    let side = 256usize; let area = side * side;
    let data: Vec<f32> = low.iter().copied().collect();
    let sx = (w as f64 * scale) as usize; let sy = (h as f64 * scale) as usize; // valid region in 1024 space
    let vw = (sx * side / 1024).max(1); let vh = (sy * side / 1024).max(1);
    let outdir = std::env::var("SUBJECT_OUTPUT").ok().map(PathBuf::from);
    if let Some(d) = &outdir { std::fs::create_dir_all(d).unwrap(); }
    println!("valid grid {vw}x{vh} of {side}; click grid ({:.1},{:.1})", click[0]*scale*side as f64/1024.0, click[1]*scale*side as f64/1024.0);
    for (i, m) in data.chunks_exact(area).enumerate() {
        let pos = m.iter().filter(|&&v| v > 0.0).count();
        let stable = m.iter().filter(|&&v| v > 1.0).count();
        let possible = m.iter().filter(|&&v| v > -1.0).count().max(1);
        let cx = ((click[0] * scale * side as f64 / 1024.0) as usize).min(side - 1);
        let cy = ((click[1] * scale * side as f64 / 1024.0) as usize).min(side - 1);
        println!(
            "cand {i}: iou_pred {:.3}  area {:>5} px ({:.1}% of valid)  stable/possible {:.2}  click_logit {:.2}",
            scores[i], pos, 100.0 * pos as f32 / (vw * vh) as f32, stable as f32 / possible as f32, m[cy * side + cx]
        );
        if let Some(d) = &outdir {
            let mut img = image::GrayImage::new(vw as u32, vh as u32);
            for y in 0..vh { for x in 0..vw {
                let p = 1.0 / (1.0 + (-m[y * side + x]).exp());
                img.put_pixel(x as u32, y as u32, image::Luma([(p * 255.0) as u8]));
            }}
            img.save(d.join(format!("cand{i}.png"))).unwrap();
        }
    }
}
