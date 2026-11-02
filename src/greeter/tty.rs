//! The VT the greeter runs on: raw keyboard input, graphics mode, and
//! switching to other VTs.
//!
//! Dropping `Tty` restores the terminal and text mode. A VT left in
//! KD_GRAPHICS shows nothing, so this also runs on panic and on signals.
//!
//! The kernel ignores VT switches (Alt+Fn) away from a VT in KD_GRAPHICS
//! unless its process handles them: in VT_PROCESS mode it signals us to
//! release the display first, and again when we are switched back to.

use foongreet::key::Key;
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const KDSETMODE: libc::c_ulong = 0x4B3A;
const KD_TEXT: libc::c_int = 0;
const KD_GRAPHICS: libc::c_int = 1;
const VT_SETMODE: libc::c_ulong = 0x5602;
const VT_RELDISP: libc::c_ulong = 0x5605;
const VT_AUTO: libc::c_char = 0;
const VT_PROCESS: libc::c_char = 1;
const VT_ACKACQ: libc::c_int = 2;

/// struct vt_mode from <linux/vt.h>.
#[repr(C)]
struct VtMode {
    mode: libc::c_char,
    waitv: libc::c_char,
    relsig: libc::c_short,
    acqsig: libc::c_short,
    frsig: libc::c_short,
}

pub static QUIT: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);
static ACQUIRE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::SeqCst);
}

extern "C" fn on_release(_: libc::c_int) {
    RELEASE.store(true, Ordering::SeqCst);
}

extern "C" fn on_acquire(_: libc::c_int) {
    ACQUIRE.store(true, Ordering::SeqCst);
}

/// A VT switch the main loop has to act on.
pub enum Switch {
    /// Switching to another VT: release the display, then `allow_away`.
    Away,
    /// Back on our VT: `allow_back`, then take the display again.
    Back,
}

pub fn install_signal_handlers() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
}

pub struct Tty {
    orig: libc::termios,
    graphics: bool,
    /// VT switches are handled (VT_PROCESS mode).
    switching: bool,
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
        let mut tty = Tty { orig, graphics, switching: false, pending: Vec::new() };
        if graphics && unsafe { libc::ioctl(0, KDSETMODE, KD_GRAPHICS) } != 0 {
            let err = io::Error::last_os_error();
            drop(tty);
            return Err(io::Error::new(err.kind(), format!("KDSETMODE (is stdin a VT?): {err}")));
        }
        if graphics {
            unsafe {
                libc::signal(libc::SIGUSR1, on_release as *const () as libc::sighandler_t);
                libc::signal(libc::SIGUSR2, on_acquire as *const () as libc::sighandler_t);
            }
            let mode = VtMode {
                mode: VT_PROCESS,
                waitv: 0,
                relsig: libc::SIGUSR1 as libc::c_short,
                acqsig: libc::SIGUSR2 as libc::c_short,
                frsig: 0,
            };
            tty.switching = unsafe { libc::ioctl(0, VT_SETMODE, &mode) } == 0;
            if !tty.switching {
                eprintln!("foongreet: VT_SETMODE: {}; switching VTs will not work", io::Error::last_os_error());
            }
        }
        Ok(tty)
    }

    /// A pending VT switch, if any.
    pub fn switch(&self) -> Option<Switch> {
        if RELEASE.swap(false, Ordering::SeqCst) {
            Some(Switch::Away)
        } else if ACQUIRE.swap(false, Ordering::SeqCst) {
            Some(Switch::Back)
        } else {
            None
        }
    }

    /// Lets the switch away go ahead, once the display is released.
    pub fn allow_away(&self) {
        unsafe { libc::ioctl(0, VT_RELDISP, 1) };
    }

    /// Acknowledges being switched back to.
    pub fn allow_back(&self) {
        unsafe { libc::ioctl(0, VT_RELDISP, VT_ACKACQ) };
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
            if self.switching {
                let mode = VtMode { mode: VT_AUTO, waitv: 0, relsig: 0, acqsig: 0, frsig: 0 };
                libc::ioctl(0, VT_SETMODE, &mode);
            }
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
            // F1-F4: Linux console (ESC [ [ A), xterm (ESC O P), vt220 (ESC [ 11 ~).
            [0x1b, b'[', b'[', b'A', ..] | [0x1b, b'O', b'P', ..] => (Some(Key::F1), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'[', b'B', ..] | [0x1b, b'O', b'Q', ..] => (Some(Key::F2), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'[', b'C', ..] | [0x1b, b'O', b'R', ..] => (Some(Key::F3), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'[', b'D', ..] | [0x1b, b'O', b'S', ..] => (Some(Key::F4), if rest[1] == b'O' { 3 } else { 4 }),
            [0x1b, b'[', b'1', b'1', b'~', ..] => (Some(Key::F1), 5),
            [0x1b, b'[', b'1', b'2', b'~', ..] => (Some(Key::F2), 5),
            [0x1b, b'[', b'1', b'3', b'~', ..] => (Some(Key::F3), 5),
            [0x1b, b'[', b'1', b'4', b'~', ..] => (Some(Key::F4), 5),
            // F5 on the console: ESC [ [ E
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
        for seq in [&b"\x1b[[C"[..], b"\x1bOR", b"\x1b[13~"] {
            assert_eq!(decode(seq), (vec![Key::F3], seq.len()));
        }
        for seq in [&b"\x1b[[D"[..], b"\x1bOS", b"\x1b[14~"] {
            assert_eq!(decode(seq), (vec![Key::F4], seq.len()));
        }
        // Other F-keys are swallowed, not typed as text.
        assert_eq!(decode(b"\x1b[[Ex"), (vec![Key::Char('x')], 5));
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
