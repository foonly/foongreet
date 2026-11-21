//! The system battery charge, from /sys/class/power_supply, shown under
//! the clock. Batteries of peripherals (mice, headsets: `scope` is
//! "Device") are left out.

use crate::canvas::{BRIGHT, Canvas, NORMAL};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

const SUPPLIES: &str = "/sys/class/power_supply";
/// How often to read the charge.
const EVERY: Duration = Duration::from_secs(15);
/// Below this, while not charging, the charge is drawn bright.
const LOW: u8 = 15;

/// The charge as last read, for drawing under the clock.
#[derive(Default)]
pub struct Gauge {
    /// None without a battery (or in the demo).
    battery: Option<Battery>,
    read_at: Option<Instant>,
}

impl Gauge {
    /// Reads the charge again if it is time to. `demo` keeps it hidden,
    /// as the demo shows made-up machine details.
    pub fn poll(&mut self, demo: bool) {
        if demo || self.read_at.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.battery = read();
        self.read_at = Some(Instant::now());
    }

    /// Draws the charge right-aligned under the clock.
    pub fn draw(&self, c: &mut Canvas) {
        let Some(b) = self.battery else { return };
        let (text, level) = match b {
            Battery { percent, charging: true } => (format!("CHARGING {percent}%"), NORMAL),
            Battery { percent, .. } if percent < LOW => (format!("BATTERY LOW {percent}%"), BRIGHT),
            Battery { percent, .. } => (format!("BATTERY {percent}%"), NORMAL),
        };
        let (cw, ch) = (c.font.width, c.font.height);
        let cols = c.w / cw;
        c.text(cols.saturating_sub(text.len() + 2) * cw, 2 * ch, &text, level);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Battery {
    /// Charge in percent, over all system batteries.
    pub percent: u8,
    pub charging: bool,
}

/// None on a machine without a battery.
pub fn read() -> Option<Battery> {
    let mut now = 0u64;
    let mut full = 0u64;
    let mut capacities = Vec::new();
    let mut charging = false;
    for entry in fs::read_dir(SUPPLIES).ok()?.flatten() {
        let dir = entry.path();
        let attr = |name| read_attr(&dir, name);
        if attr("type").as_deref() != Some("Battery")
            || attr("scope").as_deref() == Some("Device")
            || attr("present").as_deref() == Some("0")
        {
            continue;
        }
        let Some(capacity) = attr("capacity").and_then(|c| c.parse::<u64>().ok()) else { continue };
        capacities.push(capacity);
        // Weigh batteries by size when the kernel tells it.
        let num = |name| attr(name).and_then(|v| v.parse::<u64>().ok());
        if let (Some(n), Some(f)) = (num("energy_now").or(num("charge_now")), num("energy_full").or(num("charge_full"))) {
            now += n;
            full += f;
        }
        charging |= attr("status").as_deref() == Some("Charging");
    }
    if capacities.is_empty() {
        return None;
    }
    let percent = if full > 0 && capacities.len() > 1 {
        now * 100 / full
    } else {
        capacities.iter().sum::<u64>() / capacities.len() as u64
    };
    Some(Battery { percent: percent.min(100) as u8, charging })
}

fn read_attr(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string())
}
