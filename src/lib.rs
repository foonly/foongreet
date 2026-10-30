//! Shared by the greeter and the screen locker: the CRT renderer, fonts,
//! drawing on the luminance buffer, config, and display outputs.

pub mod background;
pub mod canvas;
pub mod config;
pub mod crt;
pub mod font;
pub mod idle;
pub mod key;
pub mod output;
pub mod tube;
