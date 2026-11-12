//! PNG output shared by every frame source. Called from `video.rs::write_png`.

use std::path::Path;

use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};

/// Scale `image` so its longer side is at most `max_side` (aspect kept), save it
/// as PNG at `path`, and return the path and the saved size.
pub(crate) fn save_png(
    image: RgbaImage,
    max_side: u32,
    path: &Path,
) -> Result<(String, u32, u32), String> {
    let (w, h) = image.dimensions();
    let longest = w.max(h);
    let image = if max_side > 0 && longest > max_side {
        let scale = f64::from(max_side) / f64::from(longest);
        let nw = ((f64::from(w) * scale).round() as u32).max(1);
        let nh = ((f64::from(h) * scale).round() as u32).max(1);
        image::imageops::resize(&image, nw, nh, FilterType::Triangle)
    } else {
        image
    };
    let (w, h) = image.dimensions();
    image
        .save_with_format(path, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok((path.to_string_lossy().into_owned(), w, h))
}
