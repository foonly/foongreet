//! Offscreen output for capturing the look without a display.

use super::Output;
use std::{fs::File, io, io::BufWriter, path::Path};

pub struct Snapshot {
    buf: Vec<u32>,
    width: usize,
    height: usize,
}

impl Snapshot {
    pub fn new(width: usize, height: usize) -> Snapshot {
        Snapshot { buf: vec![0; width * height], width, height }
    }
}

impl Output for Snapshot {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()> {
        draw(&mut self.buf, self.width);
        Ok(())
    }
}

pub fn save(path: &Path, px: &[u32], w: usize, h: usize) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = File::create(path).map_err(|e| err(&e))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    let mut writer = enc.write_header().map_err(|e| err(&e))?;
    let rgb: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    writer.write_image_data(&rgb).map_err(|e| err(&e))
}
