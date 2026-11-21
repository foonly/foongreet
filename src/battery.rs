//! The system battery charge, from /sys/class/power_supply. Batteries of
//! peripherals (mice, headsets: `scope` is "Device") are left out.

use std::{fs, path::Path};

const SUPPLIES: &str = "/sys/class/power_supply";

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
