//! Shortcuts. Defaults use a prefix (`ctrl+b`, then a key). Override them in
//! `$XDG_CONFIG_HOME/pctl/config.toml` or `~/.config/pctl/config.toml`:
//!
//! ```toml
//! [keys]
//! prefix = "ctrl+b"
//! next_tab = "prefix+n"    # after the prefix
//! prev_tab = "alt+left"    # or a direct key, no prefix
//! image_paste = ""         # an empty value turns a shortcut off
//! ```
//!
//! shortcut: only `[keys]` with `name = "key"` lines is read (no full TOML parser); a real
//! parser when the config grows.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    NewAgent,
    NewProject,
    NextTab,
    PrevTab,
    StopAgent,
    RestartAgent,
    AgentMenu,
    ToggleSidebar,
    FocusSidebar,
    Theme,
    Help,
    Detach,
    ImagePaste,
}

/// Name in the config file, default key, and what the help screen says.
pub const ACTIONS: [(&str, &str, Action, &str); 14] = [
    (
        "new_tab",
        "prefix+c",
        Action::NewTab,
        "New bash tab in this project",
    ),
    (
        "new_agent",
        "prefix+a",
        Action::NewAgent,
        "New agent (Claude Code, Codex, OpenCode, …)",
    ),
    (
        "new_project",
        "prefix+shift+c",
        Action::NewProject,
        "New project",
    ),
    ("next_tab", "prefix+n", Action::NextTab, "Next tab"),
    ("prev_tab", "prefix+p", Action::PrevTab, "Previous tab"),
    (
        "stop_agent",
        "prefix+x",
        Action::StopAgent,
        "Stop this agent (asks first)",
    ),
    (
        "restart_agent",
        "prefix+r",
        Action::RestartAgent,
        "Restart this agent, same conversation",
    ),
    (
        "agent_menu",
        "prefix+m",
        Action::AgentMenu,
        "This agent's menu: restart, stop, start, remove",
    ),
    (
        "toggle_sidebar",
        "prefix+b",
        Action::ToggleSidebar,
        "Show or hide the sidebar",
    ),
    (
        "focus_sidebar",
        "prefix+w",
        Action::FocusSidebar,
        "Move between projects and agents with the keyboard",
    ),
    ("theme", "prefix+t", Action::Theme, "Pick a colour theme"),
    ("help", "prefix+?", Action::Help, "Show shortcuts"),
    (
        "detach",
        "prefix+q",
        Action::Detach,
        "Leave; agents keep running",
    ),
    (
        "image_paste",
        "ctrl+v",
        Action::ImagePaste,
        "Paste an image copied here into an agent on another computer",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}
impl Key {
    /// Shift is part of the character ("shift+c" is stored as 'C'), so it only counts there.
    pub fn matches(&self, e: &KeyEvent) -> bool {
        match (self.code, e.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                let b = if e.modifiers.contains(KeyModifiers::SHIFT) {
                    b.to_ascii_uppercase()
                } else {
                    b
                };
                a == b && self.mods - KeyModifiers::SHIFT == e.modifiers - KeyModifiers::SHIFT
            }
            (a, b) => a == b && self.mods == e.modifiers,
        }
    }
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("alt+");
        }
        s.push_str(&match self.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                format!("shift+{}", c.to_ascii_lowercase())
            }
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("f{n}"),
            other => format!("{other:?}").to_lowercase(),
        });
        s
    }
}

/// `"ctrl+b"`, `"alt+left"`, `"shift+c"`, `"f2"`, `"?"`, optionally after `"prefix+"`.
pub fn parse_key(text: &str) -> Option<(bool, Key)> {
    let t = text.trim().to_lowercase();
    let (prefix, rest) = match t.strip_prefix("prefix+") {
        Some(r) => (true, r.to_string()),
        None => (false, t),
    };
    let mut mods = KeyModifiers::NONE;
    let mut shift = false;
    let parts: Vec<&str> = rest.split('+').collect();
    let (last, heads) = if rest.ends_with("++") || rest == "+" {
        ("+", &parts[..parts.len().saturating_sub(2)])
    } else {
        (*parts.last()?, &parts[..parts.len() - 1])
    };
    for h in heads {
        match *h {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
            "shift" => shift = true,
            _ => return None,
        }
    }
    let code = match last {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            KeyCode::F(f[1..].parse().ok()?)
        }
        c if c.chars().count() == 1 => {
            let ch = c.chars().next()?;
            KeyCode::Char(if shift && ch.is_ascii_alphabetic() {
                ch.to_ascii_uppercase()
            } else {
                ch
            })
        }
        _ => return None,
    };
    if shift && !matches!(code, KeyCode::Char(_)) {
        mods |= KeyModifiers::SHIFT;
    }
    Some((prefix, Key { code, mods }))
}

#[derive(Clone, Debug)]
pub struct Keymap {
    pub prefix: Key,
    pub binds: Vec<(bool, Key, Action)>,
    pub source: String,
    /// `[ui] wit = false` turns the jokes into plain text.
    pub wit: bool,
}
impl Keymap {
    pub fn defaults() -> Self {
        Self::from_pairs(&[]).0
    }
    fn from_pairs(pairs: &[(String, String)]) -> (Self, Vec<String>) {
        let mut warn = Vec::new();
        let lookup = |name: &str| {
            pairs
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let prefix = match lookup("prefix").map(|v| (parse_key(&v), v)) {
            Some((Some((false, k)), _)) => k,
            Some((_, v)) => {
                warn.push(format!("prefix = {v:?} is not a key; using ctrl+b"));
                parse_key("ctrl+b").expect("valid default").1
            }
            None => parse_key("ctrl+b").expect("valid default").1,
        };
        for (k, _) in pairs {
            if k != "prefix" && !ACTIONS.iter().any(|(n, ..)| n == k) {
                warn.push(format!("unknown shortcut name {k:?}"));
            }
        }
        let binds = ACTIONS
            .iter()
            .filter(|(name, ..)| lookup(name).is_none_or(|v| !v.is_empty()))
            .map(|(name, default, action, _)| {
                let chosen = lookup(name).and_then(|v| match parse_key(&v) {
                    Some(k) => Some(k),
                    None => {
                        warn.push(format!("{name} = {v:?} is not a key; using {default}"));
                        None
                    }
                });
                let (p, k) = chosen.unwrap_or_else(|| parse_key(default).expect("valid default"));
                (p, k, *action)
            })
            .collect();
        (
            Self {
                prefix,
                binds,
                source: String::new(),
                wit: true,
            },
            warn,
        )
    }
    /// Read the config file if there is one. Problems become warnings, never a failed start.
    pub fn load() -> (Self, Vec<String>) {
        let Some(path) = config_path() else {
            return (Self::defaults(), vec![]);
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (Self::defaults(), vec![]);
        };
        let (mut map, warn) = Self::from_pairs(&section(&text, "keys"));
        map.source = path.to_string_lossy().into();
        map.wit = !section(&text, "ui")
            .iter()
            .any(|(k, v)| k == "wit" && v == "false");
        (map, warn)
    }
    pub fn action(&self, prefixed: bool, e: &KeyEvent) -> Option<Action> {
        self.binds
            .iter()
            .find(|(p, k, _)| *p == prefixed && k.matches(e))
            .map(|(_, _, a)| *a)
    }
    pub fn label(&self, action: Action) -> String {
        self.binds
            .iter()
            .find(|(_, _, a)| *a == action)
            .map(|(p, k, _)| {
                if *p {
                    format!("{} {}", self.prefix.label(), k.label())
                } else {
                    k.label()
                }
            })
            .unwrap_or_default()
    }
}

/// `$XDG_CONFIG_HOME/pctl/config.toml`, else `~/.config/pctl/config.toml`.
pub(crate) fn config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("pctl").join("config.toml"))
}

/// `name = "value"` pairs under `[name]`; comments and other sections are skipped.
pub(crate) fn section(text: &str, name: &str) -> Vec<(String, String)> {
    let mut in_keys = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_keys = line == format!("[{name}]");
            continue;
        }
        if let (true, Some((k, v))) = (in_keys, line.split_once('=')) {
            out.push((k.trim().to_string(), v.trim().trim_matches('"').to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }
    #[test]
    fn defaults_and_overrides() {
        let d = Keymap::defaults();
        assert_eq!(
            d.prefix,
            Key {
                code: KeyCode::Char('b'),
                mods: KeyModifiers::CONTROL
            }
        );
        assert_eq!(
            d.action(true, &ev(KeyCode::Char('c'), KeyModifiers::NONE)),
            Some(Action::NewTab)
        );
        assert_eq!(
            d.action(true, &ev(KeyCode::Char('C'), KeyModifiers::SHIFT)),
            Some(Action::NewProject)
        );
        assert_eq!(
            d.action(true, &ev(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            Some(Action::Help)
        );
        assert_eq!(
            d.action(false, &ev(KeyCode::Char('c'), KeyModifiers::NONE)),
            None,
            "needs the prefix"
        );
        let (m, warn) = Keymap::from_pairs(&section(
            "[ui]\nnext_tab = \"x\"\n[keys]\n# comment\nprefix = \"ctrl+a\"\nnext_tab = \"alt+right\" # direct\nhelp = \"prefix+f1\"\nbogus = \"x\"\nprev_tab = \"ctrl+\"\n",
            "keys",
        ));
        assert_eq!(
            m.prefix,
            Key {
                code: KeyCode::Char('a'),
                mods: KeyModifiers::CONTROL
            }
        );
        assert_eq!(
            m.action(false, &ev(KeyCode::Right, KeyModifiers::ALT)),
            Some(Action::NextTab)
        );
        assert_eq!(
            m.action(true, &ev(KeyCode::F(1), KeyModifiers::NONE)),
            Some(Action::Help)
        );
        assert_eq!(
            m.action(true, &ev(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(Action::PrevTab),
            "bad value keeps the default"
        );
        assert!(
            warn.iter().any(|w| w.contains("bogus")) && warn.iter().any(|w| w.contains("prev_tab")),
            "{warn:?}"
        );
        assert_eq!(m.label(Action::NextTab), "alt+right");
        assert_eq!(
            d.action(false, &ev(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(Action::ImagePaste)
        );
        let (off, warn) = Keymap::from_pairs(&[("image_paste".into(), "".into())]);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(
            off.action(false, &ev(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            None,
            "an empty value turns it off"
        );
        assert_eq!(off.label(Action::ImagePaste), "");
        assert_eq!(d.label(Action::NewProject), "ctrl+b shift+c");
    }
}
