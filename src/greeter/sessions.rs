//! Session discovery from wayland-sessions desktop entries.

use foongreet::config;
use std::{fs, path::Path};

pub struct Session {
    pub name: String,
    /// Empty means "the user's login shell", resolved at start time.
    pub cmd: Vec<String>,
    pub env: Vec<String>,
}

/// Custom sessions from the config first, then the discovered ones, then
/// the user's shell.
pub fn discover(custom: &[config::Session]) -> Vec<Session> {
    let mut out: Vec<Session> = custom
        .iter()
        .map(|s| Session { name: s.name.clone(), cmd: split_words(&s.command), env: s.env.clone() })
        .collect();

    let mut found = Vec::new();
    if let Ok(dir) = fs::read_dir("/usr/share/wayland-sessions") {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "desktop") {
                if let Some(s) = parse_desktop(&path) {
                    found.push(s);
                }
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(found);

    out.push(Session { name: "Shell".into(), cmd: Vec::new(), env: Vec::new() });
    out
}

fn parse_desktop(path: &Path) -> Option<Session> {
    let text = fs::read_to_string(path).ok()?;
    let (mut name, mut exec) = (None, None);
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "Hidden" | "NoDisplay" if v.trim() == "true" => return None,
            _ => {}
        }
    }
    let cmd: Vec<String> = split_words(&exec?).into_iter().filter(|w| !w.starts_with('%')).collect();
    if cmd.is_empty() {
        return None;
    }
    Some(Session { name: name?, cmd, env: vec!["XDG_SESSION_TYPE=wayland".into()] })
}

/// Minimal shell-style word splitting with single/double quotes.
pub fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut has_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, ' ' | '\t') => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                has_word = true;
            }
            (Some(q), _) if c == q => quote = None,
            (q, '\\') if q != Some('\'') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has_word = true;
                }
            }
            _ => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words
}

/// The login shell for `user` from /etc/passwd.
pub fn login_shell(user: &str) -> String {
    fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|p| {
            p.lines()
                .map(|l| l.split(':').collect::<Vec<_>>())
                .find(|f| f.len() >= 7 && f[0] == user)
                .map(|f| f[6].to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}
