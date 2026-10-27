//! Background image: decoded once, reduced to luminance at CRT resolution.

use png::{ColorType, Decoder, Transformations};
use std::{fs::File, io::BufReader, path::Path};

/// Loads a PNG, scales it to cover `w`x`h` (center-cropped) and returns
/// luminance scaled by `dim` (0..1).
pub fn load(path: &Path, w: usize, h: usize, dim: f32) -> Result<Vec<u8>, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = File::open(path).map_err(|e| err(&e))?;
    let mut decoder = Decoder::new(BufReader::new(file));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| err(&e))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| err(&"image too large"))?];
    let info = reader.next_frame(&mut buf).map_err(|e| err(&e))?;

    let channels = match info.color_type {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(err(&"unexpanded indexed image")),
    };
    let (iw, ih) = (info.width as usize, info.height as usize);
    let luma = |x: usize, y: usize| -> f32 {
        let p = &buf[y * info.line_size + x * channels..];
        if channels < 3 {
            p[0] as f32
        } else {
            0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
        }
    };

    // Cover-fit: scale so the image fills the target, crop the overflow.
    let scale = (iw as f32 / w as f32).min(ih as f32 / h as f32);
    let (ox, oy) = ((iw as f32 - w as f32 * scale) / 2.0, (ih as f32 - h as f32 * scale) / 2.0);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let y0 = (oy + y as f32 * scale) as usize;
        let y1 = ((oy + (y + 1) as f32 * scale) as usize).clamp(y0 + 1, ih);
        for x in 0..w {
            let x0 = (ox + x as f32 * scale) as usize;
            let x1 = ((ox + (x + 1) as f32 * scale) as usize).clamp(x0 + 1, iw);
            // Box filter over the source block.
            let mut sum = 0.0;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    sum += luma(sx, sy);
                }
            }
            let avg = sum / ((y1 - y0) * (x1 - x0)) as f32;
            out[y * w + x] = (avg * dim).clamp(0.0, 255.0) as u8;
        }
    }
    Ok(out)
}
