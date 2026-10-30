pub mod fbdev;
pub mod kms;
#[cfg(feature = "preview")]
pub mod preview;
pub mod snapshot;

use crate::key::Key;
use std::{io, time::Duration};

pub trait Output {
    fn size(&self) -> (usize, usize);
    /// Calls `draw` with the back buffer (XRGB8888) and its stride in
    /// pixels, then shows it.
    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()>;
    /// Waits up to `timeout` for keyboard input. Console outputs read keys
    /// from the VT instead, so they just sleep here.
    fn keys(&mut self, timeout: Duration) -> Vec<Key> {
        std::thread::sleep(timeout);
        Vec::new()
    }
    /// Whether frames are actually reaching the display yet.
    fn shown(&self) -> bool {
        true
    }
    /// A setup problem worth showing on screen.
    fn warning(&self) -> Option<String> {
        None
    }
    fn is_open(&self) -> bool {
        true
    }
}
