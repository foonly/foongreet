//! Drawing text and boxes into the low-res luminance buffer, plus the boot
//! header and clock both screens show.

use crate::font::Font;

// Intensities on the luminance buffer.
pub const DIM: u8 = 110;
pub const NORMAL: u8 = 190;
pub const BRIGHT: u8 = 255;

/// Characters per second the boot header types out at.
pub const TYPE_RATE: f32 = 90.0;

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub w: usize,
    pub h: usize,
    pub font: &'a Font,
}

impl Canvas<'_> {
    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
        for yy in y.min(self.h)..(y + h).min(self.h) {
            self.buf[yy * self.w + x.min(self.w)..yy * self.w + (x + w).min(self.w)].fill(v);
        }
    }

    pub fn text(&mut self, x: usize, y: usize, s: &str, v: u8) {
        let (fw, fh) = (self.font.width, self.font.height);
        for (i, c) in s.chars().enumerate() {
            let g = self.font.glyph(c);
            let gx = x + i * fw;
            for row in 0..fh {
                let py = y + row;
                if py >= self.h {
                    break;
                }
                for col in 0..fw {
                    let px = gx + col;
                    if px < self.w && self.font.pixel(g, col, row) {
                        self.buf[py * self.w + px] = v;
                    }
                }
            }
        }
    }

    /// Double-line box in character cells with a centered title.
    pub fn frame(&mut self, col: usize, row: usize, w: usize, h: usize, title: &str, v: u8) {
        let (cw, ch) = (self.font.width, self.font.height);
        let mut top: Vec<char> = std::iter::once('╔').chain(std::iter::repeat_n('═', w - 2)).chain(['╗']).collect();
        let start = (w - title.chars().count()) / 2;
        for (i, c) in title.chars().enumerate() {
            top[start + i] = c;
        }
        let top: String = top.into_iter().collect();
        let bottom: String = std::iter::once('╚').chain(std::iter::repeat_n('═', w - 2)).chain(['╝']).collect();
        self.text(col * cw, row * ch, &top, v);
        self.text(col * cw, (row + h - 1) * ch, &bottom, v);
        for r in row + 1..row + h - 1 {
            self.text(col * cw, r * ch, "║", v);
            self.text((col + w - 1) * cw, r * ch, "║", v);
        }
    }

    /// The boot header typed out over the first seconds, with the clock in
    /// the top right. Returns whether the header has finished typing.
    pub fn header(&mut self, lines: &[String], t: f32, demo: bool) -> bool {
        let (cw, ch) = (self.font.width, self.font.height);
        let mut budget = (t * TYPE_RATE) as usize;
        for (i, line) in lines.iter().enumerate() {
            let n = line.chars().count().min(budget);
            budget -= n;
            let shown: String = line.chars().take(n).collect();
            self.text(2 * cw, (1 + i) * ch, &shown, if i == 0 { BRIGHT } else { NORMAL });
        }

        let clock = if demo { "1986-10-27 09:41:00".to_string() } else { clock() };
        let cols = self.w / cw;
        self.text((cols.saturating_sub(clock.len() + 2)) * cw, ch, &clock, NORMAL);
        budget > 0
    }
}

/// Boot header lines. `line` names where we run, e.g. "TTY1".
pub fn header_lines(demo: bool, line: &str) -> Vec<String> {
    let (mem_kb, kernel, host, line) = if demo {
        ("640".into(), "6.12.0".into(), "MAINFRAME".into(), "TTY1".into())
    } else {
        let read = |path| std::fs::read_to_string(path).unwrap_or_default().trim().to_string();
        let mem_kb = read("/proc/meminfo")
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("640")
            .to_string();
        (mem_kb, read("/proc/sys/kernel/osrelease"), read("/proc/sys/kernel/hostname"), line.to_string())
    };
    vec![
        "FOONGREET TERMINAL SYSTEM  REV 0.1".into(),
        "(C) 1986-2026 FOONLY MICROSYSTEMS".into(),
        String::new(),
        format!("MEMORY TEST ... {mem_kb}K OK"),
        format!("KERNEL ........ {}", kernel.to_uppercase()),
        format!("NODE .......... {}", host.to_uppercase()),
        format!("LINE .......... {line}"),
    ]
}

fn clock() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
