//! Loader for Linux console fonts (PSF1/PSF2, optionally gzipped).

use std::{collections::HashMap, fs, io::Read, path::Path};

pub struct Font {
    pub width: usize,
    pub height: usize,
    bytes_per_row: usize,
    bytes_per_glyph: usize,
    count: usize,
    glyphs: Vec<u8>,
    map: HashMap<char, usize>,
}

impl Font {
    pub fn load(path: &Path) -> Result<Font, String> {
        let raw = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let data = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&raw[..])
                .read_to_end(&mut out)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            out
        } else {
            raw
        };

        if data.starts_with(&[0x36, 0x04]) {
            parse_psf1(&data)
        } else if data.starts_with(&[0x72, 0xb5, 0x4a, 0x86]) {
            parse_psf2(&data)
        } else {
            Err(format!("{}: not a PSF font", path.display()))
        }
        .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Returns the bitmap rows for `c`, falling back to '?'.
    pub fn glyph(&self, c: char) -> &[u8] {
        let idx = self
            .map
            .get(&c)
            .or_else(|| self.map.get(&'?'))
            .copied()
            .unwrap_or(0);
        let start = idx * self.bytes_per_glyph;
        &self.glyphs[start..start + self.bytes_per_glyph]
    }

    pub fn pixel(&self, glyph: &[u8], x: usize, y: usize) -> bool {
        glyph[y * self.bytes_per_row + x / 8] & (0x80 >> (x % 8)) != 0
    }

    fn identity_map(&mut self) {
        for i in 0..self.count.min(256) {
            self.map.insert(char::from(i as u8), i);
        }
    }
}

fn parse_psf1(d: &[u8]) -> Result<Font, String> {
    let mode = d[2];
    let height = d[3] as usize;
    let count = if mode & 1 != 0 { 512 } else { 256 };
    let end = 4 + count * height;
    if d.len() < end {
        return Err("truncated glyph data".into());
    }
    let mut font = Font {
        width: 8,
        height,
        bytes_per_row: 1,
        bytes_per_glyph: height,
        count,
        glyphs: d[4..end].to_vec(),
        map: HashMap::new(),
    };

    if mode & 0x06 != 0 {
        // Unicode table: per glyph, u16 LE codepoints, 0xFFFE starts a
        // combining sequence (skipped), 0xFFFF ends the entry.
        let mut glyph = 0;
        let mut in_seq = false;
        for pair in d[end..].chunks_exact(2) {
            match u16::from_le_bytes([pair[0], pair[1]]) {
                0xFFFF => {
                    glyph += 1;
                    in_seq = false;
                }
                0xFFFE => in_seq = true,
                cp if !in_seq => {
                    if let Some(c) = char::from_u32(cp as u32) {
                        font.map.entry(c).or_insert(glyph);
                    }
                }
                _ => {}
            }
        }
    } else {
        font.identity_map();
    }
    Ok(font)
}

fn parse_psf2(d: &[u8]) -> Result<Font, String> {
    if d.len() < 32 {
        return Err("truncated header".into());
    }
    let u = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap()) as usize;
    let (header, flags, count, charsize, height, width) = (u(8), u(12), u(16), u(20), u(24), u(28));
    let end = header + count * charsize;
    if d.len() < end {
        return Err("truncated glyph data".into());
    }
    let mut font = Font {
        width,
        height,
        bytes_per_row: width.div_ceil(8),
        bytes_per_glyph: charsize,
        count,
        glyphs: d[header..end].to_vec(),
        map: HashMap::new(),
    };

    if flags & 1 != 0 {
        // Unicode table: per glyph, UTF-8 strings, 0xFE starts a sequence
        // (skipped), 0xFF ends the entry.
        for (glyph, entry) in d[end..].split(|&b| b == 0xFF).enumerate() {
            let singles = entry.split(|&b| b == 0xFE).next().unwrap_or(&[]);
            for c in String::from_utf8_lossy(singles).chars() {
                font.map.entry(c).or_insert(glyph);
            }
        }
    } else {
        font.identity_map();
    }
    Ok(font)
}
