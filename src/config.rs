//! Settings from /etc/foongreet/config.toml. See config.example.toml.

use serde::Deserialize;
use std::{fs, io, path::Path, path::PathBuf};

pub const DEFAULT_PATH: &str = "/etc/foongreet/config.toml";
pub const DEFAULT_FONT: &str = "/usr/share/kbd/consolefonts/default8x16.psfu.gz";

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub display: Display,
    pub login: Login,
    pub power: Power,
    /// Custom sessions, listed before the discovered ones.
    #[serde(rename = "session")]
    pub sessions: Vec<Session>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    pub background: Option<PathBuf>,
    pub background_dim: f32,
    pub font: PathBuf,
    pub fbdev: bool,
    pub phosphor: crate::crt::Phosphor,
    /// Connector for the login, e.g. "DP-1". Default: first connected.
    pub output: Option<String>,
    /// Seconds of black after the first frame before powering on, so the
    /// monitor has synced and the animation is actually seen.
    pub startup_delay: f32,
}

impl Default for Display {
    fn default() -> Display {
        Display { background: None, background_dim: 0.35, font: DEFAULT_FONT.into(), fbdev: false, phosphor: Default::default(), output: None, startup_delay: 2.0 }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Login {
    /// Preselected user; overrides the remembered one.
    pub default_user: String,
    pub cache_dir: PathBuf,
}

impl Default for Login {
    fn default() -> Login {
        Login { default_user: String::new(), cache_dir: "/var/cache/foongreet".into() }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Power {
    pub poweroff: String,
    pub reboot: String,
}

impl Default for Power {
    fn default() -> Power {
        Power { poweroff: "systemctl poweroff".into(), reboot: "systemctl reboot".into() }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub env: Vec<String>,
}

/// Loads the config. Never fails: a broken config must not lock anyone out,
/// so errors fall back to defaults and are returned as a warning. A missing
/// file at the default path is not an error.
pub fn load(path: &Path) -> (Config, Option<String>) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound && path == Path::new(DEFAULT_PATH) => {
            return (Config::default(), None);
        }
        Err(e) => return (Config::default(), Some(format!("{}: {e}", path.display()))),
    };
    match toml::from_str(&text) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// As shipped, the example is all defaults, so it can be installed as is.
    #[test]
    fn example_is_defaults() {
        let config: Config = toml::from_str(EXAMPLE).unwrap();
        assert!(config.sessions.is_empty() && config.display.background.is_none() && config.display.output.is_none());
        assert_eq!(config.power.reboot, "systemctl reboot");
    }

    /// The commented-out "#key = value" examples must stay valid too.
    #[test]
    fn example_with_extras_parses() {
        let uncommented: String = EXAMPLE
            .lines()
            .map(|l| if l.starts_with('#') && !l.starts_with("# ") && l != "#" { &l[1..] } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        let config: Config = toml::from_str(&uncommented).unwrap();
        assert_eq!(config.sessions.len(), 1);
        assert_eq!(config.display.output.as_deref(), Some("DP-1"));
    }

    #[test]
    fn empty_is_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.display.font, Path::new(DEFAULT_FONT));
        assert!(config.sessions.is_empty());
    }

    #[test]
    fn phosphor() {
        let config: Config = toml::from_str("[display]\nphosphor = \"green\"").unwrap();
        assert_eq!(config.display.phosphor, crate::crt::Phosphor::Green);
        assert!(toml::from_str::<Config>("[display]\nphosphor = \"blue\"").is_err());
    }

    #[test]
    fn typos_are_rejected() {
        assert!(toml::from_str::<Config>("[display]\nbackgrund = \"x.png\"").is_err());
    }
}
