//! Going dormant when nobody is around. After `IDLE_AFTER` without input
//! the picture freezes (the caller hides the cursor and sets `Crt::still`)
//! and is only redrawn when the clock ticks; any input wakes it. A
//! full-rate CRT costs a good part of a core.

use crate::crt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const IDLE_AFTER: Duration = Duration::from_secs(25);

pub struct Idle {
    last_input: Instant,
    /// UI time when it went dormant.
    since: Option<f32>,
    /// Time spent dormant, which the rolling bar skips: it was parked off
    /// screen and carries on from there.
    bar_offset: f32,
    /// Unix second of the clock in the last dormant frame.
    clock_drawn: u64,
}

impl Default for Idle {
    fn default() -> Idle {
        Idle { last_input: Instant::now(), since: None, bar_offset: 0.0, clock_drawn: 0 }
    }
}

impl Idle {
    /// A key or other event someone will want to see the result of.
    pub fn input(&mut self) {
        self.last_input = Instant::now();
    }

    /// The UI clock restarted (at power-on).
    pub fn restart(&mut self) {
        self.since = None;
        self.bar_offset = 0.0;
    }

    pub fn dormant(&self) -> bool {
        self.since.is_some()
    }

    /// Goes dormant after `IDLE_AFTER` without input, once the rolling bar
    /// is off screen so it doesn't freeze halfway; wakes on input or while
    /// `busy` (something animating). `t` is the UI time. Returns the new
    /// state when it changed.
    pub fn update(&mut self, t: f32, busy: bool) -> Option<bool> {
        let quiet = !busy && self.last_input.elapsed() >= IDLE_AFTER;
        match self.since {
            None if quiet && !crt::bar_visible(t - self.bar_offset) => {
                self.since = Some(t);
                Some(true)
            }
            Some(since) if !quiet => {
                self.bar_offset += t - since;
                self.since = None;
                Some(false)
            }
            _ => None,
        }
    }

    /// The time to render the CRT at, given the UI time.
    pub fn crt_time(&self, t: f32) -> f32 {
        self.since.unwrap_or(t) - self.bar_offset
    }

    /// Whether a frame is due: always while awake, when the clock's second
    /// has changed while dormant. Gives the second to pass to `drawn`.
    pub fn due(&self) -> Option<u64> {
        let now = now();
        (self.since.is_none() || now != self.clock_drawn).then_some(now)
    }

    /// Call after drawing the frame `due` allowed.
    pub fn drawn(&mut self, second: u64) {
        self.clock_drawn = second;
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
