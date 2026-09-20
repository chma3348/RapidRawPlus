//! Poster frames and metadata from real video files.
//! `VIDEO_DIR=/path/with/clips VIDEO_OUT=/tmp/out cargo test --test probe_video -- --ignored --nocapture`
use rapidraw_lib::{formats, video};
use std::path::PathBuf;

#[test]
#[ignore = "diagnostic"]
fn poster_frames() {
    let dir = std::env::var("VIDEO_DIR").expect("VIDEO_DIR");
    let out = PathBuf::from(std::env::var("VIDEO_OUT").expect("VIDEO_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| formats::is_video_file(p)).collect();
    entries.sort();
    assert!(!entries.is_empty(), "no videos found in {dir}");
    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let info = video::video_info(&path).unwrap();
        let t = std::time::Instant::now();
        match video::poster_frame(&path) {
            Ok(frame) => {
                let f = out.join(format!("{name}.jpg"));
                frame.to_rgb8().save(&f).unwrap();
                println!("{name:<28} {}x{} {:.1}s {:?}  frame {}x{} in {:.1?}",
                    info.width.unwrap_or(0), info.height.unwrap_or(0),
                    info.duration_seconds.unwrap_or(0.0), info.codecs,
                    frame.width(), frame.height(), t.elapsed());
            }
            Err(e) => println!("{name:<28} NO FRAME: {e}"),
        }
    }
}
