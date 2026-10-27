//! Last successful user and session, kept between runs.
//!
//! The greeter runs as an unprivileged user, so the cache directory must be
//! created and owned by that user beforehand. Failures to read or write are
//! not fatal: the greeter just starts with an empty form.

use std::{fs, io, path::PathBuf};

pub struct State {
    dir: PathBuf,
    pub user: String,
    pub session: String,
}

impl State {
    pub fn load(dir: PathBuf) -> State {
        let mut state = State { dir, user: String::new(), session: String::new() };
        if let Ok(text) = fs::read_to_string(state.dir.join("state")) {
            for line in text.lines() {
                match line.split_once('=') {
                    Some(("user", v)) => state.user = v.to_string(),
                    Some(("session", v)) => state.session = v.to_string(),
                    _ => {}
                }
            }
        }
        state
    }

    /// Writes atomically so a crash mid-write never leaves a torn file.
    pub fn save(&self) -> io::Result<()> {
        let tmp = self.dir.join("state.tmp");
        fs::write(&tmp, format!("user={}\nsession={}\n", self.user, self.session))?;
        fs::rename(tmp, self.dir.join("state"))
    }
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
}
