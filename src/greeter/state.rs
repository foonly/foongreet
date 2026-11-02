//! Last successful user and session, kept between runs, and the screen
//! and rotation picked with F3/F4.
//!
//! The greeter runs as an unprivileged user, so the cache directory must be
//! created and owned by that user beforehand. Failures to read or write are
//! not fatal: the greeter just starts with an empty form.

use foongreet::crt::Rotation;
use std::{collections::BTreeMap, fs, io, path::PathBuf};

pub struct State {
    dir: PathBuf,
    pub user: String,
    pub session: String,
    /// "screen.<ids of a set of screens>" = id of the one for the login,
    /// "rotate.<id>" = degrees.
    screens: BTreeMap<String, String>,
}

impl State {
    pub fn load(dir: PathBuf) -> State {
        let mut state = State { dir, user: String::new(), session: String::new(), screens: BTreeMap::new() };
        if let Ok(text) = fs::read_to_string(state.dir.join("state")) {
            for line in text.lines() {
                match line.split_once('=') {
                    Some(("user", v)) => state.user = v.to_string(),
                    Some(("session", v)) => state.session = v.to_string(),
                    Some((k, v)) if k.starts_with("screen.") || k.starts_with("rotate.") => {
                        state.screens.insert(k.into(), v.into());
                    }
                    _ => {}
                }
            }
        }
        state
    }

    /// Writes atomically so a crash mid-write never leaves a torn file.
    pub fn save(&self) -> io::Result<()> {
        let tmp = self.dir.join("state.tmp");
        let mut text = format!("user={}\nsession={}\n", self.user, self.session);
        for (k, v) in &self.screens {
            text += &format!("{k}={v}\n");
        }
        fs::write(&tmp, text)?;
        fs::rename(tmp, self.dir.join("state"))
    }

    /// The screen picked for the login when exactly the screens `ids` are
    /// connected.
    pub fn screen(&self, ids: &[String]) -> Option<&str> {
        self.screens.get(&set_key(ids)).map(String::as_str)
    }

    pub fn set_screen(&mut self, ids: &[String], id: &str) {
        self.screens.insert(set_key(ids), id.into());
    }

    pub fn rotation(&self, id: &str) -> Rotation {
        let deg = self.screens.get(&format!("rotate.{id}")).and_then(|d| d.parse().ok());
        deg.and_then(Rotation::from_degrees).unwrap_or_default()
    }

    pub fn set_rotation(&mut self, id: &str, rotation: Rotation) {
        let key = format!("rotate.{id}");
        if rotation == Rotation::R0 {
            self.screens.remove(&key);
        } else {
            self.screens.insert(key, rotation.degrees().to_string());
        }
    }
}

/// The same for any order of the same screens.
fn set_key(ids: &[String]) -> String {
    let mut ids = ids.to_vec();
    ids.sort();
    format!("screen.{}", ids.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("foongreet-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut s = State::load(dir.clone());
        assert!(s.user.is_empty());
        s.user = "niklas".into();
        s.session = "Hyprland (uwsm-managed)".into();
        s.save().unwrap();
        let s = State::load(dir.clone());
        assert_eq!((s.user.as_str(), s.session.as_str()), ("niklas", "Hyprland (uwsm-managed)"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn screens_round_trip() {
        let dir = std::env::temp_dir().join(format!("foongreet-test-screens-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut s = State::load(dir.clone());
        let ids = ["b".to_string(), "a".to_string()];
        s.set_screen(&ids, "b");
        s.set_rotation("b", Rotation::R270);
        s.save().unwrap();
        let s = State::load(dir.clone());
        assert_eq!(s.screen(&["a".to_string(), "b".to_string()]), Some("b"));
        assert_eq!(s.screen(&["a".to_string()]), None);
        assert_eq!((s.rotation("b"), s.rotation("a")), (Rotation::R270, Rotation::R0));
        fs::remove_dir_all(dir).unwrap();
    }
}
