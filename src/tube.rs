//! The tube powering on and off: black until the display has synced, the
//! picture growing out of a dot, and collapsing back into one before exit.

use std::time::Instant;

const POWER_OFF: f32 = 0.45;
const POWER_ON: f32 = 0.6;

pub enum Frame {
    /// Draw with this collapse (0 = full picture, 1 = a dot).
    Collapse(f32),
    /// Finished powering on this frame; draw the full picture and start
    /// the UI's animations from here.
    PoweredOn,
    /// Finished powering off.
    Off,
}

pub struct Tube {
    /// Seconds of black after the first frame reaches the display.
    warmup: f32,
    shown_at: Option<Instant>,
    on: bool,
    off_at: Option<Instant>,
}

impl Tube {
    pub fn new(warmup: f32) -> Tube {
        Tube { warmup: warmup.max(0.0), shown_at: None, on: false, off_at: None }
    }

    /// Call once frames are reaching the display; later calls do nothing.
    pub fn shown(&mut self) {
        self.shown_at.get_or_insert_with(Instant::now);
    }

    /// Starts powering off; later calls do nothing.
    pub fn power_off(&mut self) {
        self.off_at.get_or_insert_with(Instant::now);
    }

    /// Whether the power-on has finished (still true while powering off).
    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn powering_off(&self) -> bool {
        self.off_at.is_some()
    }

    pub fn frame(&mut self) -> Frame {
        if let Some(off) = self.off_at {
            let c = off.elapsed().as_secs_f32() / POWER_OFF;
            return if c >= 1.0 { Frame::Off } else { Frame::Collapse(c) };
        }
        if self.on {
            return Frame::Collapse(0.0);
        }
        let since = self.shown_at.map_or(-1.0, |s| s.elapsed().as_secs_f32() - self.warmup);
        if since >= POWER_ON {
            self.on = true;
            Frame::PoweredOn
        } else {
            Frame::Collapse(1.0 - (since / POWER_ON).clamp(0.0, 1.0))
        }
    }
}
