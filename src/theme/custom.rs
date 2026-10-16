//! Your own themes: one TOML file each in `$XDG_CONFIG_HOME/pctl/themes` or
//! `~/.config/pctl/themes`, named after the file. `pctl theme --new NAME [--from BASE]` writes one
//! filled from a built-in theme; change the colours and the app's theme picker shows it.
//!
//! shortcut: a flat `key = "value"` subset of TOML (plus one `ansi = [...]` line), not a full
//! parser; a real one if themes grow sections.
use super::{Palette, ThemeName};
use anyhow::{Context, Result, ensure};
use ratatui::style::Color;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Custom {
    pub name: String,
    pub about: String,
    pub palette: Palette,
}

const KEYS: [(&str, &str); 13] = [
    ("bg", "app background"),
    ("panel", "sidebar, tab bar and status line"),
    ("raised", "selected rows, dialogs and buttons"),
    ("text", "normal text"),
    ("muted", "quiet text and hints"),
    ("border", "lines and the sidebar edge"),
    ("accent", "highlights and the selected agent"),
    ("cyan", "working agents"),
    ("warn", "notices"),
    ("danger", "agents waiting for you"),
    ("ink", "text on accent-coloured buttons"),
    ("term_bg", "terminal background"),
    ("term_fg", "terminal text"),
];

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("pctl").join("themes"))
}

fn slot<'a>(p: &'a mut Palette, key: &str) -> Option<&'a mut Color> {
    Some(match key {
        "bg" => &mut p.bg,
        "panel" => &mut p.panel,
        "raised" => &mut p.raised,
        "text" => &mut p.text,
        "muted" => &mut p.muted,
        "border" => &mut p.border,
        "accent" => &mut p.accent,
        "cyan" => &mut p.cyan,
        "warn" => &mut p.warn,
        "danger" => &mut p.danger,
        "ink" => &mut p.ink,
        "term_bg" => &mut p.term_bg,
        "term_fg" => &mut p.term_fg,
        _ => return None,
    })
}
fn hex(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "default".into(),
    }
}
fn color(v: &str) -> Option<Color> {
    let v = v.trim().trim_matches('"');
    if v == "default" {
        return Some(Color::Reset);
    }
    let h = v
        .strip_prefix('#')
        .filter(|h| h.len() == 6 && h.is_ascii())?;
    let n = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color::Rgb(n(0)?, n(2)?, n(4)?))
}
pub(crate) fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Read one theme file. Problems become warnings; anything missing comes from `base`.
pub fn parse(name: &str, text: &str) -> (Custom, Vec<String>) {
    let mut warn = Vec::new();
    let pairs: Vec<(String, String)> = text
        .lines()
        .map(|l| l.split(" #").next().unwrap_or("").trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let get = |k: &str| {
        pairs
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    let base = match get("base") {
        Some(b) => ThemeName::from_name(b.trim_matches('"')).unwrap_or_else(|| {
            warn.push(format!(
                "{name}: base {b} is not a built-in theme; using portal"
            ));
            ThemeName::Portal
        }),
        None => ThemeName::Portal,
    };
    let mut palette = base.palette();
    for (k, v) in &pairs {
        match k.as_str() {
            "base" | "about" | "ansi" => {}
            key => match (slot(&mut palette, key), color(v)) {
                (Some(s), Some(c)) => *s = c,
                (Some(_), None) => warn.push(format!("{name}: {key} = {v} is not #rrggbb")),
                (None, _) => warn.push(format!("{name}: unknown key {key}")),
            },
        }
    }
    if let Some(v) = get("ansi") {
        let list: Vec<Option<Color>> = v
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(color)
            .collect();
        match (list.len(), list.iter().all(Option::is_some)) {
            (16, true) => {
                let mut a = [Color::Reset; 16];
                for (i, c) in list.into_iter().enumerate() {
                    a[i] = c.unwrap_or(Color::Reset);
                }
                palette.ansi = Some(a);
            }
            _ => warn.push(format!("{name}: ansi needs 16 #rrggbb colours")),
        }
    }
    let about = get("about")
        .map(|a| a.trim_matches('"').to_string())
        .unwrap_or_else(|| "your own theme".into());
    (
        Custom {
            name: name.into(),
            about,
            palette,
        },
        warn,
    )
}

/// Every readable theme file, sorted by name, and what was wrong with the others.
pub fn load_all() -> (Vec<Custom>, Vec<String>) {
    let mut out = Vec::new();
    let mut warn = Vec::new();
    let Some(dir) = dir() else { return (out, warn) };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (out, warn);
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !plain(&name) || ThemeName::from_name(&name).is_some() {
            warn.push(format!(
                "{}: theme names use a-z, 0-9, - and _ and differ from built-ins",
                path.display()
            ));
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(m) if m.len() <= 16 * 1024 => {}
            _ => {
                warn.push(format!("{name}: file missing or over 16 KiB"));
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (c, w) = parse(&name, &text);
        warn.extend(w);
        out.push(c);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, warn)
}

/// A theme file with every colour of `base`, each with a comment saying where it shows.
pub fn template(name: &str, base: ThemeName) -> String {
    let p = base.palette();
    let mut probe = p;
    let mut s = format!(
        "# pctl theme \"{name}\", made from {base}. Change any colour (#rrggbb, or \"default\" for\n# the terminal's own), save, and pick \"{name}\" from ◐ Theme in pctl. Delete a line to\n# keep {base}'s colour.\nbase = \"{base}\"\nabout = \"made from {base}; say what makes it yours\"\n\n",
        base = base.label().to_lowercase()
    );
    for (key, what) in KEYS {
        let c = slot(&mut probe, key).map(|c| hex(*c)).unwrap_or_default();
        s.push_str(&format!("{key:<8} = \"{c}\"   # {what}\n"));
    }
    if let Some(a) = p.ansi {
        let list: Vec<String> = a.iter().map(|c| format!("\"{}\"", hex(*c))).collect();
        s.push_str("\n# The 16 terminal colours: black red green yellow blue magenta cyan white, then bright.\n");
        s.push_str(&format!("ansi = [{}]\n", list.join(", ")));
    }
    s
}

/// Write a new theme file for `name`; refuses to overwrite one.
pub fn create(name: &str, base: ThemeName) -> Result<PathBuf> {
    ensure!(
        plain(name),
        "theme names use a-z, 0-9, - and _ (at most 40)"
    );
    ensure!(
        ThemeName::from_name(name).is_none(),
        "{name} is a built-in theme; pick another name"
    );
    let dir = dir().context("no HOME or XDG_CONFIG_HOME for the theme folder")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.toml"));
    ensure!(
        !path.exists(),
        "{} already exists; edit it instead",
        path.display()
    );
    std::fs::write(&path, template(name, base))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_template_reads_back_as_its_base_and_edits_apply() {
        let t = template("mine", ThemeName::Nebula);
        let (c, warn) = parse("mine", &t);
        assert!(warn.is_empty(), "{warn:?}");
        let n = ThemeName::Nebula.palette();
        assert_eq!(
            (c.palette.bg, c.palette.term_bg, c.palette.ansi),
            (n.bg, n.term_bg, n.ansi)
        );
        let (c, warn) = parse(
            "mine",
            "base = \"lab\"\naccent = \"#123456\"\nterm_bg = \"default\"\nbogus = \"#000000\"\ntext = \"blue\"\nansi = [\"#000000\"]\n",
        );
        assert_eq!(c.palette.accent, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(c.palette.term_bg, Color::Reset);
        assert_eq!(
            c.palette.bg,
            ThemeName::Lab.palette().bg,
            "unset keys come from the base"
        );
        assert_eq!(warn.len(), 3, "{warn:?}");
        assert!(plain("my-theme_2") && !plain("My Theme") && !plain(""));
    }
}
