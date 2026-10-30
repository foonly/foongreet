//! Desktop window for tuning the look without switching VTs.

use super::Output;
use crate::key::Key;
use minifb::{InputCallback, KeyRepeat, Window, WindowOptions};
use std::{cell::RefCell, io, rc::Rc, thread, time::Duration};

pub struct Preview {
    window: Window,
    buf: Vec<u32>,
    width: usize,
    height: usize,
    chars: Rc<RefCell<Vec<char>>>,
    keys: Vec<Key>,
}

struct Chars(Rc<RefCell<Vec<char>>>);

impl InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        if let Some(c) = char::from_u32(c).filter(|c| !c.is_control()) {
            self.0.borrow_mut().push(c);
        }
    }
}

impl Preview {
    pub fn open(width: usize, height: usize) -> io::Result<Preview> {
        let mut window = Window::new("foongreet preview", width, height, WindowOptions::default())
            .map_err(|e| io::Error::other(e.to_string()))?;
        window.set_target_fps(0);
        let chars = Rc::new(RefCell::new(Vec::new()));
        window.set_input_callback(Box::new(Chars(chars.clone())));
        Ok(Preview { window, buf: vec![0; width * height], width, height, chars, keys: Vec::new() })
    }
}

impl Output for Preview {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()> {
        draw(&mut self.buf, self.width);
        self.window
            .update_with_buffer(&self.buf, self.width, self.height)
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.collect_keys();
        Ok(())
    }

    fn keys(&mut self, timeout: Duration) -> Vec<Key> {
        // Frames can be a second apart when the greeter is dormant, so
        // handle window events here too.
        self.window.update();
        self.collect_keys();
        if self.keys.is_empty() {
            thread::sleep(timeout);
        }
        std::mem::take(&mut self.keys)
    }

    fn is_open(&self) -> bool {
        self.window.is_open()
    }
}

impl Preview {
    fn collect_keys(&mut self) {
        use minifb::Key as K;
        let shift = self.window.is_key_down(K::LeftShift) || self.window.is_key_down(K::RightShift);
        for k in self.window.get_keys_pressed(KeyRepeat::Yes) {
            self.keys.extend(match k {
                K::Enter | K::NumPadEnter => Some(Key::Enter),
                K::Backspace => Some(Key::Backspace),
                K::Tab if shift => Some(Key::BackTab),
                K::Tab => Some(Key::Tab),
                K::Up => Some(Key::Up),
                K::Down => Some(Key::Down),
                K::Left => Some(Key::Left),
                K::Right => Some(Key::Right),
                K::Escape => Some(Key::Esc),
                K::F1 => Some(Key::F1),
                K::F2 => Some(Key::F2),
                _ => None,
            });
        }
        self.keys.extend(self.chars.borrow_mut().drain(..).map(Key::Char));
    }
}
