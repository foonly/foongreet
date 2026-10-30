//! The lock screen: boot header, clock, and the lock box, drawn into the
//! low-res luminance buffer of the screen that has the prompt.

use foongreet::{
    canvas,
    canvas::{BRIGHT, Canvas, DIM, NORMAL},
    font::Font,
};
use std::time::Instant;

const BOX_COLS: usize = 52;
const BOX_ROWS: usize = 7;
const LABEL_COLS: usize = 12;

pub struct Ui {
    pub font: Font,
    start: Instant,
    header: Vec<String>,
    demo: bool,
    user: String,
}

impl Ui {
    /// `line` is the output the prompt is on, shown in the header.
    pub fn new(font: Font, user: String, line: &str, demo: bool) -> Ui {
        Ui { font, start: Instant::now(), header: canvas::header_lines(demo, line), demo, user }
    }

    /// The output the prompt moved to.
    pub fn set_line(&mut self, line: &str) {
        self.header = canvas::header_lines(self.demo, line);
    }

    /// Restarts animations (the boot header typing out) from the beginning.
    pub fn restart_clock(&mut self) {
        self.start = Instant::now();
    }

    pub fn time(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Draws over `buf`, which holds the background already.
    pub fn draw(&self, buf: &mut [u8], w: usize, h: usize) {
        let t = self.time();
        let mut c = Canvas { buf, w, h, font: &self.font };
        let (cw, ch) = (self.font.width, self.font.height);
        let cols = w / cw;
        let rows = h / ch;

        if !c.header(&self.header, t, self.demo) {
            return;
        }
        let bc = (cols.saturating_sub(BOX_COLS)) / 2;
        let br = (rows.saturating_sub(BOX_ROWS)) / 2 + 1;
        let (x0, y0) = (bc * cw, br * ch);
        c.fill(x0 - cw, y0 - ch / 2, (BOX_COLS + 2) * cw, BOX_ROWS * ch + ch, 0);
        c.frame(bc, br, BOX_COLS, BOX_ROWS, " TERMINAL LOCKED ", NORMAL);

        let user = self.user.to_uppercase();
        c.text((bc + 4) * cw, (br + 2) * ch, &format!("{:.<width$}", "USER", width = LABEL_COLS - 1), DIM);
        c.text((bc + 4 + LABEL_COLS) * cw, (br + 2) * ch, &user, NORMAL);

        // Test build: no password check yet.
        let blink = (t * 2.0) as u32 % 2 == 0;
        let status = if blink { "PRESS ANY KEY TO UNLOCK" } else { "" };
        c.text((bc + 4) * cw, (br + 4) * ch, status, BRIGHT);

        let footer = "[ANY KEY] UNLOCK  (TEST MODE, NO PASSWORD CHECK)";
        c.text(cols.saturating_sub(footer.len()) / 2 * cw, (rows - 3) * ch, footer, DIM);
    }
}
