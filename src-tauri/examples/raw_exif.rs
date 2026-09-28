//! Exposure-related EXIF of RAW files, for comparing decodes with Resolve.
fn main() {
    for path in std::env::args().skip(1) {
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let exif = exif::Reader::new().read_from_container(&mut reader);
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        match exif {
            Ok(exif) => {
                let get = |tag: exif::Tag| {
                    exif.get_field(tag, exif::In::PRIMARY)
                        .map(|f| f.display_value().with_unit(&exif).to_string())
                        .unwrap_or_else(|| "-".into())
                };
                println!(
                    "{name}: bias {} | ISO {} | {} {} | mode {} | WB {} | lens {}",
                    get(exif::Tag::ExposureBiasValue),
                    get(exif::Tag::PhotographicSensitivity),
                    get(exif::Tag::ExposureTime),
                    get(exif::Tag::FNumber),
                    get(exif::Tag::ExposureProgram),
                    get(exif::Tag::WhiteBalance),
                    get(exif::Tag::LensModel),
                );
            }
            Err(e) => println!("{name}: {e}"),
        }
    }
}
