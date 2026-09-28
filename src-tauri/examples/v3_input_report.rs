//! Print v3's interpretation of a photo as the app would decode it, with the
//! installed transforms: which path it took and why.
//!   cargo run --release --example v3_input_report -- PHOTO...
use anyhow::Result;
fn main() -> Result<()> {
    let state = rapidraw_lib::AppState::default();
    let support = std::path::PathBuf::from(std::env::var("HOME")?)
        .join("Library/Application Support/io.github.CyberTimon.RapidRAW");
    for (slot, name) in [
        (&state.output_transform, "output-transform.cube"),
        (&state.input_transform, "input-transform.cube"),
    ] {
        let path = support.join(name);
        if path.exists() {
            *slot.lock().unwrap() = Some(path);
        }
    }
    for path in std::env::args().skip(1) {
        let start = std::time::Instant::now();
        let report = rapidraw_lib::color_engine::application::input_report(
            &state,
            &path,
            &serde_json::json!({"processVersion": 3}),
        )?;
        println!(
            "{}: {} | {} | {}x{} | {:.0} ms",
            std::path::Path::new(&path).file_name().unwrap().to_string_lossy(),
            report["provenance"]["transform_decision"],
            report["provenance"]["interpretation"],
            report["width"],
            report["height"],
            start.elapsed().as_secs_f64() * 1000.
        );
        for w in report["provenance"]["warnings"].as_array().unwrap_or(&vec![]) {
            println!("    {}", w.as_str().unwrap_or(""));
        }
    }
    Ok(())
}
