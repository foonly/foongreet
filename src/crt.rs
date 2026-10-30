//! Software CRT emulation.
//!
//! The UI draws a low-resolution luminance image. Each frame it goes through
//! phosphor persistence, glow and noise at low resolution, and is then mapped
//! onto the output through a precomputed table holding the barrel distortion,
//! scanline beam profile and vignette. Horizontally each output pixel blends
//! the two nearest low-res pixels, so strokes keep an even width under the
//! curvature. The final intensity is colorized through a phosphor palette.

use rayon::prelude::*;
use serde::Deserialize;
use std::time::Instant;

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

    /// Persistence: fraction kept per `DECAY_FRAME` (of 256).
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
/// How steeply the horizontal blend goes from one low-res pixel to the
/// next: 1.0 is plain linear, higher is crisper.
const SHARPEN: f32 = 2.0;
/// Barrel curvature applied along each axis.
const CURVE_X: f32 = 0.035;
const CURVE_Y: f32 = 0.05;

/// Where an output pixel samples the low-res image. Pixels off the tube
/// have zero weights.
#[derive(Clone, Copy, Default)]
struct Sample {
    /// Left of the two horizontally blended low-res pixels.
    idx: u32,
    /// Share of the right-hand pixel, of 256.
    frac: u8,
    /// Weight of the sharp image (scanline profile and vignette), of 255.
    sharp: u8,
    /// Weight of the glow and base level (vignette only), of 255.
    glow: u8,
}

/// The `Sample`s of all output pixels, one array per field, so that
/// `shade_line` can load the weights of a block as whole vectors.
struct Map {
    idx: Vec<u32>,
    frac: Vec<u8>,
    sharp: Vec<u8>,
    glow: Vec<u8>,
}

impl Map {
    fn new(samples: Vec<Sample>) -> Map {
        Map {
            idx: samples.iter().map(|m| m.idx).collect(),
            frac: samples.iter().map(|m| m.frac).collect(),
            sharp: samples.iter().map(|m| m.sharp).collect(),
            glow: samples.iter().map(|m| m.glow).collect(),
        }
    }

    fn get(&self, i: usize) -> Sample {
        Sample { idx: self.idx[i], frac: self.frac[i], sharp: self.sharp[i], glow: self.glow[i] }
    }
}

pub struct Crt {
    pub lw: usize,
    pub lh: usize,
    ow: usize,
    oh: usize,
    /// One entry per output pixel.
    map: Map,
    phos: Vec<u16>,
    sharp: Vec<u8>,
    glow: Vec<u8>,
    tmp: Vec<u16>,
    /// Per low-res pixel: its sharp and glow values and those of its
    /// right-hand neighbour, packed so `shade` needs a single load.
    pairs: Vec<u32>,
    noise: Vec<u8>,
    palette: [u32; 256],
    decay: u32,
    rng: u32,
    /// When the last frame was rendered, for the afterglow.
    last: Option<Instant>,
    /// Freezes flicker and noise and hides the rolling bar, so frames only
    /// differ where `lum` does.
    pub still: bool,
}

/// The phosphor decay rate is given per this many seconds; frames further
/// apart fade further, so the afterglow lasts as long at any frame rate.
const DECAY_FRAME: f32 = 1.0 / 30.0;

/// Low-res target height; the scale factor is picked to land near it.
const TARGET_LINES: usize = 480;
/// Render threads. Each one past the first mostly adds overhead (waking
/// and spinning) rather than speed, as frames are cheap: per 2560x1440
/// frame, 9.0 ms of CPU on one thread, 9.4 ms on two (5.7 ms wall), 13 ms
/// on four. Two keep even 4K under 10 ms of wall time.
const MAX_THREADS: usize = 2;

/// Size of the noise texture: random values 0..=3, added to the picture.
const NOISE_LEN: usize = 1 << 16;

/// Rolling bar height, as a fraction of the screen.
const BAR_HEIGHT: f32 = 0.08;

/// Rolling bar center at time `t`, as a fraction of the screen height
/// (0 = top). It spends part of each cycle off screen.
fn bar_pos(t: f32) -> f32 {
    (t * 0.07).fract() * 1.4 - 0.2
}

/// Whether the rolling bar shows on screen at time `t`.
pub fn bar_visible(t: f32) -> bool {
    let y = bar_pos(t);
    y > -BAR_HEIGHT && y < 1.0 + BAR_HEIGHT
}

fn noise_texture() -> Vec<u8> {
    let mut x: u32 = 0x9e37_79b9;
    (0..NOISE_LEN)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 30) as u8
        })
        .collect()
}

/// Sizes the global thread pool `render` uses. Call before rendering.
pub fn init_threads() {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS);
    let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
}

impl Crt {
    pub fn new(ow: usize, oh: usize, phosphor: Phosphor) -> Crt {
        let scale = ((oh as f32 / TARGET_LINES as f32).round() as usize).max(1);
        let (lw, lh) = (ow / scale, oh / scale);
        let n = lw * lh;
        Crt {
            lw,
            lh,
            ow,
            oh,
            map: Map::new(build_map(ow, oh, lw, lh, scale)),
            phos: vec![0; n],
            sharp: vec![0; n],
            glow: vec![0; n],
            tmp: vec![0; n],
            pairs: vec![0; n],
            noise: noise_texture(),
            palette: palette(phosphor),
            decay: phosphor.decay(),
            rng: 0x9e3779b9,
            last: None,
            still: false,
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
    /// since start; `collapse` runs 0..1 for the power-off animation and
    /// back from 1 to 0 for power-on. At 1 the screen is black.
    pub fn render(&mut self, lum: &[u8], t: f32, collapse: f32, out: &mut [u32], stride: usize) {
        let (lw, lh) = (self.lw, self.lh);

        let dt = self.last.map_or(DECAY_FRAME, |l| l.elapsed().as_secs_f32());
        self.last = Some(Instant::now());
        // Of 256, at most 255 so that decay << 8 fits a u16.
        let decay = ((self.decay as f32 / 256.0).powf(dt / DECAY_FRAME) * 256.0).min(255.0) as u16;

        // Flicker and a slow rolling bright bar.
        let (flicker, mut seed) =
            if self.still { (250, 0x2545_f491) } else { (244 + (self.rand() % 12) as u16, self.rand() | 1) };
        let bar_y = if self.still { -1.0 } else { bar_pos(t) } * lh as f32;
        let bar_h = lh as f32 * BAR_HEIGHT;

        // All in u16 so it vectorizes; the noise comes from the texture at
        // a random offset per row.
        for y in 0..lh {
            let d = (y as f32 - bar_y) / bar_h;
            let bar = if d.abs() < 1.0 { ((1.0 - d * d) * 6.0) as u16 } else { 0 };
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let off = seed as usize % (NOISE_LEN - lw);
            let row = y * lw..(y + 1) * lw;
            let (lum, phos, sharp) = (&lum[row.clone()], &mut self.phos[row.clone()], &mut self.sharp[row]);
            for (((&l, p), s), &n) in lum.iter().zip(phos).zip(sharp).zip(&self.noise[off..off + lw]) {
                // (p * decay) >> 8, as a high multiply.
                let kept = ((*p as u32 * ((decay << 8) as u32)) >> 16) as u16;
                *p = ((l as u16) << 8).max(kept);
                *s = (((*p >> 8) * flicker >> 8) + bar + n as u16).min(255) as u8;
            }
        }

        box_blur(&self.sharp, &mut self.tmp, &mut self.glow, &mut self.pairs, lw, lh);

        // Power-off: squash vertically first, then horizontally to a dot.
        let sy = (1.0 - collapse * 1.6).max(0.004);
        let sx = if collapse < 0.6 { 1.0 } else { (1.0 - (collapse - 0.6) * 2.5).max(0.002) };
        let boost = if collapse > 0.0 { (1.0 / (sx * sy).sqrt()).min(3.0) } else { 1.0 };
        let fade = if collapse > 0.9 { (1.0 - collapse) * 10.0 } else { 1.0 };
        let boost = (boost * fade * 256.0) as u32;
        // The squashed beam glows hot regardless of the picture.
        let flash = ((1.0 - sy) * 200.0 * fade) as u32;

        let (ow, oh) = (self.ow, self.oh);
        let (map, pairs, pal) = (&self.map, &self.pairs, &self.palette);
        out.par_chunks_mut(stride).take(oh).enumerate().for_each(|(y, line)| {
            let line = &mut line[..ow];
            if collapse > 0.0 {
                let ys = oh as f32 / 2.0 + (y as f32 - oh as f32 / 2.0) / sy;
                if ys < 0.0 || ys >= oh as f32 {
                    line.fill(0);
                    return;
                }
                let row = ys as usize * ow;
                for (x, px) in line.iter_mut().enumerate() {
                    let xs = ow as f32 / 2.0 + (x as f32 - ow as f32 / 2.0) / sx;
                    *px = if xs < 0.0 || xs >= ow as f32 {
                        0
                    } else {
                        let m = map.get(row + xs as usize);
                        let v = (shade(m, pairs) * boost >> 8) + (flash * m.glow as u32 / 255);
                        pal[v.min(255) as u8 as usize]
                    };
                }
            } else {
                shade_line(line, map, y * ow, pairs, pal);
            }
        });
    }
}

#[inline]
fn shade(m: Sample, pairs: &[u32]) -> u32 {
    let p = pairs[m.idx as usize];
    let f = m.frac as u32;
    let blend = |a: u32, b: u32| (a * (256 - f) + b * f) >> 8;
    let s = blend(p & 0xff, p >> 16 & 0xff) * m.sharp as u32;
    let g = (blend(p >> 8 & 0xff, p >> 24) * GLOW >> 8) * m.glow as u32;
    ((s + g + BASE * m.glow as u32) / 255).min(255)
}

/// Pixels per block in `shade_line`.
const LANES: usize = 16;

/// `shade` for the output line starting at map index `start`, in blocks:
/// only the loads that depend on the map (`pairs[idx]`, the palette) are
/// done one by one, everything in between on whole blocks, so it compiles
/// to SIMD (even plain SSE2). Same results as `shade`.
fn shade_line(line: &mut [u32], map: &Map, start: usize, pairs: &[u32], pal: &[u32; 256]) {
    let n = line.len();
    let blocks = n / LANES * LANES;
    for x in (0..blocks).step_by(LANES) {
        let i = start + x;
        let block = |v: &[u8]| -> [u8; LANES] { v[i..i + LANES].try_into().unwrap() };
        let idx: &[u32; LANES] = map.idx[i..i + LANES].try_into().unwrap();
        let p: [u32; LANES] = std::array::from_fn(|k| pairs[idx[k] as usize]);
        let v = shade_block(&p, &block(&map.frac), &block(&map.sharp), &block(&map.glow));
        for (px, &v) in line[x..x + LANES].iter_mut().zip(&v) {
            *px = pal[v as usize];
        }
    }
    for x in blocks..n {
        line[x] = pal[shade(map.get(start + x), pairs) as u8 as usize];
    }
}

/// The arithmetic of `shade` for a block, all in u16, which SSE2 can
/// multiply: each product stays below 65536 (255 * 256 at most), and the
/// sum saturates instead, as it is capped at 255 * 255 anyway.
#[inline(always)]
fn shade_block(p: &[u32; LANES], f: &[u8; LANES], ws: &[u8; LANES], wg: &[u8; LANES]) -> [u8; LANES] {
    std::array::from_fn(|k| {
        let byte = |shift: u32| (p[k] >> shift & 0xff) as u16;
        let (s0, g0, s1, g1) = (byte(0), byte(8), byte(16), byte(24));
        let (f, ws, wg) = (f[k] as u16, ws[k] as u16, wg[k] as u16);
        let s = ((s0 * (256 - f) + s1 * f) >> 8) * ws;
        let g = (((g0 * (256 - f) + g1 * f) >> 8) * GLOW as u16 >> 8) * wg;
        let x = s.saturating_add(g).saturating_add(BASE as u16 * wg).min(255 * 255);
        // x / 255, exact for x < 65535.
        ((x + 1 + (x >> 8)) >> 8) as u8
    })
}

/// Packs sharp and glow of each pixel and its right-hand neighbour as
/// sharp, glow, next sharp, next glow (low byte first). The last pixel of
/// a row has no neighbour; the map never blends past it.
fn pack_row(sharp: &[u8], glow: &[u8], pairs: &mut [u32]) {
    let w = pairs.len();
    let pack = |s0: u8, g0: u8, s1: u8, g1: u8| u32::from_le_bytes([s0, g0, s1, g1]);
    for x in 0..w - 1 {
        pairs[x] = pack(sharp[x], glow[x], sharp[x + 1], glow[x + 1]);
    }
    pairs[w - 1] = pack(sharp[w - 1], glow[w - 1], 0, 0);
}

fn build_map(ow: usize, oh: usize, lw: usize, lh: usize, scale: usize) -> Vec<Sample> {
    let aspect = ow as f32 / oh as f32;
    let mut map = vec![Sample::default(); ow * oh];
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

            // Blend between the two low-res pixel centers around lx.
            let px = (lx - 0.5).clamp(0.0, (lw - 1) as f32);
            let x0 = (px as usize).min(lw.saturating_sub(2));
            // Steepened around the midpoint ("sharp bilinear"): crisp inside
            // a pixel, blended only near its edges.
            let f = ((px - x0 as f32 - 0.5) * SHARPEN + 0.5).clamp(0.0, 1.0);
            let frac = (f * 256.0).round().min(255.0) as u8;

            *m = Sample {
                idx: ((ly as usize).min(lh - 1) * lw + x0) as u32,
                frac,
                sharp: (scan * vig * 255.0).round() as u8,
                glow: (vig * 255.0).round() as u8,
            };
        }
    });
    map
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Separable box blur, applied twice for a roughly gaussian falloff, and
/// packs `pairs` (see `Crt::pairs`) from `src` and the result while each
/// row is still in cache. Both passes run row by row; the vertical one
/// keeps a running sum per column, so it never walks memory column-wise.
fn box_blur(src: &[u8], tmp: &mut [u16], dst: &mut [u8], pairs: &mut [u32], w: usize, h: usize) {
    // A constant radius turns the divisions into multiplies, which (unlike
    // divisions) vectorize.
    const R: usize = GLOW_RADIUS;
    let (r, div) = (R, (2 * R + 1) as u32);
    let mut acc = vec![0u32; w];
    // Prefix sums of a row with its edge pixels repeated r times on each
    // side; a window sum is then the difference of two of them.
    let mut prefix = vec![0u32; w + 2 * r + 1];
    for pass in 0..2 {
        let input: &[u8] = if pass == 0 { src } else { dst };
        for y in 0..h {
            let row = &input[y * w..][..w];
            let (head, rest) = prefix[1..].split_at_mut(r);
            let (body, tail) = rest.split_at_mut(w);
            let mut sum = 0;
            for p in head {
                sum += row[0] as u32;
                *p = sum;
            }
            for (p, &v) in body.iter_mut().zip(row) {
                sum += v as u32;
                *p = sum;
            }
            for p in tail {
                sum += row[w - 1] as u32;
                *p = sum;
            }
            let out = &mut tmp[y * w..][..w];
            for ((o, hi), lo) in out.iter_mut().zip(&prefix[2 * r + 1..]).zip(&prefix[..w]) {
                *o = ((hi - lo) / div) as u16;
            }
        }
        let row = |y: usize| &tmp[y * w..][..w];
        for (x, a) in acc.iter_mut().enumerate() {
            *a = row(0)[x] as u32 * (r as u32 + 1);
        }
        for y in 1..=r {
            for (a, &v) in acc.iter_mut().zip(row(y)) {
                *a += v as u32;
            }
        }
        for y in 0..h {
            let (add, sub) = (row((y + r + 1).min(h - 1)), row(y.saturating_sub(r)));
            let out = &mut dst[y * w..][..w];
            for x in 0..w {
                out[x] = (acc[x] / div) as u8;
                acc[x] = acc[x] + add[x] as u32 - sub[x] as u32;
            }
            if pass == 1 {
                pack_row(&src[y * w..][..w], out, &mut pairs[y * w..][..w]);
            }
        }
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
