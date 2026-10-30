//! Keyboard input, decoded by whichever backend reads it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    ClearLine,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Esc,
    F1,
    F2,
    Interrupt,
}
