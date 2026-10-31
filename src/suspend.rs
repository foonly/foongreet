//! Noticing that the system was suspended. `Instant` (CLOCK_MONOTONIC)
//! stops while suspended and CLOCK_BOOTTIME does not, so the gap between
//! them grows by the time spent asleep.

use std::time::Duration;

/// Smaller jumps are scheduling noise, not a suspend.
const MIN_SLEEP: Duration = Duration::from_millis(500);

pub struct SuspendWatch {
    gap: Duration,
}

impl Default for SuspendWatch {
    fn default() -> SuspendWatch {
        SuspendWatch { gap: gap() }
    }
}

impl SuspendWatch {
    /// Whether the system was suspended since the last call.
    pub fn resumed(&mut self) -> bool {
        let gap = gap();
        let slept = gap.saturating_sub(self.gap) >= MIN_SLEEP;
        self.gap = gap;
        slept
    }
}

fn gap() -> Duration {
    let read = |clock| {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: valid clock id and pointer; cannot fail on Linux.
        unsafe { libc::clock_gettime(clock, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    };
    read(libc::CLOCK_BOOTTIME).saturating_sub(read(libc::CLOCK_MONOTONIC))
}
