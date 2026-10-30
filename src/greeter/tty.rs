//! The VT the greeter runs on: raw keyboard input and graphics mode.
//!
//! Dropping `Tty` restores the terminal and text mode. A VT left in
//! KD_GRAPHICS shows nothing, so this also runs on panic and on signals.

use foongreet::key::Key;
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const KDSETMODE: libc::c_ulong = 0x4B3A;
const KD_TEXT: libc::c_int = 0;
const KD_GRAPHICS: libc::c_int = 1;

pub static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
}

pub struct Tty {
    orig: libc::termios,
    graphics: bool,
    pending: Vec<u8>,
}

impl Tty {
    /// Puts stdin into raw mode and (optionally) the VT into graphics mode.
    pub fn open(graphics: bool) -> io::Result<Tty> {
        let mut orig: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut orig) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = orig;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let tty = Tty { orig, graphics, pending: Vec::new() };
        if graphics && unsafe { libc::ioctl(0, KDSETMODE, KD_GRAPHICS) } != 0 {
            let err = io::Error::last_os_error();
            drop(tty);
            return Err(io::Error::new(err.kind(), format!("KDSETMODE (is stdin a VT?): {err}")));
        }
        Ok(tty)
    }

    /// Waits up to `timeout` for input and returns decoded keys.
    pub fn keys(&mut self, timeout: Duration) -> Vec<Key> {
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
            return Vec::new();
        }
        let mut buf = [0u8; 256];
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return Vec::new();
        }
        self.pending.extend_from_slice(&buf[..n as usize]);
        let (keys, used) = decode(&self.pending);
        self.pending.drain(..used);
        keys
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        unsafe {
            if self.graphics {
                libc::ioctl(0, KDSETMODE, KD_TEXT);
            }
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

/// Decodes Linux console input. Returns keys and the number of bytes used;
/// an incomplete UTF-8 sequence at the end is left for the next read.
fn decode(b: &[u8]) -> (Vec<Key>, usize) {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        let (key, len) = match rest {
            [0x1b, b'[', b'A', ..] => (Some(Key::Up), 3),
            [0x1b, b'[', b'B', ..] => (Some(Key::Down), 3),
            [0x1b, b'[', b'C', ..] => (Some(Key::Right), 3),
            [0x1b, b'[', b'D', ..] => (Some(Key::Left), 3),
            [0x1b, b'[', b'Z', ..] => (Some(Key::BackTab), 3),
            // F1/F2: Linux console (ESC [ [ A), xterm (ESC O P), vt220 (ESC [ 11 ~).
            [0x1b, b'[', b'[', b'A', ..] | [0x1b, b'O', b'P', ..] => (Some(Key::F1), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'[', b'B', ..] | [0x1b, b'O', b'Q', ..] => (Some(Key::F2), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'1', b'1', b'~', ..] => (Some(Key::F1), 5),
            [0x1b, b'[', b'1', b'2', b'~', ..] => (Some(Key::F2), 5),
            // Other console F-keys: ESC [ [ C..E
            [0x1b, b'[', b'[', _, ..] => (None, 4),
            [0x1b, b'O', _, ..] => (None, 3),
            // Other CSI sequences (F6+, Home, Del...): skip to the final byte.
            [0x1b, b'[', tail @ ..] => {
                let end = tail.iter().position(|c| (0x40..=0x7e).contains(c));
                match end {
                    Some(p) => (None, 3 + p),
                    None => break,
                }
            }
            [0x1b, ..] => (Some(Key::Esc), 1),
            [b'\r' | b'\n', ..] => (Some(Key::Enter), 1),
            [b'\t', ..] => (Some(Key::Tab), 1),
            [0x7f | 0x08, ..] => (Some(Key::Backspace), 1),
            [0x15, ..] => (Some(Key::ClearLine), 1),
            [0x03, ..] => (Some(Key::Interrupt), 1),
            [c, ..] if *c < 0x20 => (None, 1),
            _ => {
                let len = utf8_len(rest[0]);
                if rest.len() < len {
                    break;
                }
                let ch = std::str::from_utf8(&rest[..len]).ok().and_then(|s| s.chars().next());
                (ch.map(Key::Char), len)
            }
        };
        keys.extend(key);
        i += len;
    }
    (keys, i)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Name of the controlling VT, e.g. "TTY1".
pub fn name() -> String {
    std::fs::read_link("/proc/self/fd/0")
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_uppercase()))
        .filter(|n| n.starts_with("TTY"))
        .unwrap_or_else(|| "CONSOLE".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys() {
        for seq in [&b"\x1b[[A"[..], b"\x1bOP", b"\x1b[11~"] {
            assert_eq!(decode(seq), (vec![Key::F1], seq.len()));
        }
        for seq in [&b"\x1b[[B"[..], b"\x1bOQ", b"\x1b[12~"] {
            assert_eq!(decode(seq), (vec![Key::F2], seq.len()));
        }
        // Other F-keys are swallowed, not typed as text.
        assert_eq!(decode(b"\x1b[[Cx"), (vec![Key::Char('x')], 5));
        assert_eq!(decode(b"\x1b[15~y"), (vec![Key::Char('y')], 6));
    }

    #[test]
    fn mixed_input() {
        let (keys, used) = decode("ab\x1b[A\r\x7fä".as_bytes());
        assert_eq!(keys, [Key::Char('a'), Key::Char('b'), Key::Up, Key::Enter, Key::Backspace, Key::Char('ä')]);
        assert_eq!(used, 9);
        // Incomplete UTF-8 is left for the next read.
        assert_eq!(decode(&[b'a', 0xc3]), (vec![Key::Char('a')], 1));
    }
}
