fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let photo = image::open(&args[1])?;
    let plate = image::open(&args[2])?;
    let (w, h) = (photo.width(), photo.height());
    let alpha = image::GrayImage::from_fn(w, h, |_, y| image::Luma([if y < h / 2 { 255 } else { 0 }]));
    for (label, o) in [
        ("as shot", rapidraw_lib::sky_replace::SkyReplaceOptions::as_shot()),
        ("auto match", rapidraw_lib::sky_replace::SkyReplaceOptions::auto_match()),
    ] {
        let t = std::time::Instant::now();
        let out = rapidraw_lib::sky_replace::replace_sky(&photo, &alpha, &plate, &o)?;
        println!("{label:12} {w}x{h}  {:>6.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        drop(out);
    }
    let small = photo.thumbnail(960, 960);
    let sa = image::imageops::resize(&alpha, small.width(), small.height(), image::imageops::FilterType::Triangle);
    let t = std::time::Instant::now();
    rapidraw_lib::sky_replace::replace_sky(&small, &sa, &plate, &rapidraw_lib::sky_replace::SkyReplaceOptions::auto_match())?;
    println!("preview      {}x{}  {:>6.0} ms", small.width(), small.height(), t.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}
