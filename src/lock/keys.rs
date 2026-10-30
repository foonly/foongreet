//! Wayland key events (xkbcommon keysyms) to `Key`.

use foongreet::key::Key;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

pub fn map(ev: &KeyEvent) -> Option<Key> {
    Some(match ev.keysym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Escape => Key::Esc,
        Keysym::Tab => Key::Tab,
        Keysym::ISO_Left_Tab => Key::BackTab,
        Keysym::Up => Key::Up,
        Keysym::Down => Key::Down,
        Keysym::Left => Key::Left,
        Keysym::Right => Key::Right,
        Keysym::F1 => Key::F1,
        Keysym::F2 => Key::F2,
        _ => {
            // xkbcommon applies Ctrl, so Ctrl+U arrives as ^U like on a VT.
            let mut chars = ev.utf8.as_deref()?.chars();
            match (chars.next()?, chars.next()) {
                ('\x15', None) => Key::ClearLine,
                (c, None) if !c.is_control() => Key::Char(c),
                _ => return None,
            }
        }
    })
}
