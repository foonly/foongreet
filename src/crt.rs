//! Software CRT emulation.
//!
//! The UI draws a low-resolution luminance image. Each frame it goes through
//! phosphor persistence, glow and noise at low resolution, and is then mapped
//! onto the output through a precomputed table holding the barrel distortion,
//! scanline beam profile and vignette. The final intensity is colorized
//! through a phosphor palette.

use rayon::prelude::*;
use serde::Deserialize;

#[derive(Deserialize, Clone, Copy, Default, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Phosphor {
    /// P3-style amber: warm, longer afterglow.
    #[default]
    Amber,
    /// P1-style green: cooler, quicker to fade.
    Green,
}

impl Phosphor {
    /// Saturated color at mid brightness and the pale color of an
    /// overdriven core.
    fn colors(self) -> ([f32; 3], [f32; 3]) {
        match self {
            Phosphor::Amber => ([1.0, 0.69, 0.0], [1.0, 0.92, 0.7]),
            Phosphor::Green => ([0.2, 1.0, 0.25], [0.78, 1.0, 0.8]),
        }
    }

    /// Persistence: fraction of the previous frame kept (of 256).
    fn decay(self) -> u32 {
        match self {
            Phosphor::Amber => 200,
            Phosphor::Green => 170,
        }
    }
}
/// Glow strength (of 256) and blur radius in low-res pixels.
const GLOW: u32 = 170;
const GLOW_RADIUS: usize = 3;
/// Dark-tube base level so the glass never reads as pure black.
const BASE: u32 = 7;
/// Barrel curvature applied along each axis.
const CURVE_X: f32 = 0.035;
const CURVE_Y: f32 = 0.05;

const OUTSIDE: u32 = u32::MAX;
const IDX_BITS: u32 = 20;
const IDX_MASK: u32 = (1 << IDX_BITS) - 1;

pub struct Crt {
    pub lw: usize,
    pub lh: usize,
    ow: usize,
    oh: usize,
    /// Per output pixel: low-res index (20 bits), sharp weight (6 bits),
    /// glow weight (6 bits). `OUTSIDE` for pixels off the tube.
    map: Vec<u32>,
    phos: Vec<u16>,
    sharp: Vec<u8>,
    glow: Vec<u8>,
    tmp: Vec<u16>,
    palette: [u32; 256],
    decay: u32,
    rng: u32,
}

/// Low-res target height; the scale factor is picked to land near it.
const TARGET_LINES: usize = 480;

impl Crt {
    pub fn new(ow: usize, oh: usize, phosphor: Phosphor) -> Crt {
        let mut scale = ((oh as f32 / TARGET_LINES as f32).round() as usize).max(1);
        while (ow / scale) * (oh / scale) > IDX_MASK as usize {
            scale += 1;
        }
        let (lw, lh) = (ow / scale, oh / scale);
        let n = lw * lh;
        Crt {
            lw,
            lh,
            ow,
            oh,
            map: build_map(ow, oh, lw, lh, scale),
            phos: vec![0; n],
            sharp: vec![0; n],
            glow: vec![0; n],
            tmp: vec![0; n],
            palette: palette(phosphor),
            decay: phosphor.decay(),
            rng: 0x9e3779b9,
        }
    }

    fn rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Renders `lum` (lw*lh) into `out` (stride in pixels). `t` is seconds
    /// since start; `collapse` runs 0..1 for the power-off animation.
    pub fn render(&mut self, lum: &[u8], t: f32, collapse: f32, out: &mut [u32], stride: usize) {
        let (lw, lh) = (self.lw, self.lh);

        // Flicker and a slow rolling bright bar.
        let flicker = 244 + (self.rand() % 12);
        let bar_y = ((t * 0.07).fract() * 1.4 - 0.2) * lh as f32;
        let bar_h = lh as f32 * 0.08;

        let mut seed = self.rand() | 1;
        for y in 0..lh {
            let d = (y as f32 - bar_y) / bar_h;
            let bar = if d.abs() < 1.0 { ((1.0 - d * d) * 6.0) as u32 } else { 0 };
            let row = y * lw;
            for i in row..row + lw {
                let v = (lum[i] as u32) << 8;
                let p = (self.phos[i] as u32 * self.decay) >> 8;
                let p = v.max(p);
                self.phos[i] = p as u16;
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let s = ((p >> 8) * flicker >> 8) + bar + (seed & 3);
                self.sharp[i] = s.min(255) as u8;
            }
        }

        box_blur(&self.sharp, &mut self.tmp, &mut self.glow, lw, lh, GLOW_RADIUS);

        // Power-off: squash vertically first, then horizontally to a dot.
        let sy = (1.0 - collapse * 1.6).max(0.004);
        let sx = if collapse < 0.6 { 1.0 } else { (1.0 - (collapse - 0.6) * 2.5).max(0.002) };
        let boost = if collapse > 0.0 { (1.0 / (sx * sy).sqrt()).min(3.0) } else { 1.0 };
        let fade = if collapse > 0.9 { (1.0 - collapse) * 10.0 } else { 1.0 };
        let boost = (boost * fade * 256.0) as u32;

        let (ow, oh) = (self.ow, self.oh);
        let (map, sharp, glow, pal) = (&self.map, &self.sharp, &self.glow, &self.palette);
        out.par_chunks_mut(stride).take(oh).enumerate().for_each(|(y, line)| {
            let line = &mut line[..ow];
            if collapse > 0.0 {
                let ys = oh as f32 / 2.0 + (y as f32 - oh as f32 / 2.0) / sy;
                if ys < 0.0 || ys >= oh as f32 {
                    line.fill(0);
                    return;
                }
                let src = &map[ys as usize * ow..][..ow];
                for (x, px) in line.iter_mut().enumerate() {
                    let xs = ow as f32 / 2.0 + (x as f32 - ow as f32 / 2.0) / sx;
                    *px = if xs < 0.0 || xs >= ow as f32 {
                        0
                    } else {
                        let v = shade(src[xs as usize], sharp, glow);
                        pal[((v * boost) >> 8).min(255) as usize]
                    };
                }
            } else {
                let src = &map[y * ow..][..ow];
                for (px, &m) in line.iter_mut().zip(src) {
                    *px = if m == OUTSIDE { 0 } else { pal[shade(m, sharp, glow) as usize] };
                }
            }
        });
    }
}

#[inline]
fn shade(m: u32, sharp: &[u8], glow: &[u8]) -> u32 {
    if m == OUTSIDE {
        return 0;
    }
    let idx = (m & IDX_MASK) as usize;
    let ws = (m >> IDX_BITS) & 63;
    let wg = m >> (IDX_BITS + 6);
    let s = sharp[idx] as u32 * ws;
    let g = (glow[idx] as u32 * GLOW >> 8) * wg;
    ((s + g + BASE * wg) >> 6).min(255)
}

fn build_map(ow: usize, oh: usize, lw: usize, lh: usize, scale: usize) -> Vec<u32> {
    let aspect = ow as f32 / oh as f32;
    let mut map = vec![OUTSIDE; ow * oh];
    map.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5) / oh as f32 * 2.0 - 1.0;
        for (x, m) in row.iter_mut().enumerate() {
            let u = (x as f32 + 0.5) / ow as f32 * 2.0 - 1.0;
            let cu = u * (1.0 + CURVE_X * v * v);
            let cv = v * (1.0 + CURVE_Y * u * u);
            if cu.abs() >= 1.0 || cv.abs() >= 1.0 {
                continue;
            }
            let (ux, uy) = ((cu + 1.0) / 2.0, (cv + 1.0) / 2.0);
            let lx = ux * lw as f32;
            let ly = uy * lh as f32;

            // Gaussian beam profile across each scanline.
            let scan = if scale >= 2 {
                let f = ly.fract() - 0.5;
                0.3 + 0.7 * (-f * f / (2.0 * 0.2 * 0.2)).exp()
            } else {
                1.0
            };

            // Vignette plus a soft, slightly rounded tube edge.
            let vig = (16.0 * ux * (1.0 - ux) * uy * (1.0 - uy)).powf(0.22);
            let ex = ux.min(1.0 - ux) * aspect;
            let ey = uy.min(1.0 - uy);
            let corner = 0.03;
            let edge = if ex < corner && ey < corner {
                corner - ((corner - ex).powi(2) + (corner - ey).powi(2)).sqrt()
            } else {
                ex.min(ey)
            };
            let vig = vig * smoothstep(0.0, 0.006, edge);

            let idx = (ly as usize).min(lh - 1) * lw + (lx as usize).min(lw - 1);
            let ws = (scan * vig * 63.0).round() as u32;
            let wg = (vig * 63.0).round() as u32;
            *m = idx as u32 | ws << IDX_BITS | wg << (IDX_BITS + 6);
        }
    });
    map
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Separable box blur, applied twice for a roughly gaussian falloff.
fn box_blur(src: &[u8], tmp: &mut [u16], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    let mut cur: Vec<u8> = src.to_vec();
    for _ in 0..2 {
        for y in 0..h {
            let row = &cur[y * w..][..w];
            let mut acc: u32 = row[0] as u32 * (r as u32 + 1) + row[1..=r].iter().map(|&v| v as u32).sum::<u32>();
            for x in 0..w {
                tmp[y * w + x] = (acc / div) as u16;
                let add = row[(x + r + 1).min(w - 1)] as u32;
                let sub = row[x.saturating_sub(r)] as u32;
                acc = acc + add - sub;
            }
        }
        for x in 0..w {
            let at = |y: usize| tmp[y * w + x] as u32;
            let mut acc: u32 = at(0) * (r as u32 + 1) + (1..=r).map(at).sum::<u32>();
            for y in 0..h {
                dst[y * w + x] = (acc / div) as u8;
                acc = acc + at((y + r + 1).min(h - 1)) - at(y.saturating_sub(r));
            }
        }
        cur.copy_from_slice(dst);
    }
}

/// Dark lows, saturated mids, and a pale core where the phosphor saturates.
fn palette(phosphor: Phosphor) -> [u32; 256] {
    let (base, hot) = phosphor.colors();
    let mut pal = [0u32; 256];
    for (i, p) in pal.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let k = t.powf(1.4);
        let h = smoothstep(0.7, 1.0, t);
        let c: Vec<u32> = (0..3)
            .map(|j| ((base[j] * k * (1.0 - h) + hot[j] * h) * 255.0).round().clamp(0.0, 255.0) as u32)
            .collect();
        *p = c[0] << 16 | c[1] << 8 | c[2];
    }
    pal
}
