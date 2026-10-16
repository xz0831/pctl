//! The terminal view: tmux's own screen (`capture-pane -e`) turned into styled runs, and key
//! events turned into `send-keys` arguments. tmux is the terminal emulator; this only redraws.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};

/// One screen row: runs of text with their style.
pub type Row = Vec<(String, Style)>;

/// Rows of `capture-pane -p -e` output. SGR sequences set the style; OSC strings (hyperlinks)
/// and any other escape are dropped.
pub fn parse_capture(text: &str) -> Vec<Row> {
    text.split('\n').map(parse_line).collect()
}

fn parse_line(line: &str) -> Row {
    let (mut row, mut style, mut text) = (Row::new(), Style::default(), String::new());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() {
                text.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut last = None;
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        last = Some(d);
                        break;
                    }
                    params.push(d);
                }
                if last == Some('m') {
                    if !text.is_empty() {
                        row.push((std::mem::take(&mut text), style));
                    }
                    style = sgr(style, &params);
                }
            }
            Some(']') => {
                while let Some(d) = chars.next() {
                    if d == '\x07' {
                        break;
                    }
                    if d == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if !text.is_empty() {
        row.push((text, style));
    }
    row
}

/// Apply one SGR parameter list. Colon sub-parameters (`4:3`, `38:2::r:g:b`) are understood.
fn sgr(mut s: Style, params: &str) -> Style {
    if params.is_empty() {
        return Style::default();
    }
    let parts: Vec<&str> = params.split(';').collect();
    let num = |p: Option<&&str>| p.and_then(|v| v.parse::<u8>().ok());
    let mut i = 0;
    while i < parts.len() {
        let (head, sub) = match parts[i].split_once(':') {
            Some((h, rest)) => (h, Some(rest)),
            None => (parts[i], None),
        };
        let n: u16 = head.parse().unwrap_or(0);
        match n {
            0 => s = Style::default(),
            1 => s = s.add_modifier(Modifier::BOLD),
            2 => s = s.add_modifier(Modifier::DIM),
            3 => s = s.add_modifier(Modifier::ITALIC),
            4 if sub == Some("0") => s = s.remove_modifier(Modifier::UNDERLINED),
            4 => s = s.add_modifier(Modifier::UNDERLINED),
            5 | 6 => s = s.add_modifier(Modifier::SLOW_BLINK),
            7 => s = s.add_modifier(Modifier::REVERSED),
            8 => s = s.add_modifier(Modifier::HIDDEN),
            9 => s = s.add_modifier(Modifier::CROSSED_OUT),
            21 | 22 => s = s.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => s = s.remove_modifier(Modifier::ITALIC),
            24 => s = s.remove_modifier(Modifier::UNDERLINED),
            25 => s = s.remove_modifier(Modifier::SLOW_BLINK),
            27 => s = s.remove_modifier(Modifier::REVERSED),
            28 => s = s.remove_modifier(Modifier::HIDDEN),
            29 => s = s.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => s = s.fg(Color::Indexed((n - 30) as u8)),
            39 => s = s.fg(Color::Reset),
            40..=47 => s = s.bg(Color::Indexed((n - 40) as u8)),
            49 => s = s.bg(Color::Reset),
            90..=97 => s = s.fg(Color::Indexed((n - 90 + 8) as u8)),
            100..=107 => s = s.bg(Color::Indexed((n - 100 + 8) as u8)),
            38 | 48 | 58 => {
                let color = match sub {
                    // 38:5:n or 38:2:[colorspace]:r:g:b
                    Some(rest) => {
                        let v: Vec<&str> = rest.split(':').collect();
                        match v.first().copied() {
                            Some("5") => num(v.get(1)).map(Color::Indexed),
                            Some("2") => {
                                let t = &v[v.len().saturating_sub(3)..];
                                match (num(t.first()), num(t.get(1)), num(t.get(2))) {
                                    (Some(r), Some(g), Some(b)) if v.len() >= 4 => {
                                        Some(Color::Rgb(r, g, b))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    }
                    None => match parts.get(i + 1).copied() {
                        Some("5") => {
                            i += 2;
                            num(parts.get(i)).map(Color::Indexed)
                        }
                        Some("2") => {
                            let c = (
                                num(parts.get(i + 2)),
                                num(parts.get(i + 3)),
                                num(parts.get(i + 4)),
                            );
                            i += 4;
                            match c {
                                (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r, g, b)),
                                _ => None,
                            }
                        }
                        _ => None,
                    },
                };
                match (n, color) {
                    (38, Some(c)) => s = s.fg(c),
                    (48, Some(c)) => s = s.bg(c),
                    _ => {} // 58 (underline colour) has no ratatui equivalent
                }
            }
            _ => {}
        }
        i += 1;
    }
    s
}

/// `send-keys` arguments for a key press: `(true, text)` to send literally with `-l`, or
/// `(false, name)` for a tmux key name such as `C-c`, `Enter` or `M-Up`.
pub fn key_args(k: &KeyEvent) -> Option<(bool, String)> {
    let m = k.modifiers;
    let mut prefix = String::new();
    if m.contains(KeyModifiers::CONTROL) {
        prefix.push_str("C-");
    }
    if m.contains(KeyModifiers::ALT) {
        prefix.push_str("M-");
    }
    let name = match k.code {
        KeyCode::Char(' ') if !prefix.is_empty() => "Space".to_string(),
        KeyCode::Char(c) if prefix.is_empty() => return Some((true, c.to_string())),
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Backspace => "BSpace".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => return Some((false, "BTab".into())),
        KeyCode::Esc => "Escape".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PPage".into(),
        KeyCode::PageDown => "NPage".into(),
        KeyCode::Delete => "DC".into(),
        KeyCode::Insert => "IC".into(),
        KeyCode::F(n) => format!("F{n}"),
        _ => return None,
    };
    if m.contains(KeyModifiers::SHIFT) && !matches!(k.code, KeyCode::Char(_)) {
        prefix.push_str("S-");
    }
    Some((false, format!("{prefix}{name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_styles_and_drops_other_escapes() {
        let rows = parse_capture(
            "\x1b[1m\x1b[31mbold red\x1b[0m plain \x1b[38;5;208mor\x1b[48;2;10;20;30mrgb\x1b[39m\x1b[49m \x1b[4:3mcurly\x1b[0m 한글\n\x1b]8;;http://x.invalid\x1b\\link\x1b]8;;\x1b\\ end",
        );
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, ["bold red plain orrgb curly 한글", "link end"]);
        let r = &rows[0];
        assert_eq!(
            r[0].1,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Indexed(1))
        );
        assert_eq!(r[1].1, Style::default());
        assert_eq!(r[2].1.fg, Some(Color::Indexed(208)));
        assert_eq!(r[3].1.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(r[5].1.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(
            parse_capture("\x1b[38:2::1:2:3mx")[0][0].1.fg,
            Some(Color::Rgb(1, 2, 3))
        );
        assert_eq!(
            parse_capture("\x1b[38;5mx")[0][0].0,
            "x",
            "truncated colour does not panic"
        );
    }
    #[test]
    fn keys_become_tmux_names() {
        let k = |code, m| key_args(&KeyEvent::new(code, m));
        assert_eq!(
            k(KeyCode::Char('a'), KeyModifiers::NONE),
            Some((true, "a".into()))
        );
        assert_eq!(
            k(KeyCode::Char('A'), KeyModifiers::SHIFT),
            Some((true, "A".into()))
        );
        assert_eq!(
            k(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some((false, "C-c".into()))
        );
        assert_eq!(
            k(KeyCode::Char('x'), KeyModifiers::ALT),
            Some((false, "M-x".into()))
        );
        assert_eq!(
            k(KeyCode::Char(' '), KeyModifiers::CONTROL),
            Some((false, "C-Space".into()))
        );
        assert_eq!(
            k(KeyCode::Enter, KeyModifiers::NONE),
            Some((false, "Enter".into()))
        );
        assert_eq!(
            k(KeyCode::Up, KeyModifiers::SHIFT),
            Some((false, "S-Up".into()))
        );
        assert_eq!(
            k(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some((false, "BTab".into()))
        );
        assert_eq!(
            k(KeyCode::F(5), KeyModifiers::NONE),
            Some((false, "F5".into()))
        );
    }
}
