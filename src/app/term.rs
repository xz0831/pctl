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

/// A cell of the view: row, then column.
pub type Cell = (u16, u16);

/// The characters of a row, each with the cell column it starts at and how many cells it takes.
pub fn row_cells(row: &Row) -> Vec<(char, usize, usize)> {
    let mut out = Vec::new();
    let mut x = 0;
    for (text, _) in row {
        for c in text.chars() {
            let w = cells(c);
            out.push((c, x, w));
            x += w;
        }
    }
    out
}

/// Cells one character takes: 2 for the wide ranges tmux also pads, 0 for combining marks, else 1.
/// The selection and the highlight must count columns the way the screen does.
fn cells(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// The text a selection covers, read from the rows of the view: the whole rows between the first
/// and the last, the first from its start column and the last to its end column. Each line loses
/// its trailing spaces; a character is copied once however many cells it takes. Either end may
/// come first, so a drag upwards or leftwards reads the same.
pub fn selection_text(rows: &[Row], a: Cell, b: Cell) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let ((r0, c0), (r1, c1)) = ((a.0 as usize, a.1 as usize), (b.0 as usize, b.1 as usize));
    let mut lines = Vec::new();
    let last = r1.min(rows.len() - 1);
    for (r, row) in rows.iter().enumerate().take(last + 1).skip(r0) {
        let start = if r == r0 { c0 } else { 0 };
        let end = if r == r1 { c1 } else { usize::MAX };
        let text: String = row_cells(row)
            .into_iter()
            .filter(|(_, x, w)| x + w > start && *x <= end)
            .map(|(c, _, _)| c)
            .collect();
        lines.push(text.trim_end_matches(' ').to_string());
    }
    lines.join("\n")
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 base64 with `=` padding, for the OSC 52 clipboard write. This is the whole encoder:
/// the app has no base64 crate.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
    }
    out
}

/// The reverse of `base64`: strict RFC 4648 (padded, no whitespace, no other characters), or None.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (i, quad) in b.chunks(4).enumerate() {
        // Only the last group may be padded, with at most two `=`.
        let pad = if (i + 1) * 4 == b.len() {
            quad.iter().rev().take_while(|c| **c == b'=').count()
        } else {
            0
        };
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for c in &quad[..4 - pad] {
            let sextet = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            n = n << 6 | u32::from(sextet);
        }
        n <<= 6 * pad;
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(s: &str) -> Row {
        vec![(s.to_string(), Style::default())]
    }
    #[test]
    fn unbase64_reads_the_rfc4648_vectors_and_what_base64_writes() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(unbase64(coded), Some(plain.as_bytes().to_vec()), "{coded}");
        }
        let all: Vec<u8> = (0..=255).collect();
        for len in 0..=all.len() {
            assert_eq!(
                unbase64(&base64(&all[..len])),
                Some(all[..len].to_vec()),
                "{len}"
            );
        }
        for bad in [
            "Zg=", "Zg", "Z===", "====", "Zg==Zg==", "Zm9v\n", "Zm 9", "Zm9-", "Zm9_", "Zm9é",
            "Zg=a",
        ] {
            assert_eq!(unbase64(bad), None, "{bad:?}");
        }
    }
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("한글", "7ZWc6riA"),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain}");
        }
        let all: Vec<u8> = (0..=255).collect();
        let coded = base64(&all);
        assert_eq!(
            coded.len(),
            344,
            "ceil(256/3)*4 bytes for every byte there is"
        );
        assert!(
            coded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
        );
        assert_eq!(base64(&[0]), "AA==");
        assert_eq!(
            base64(&[0xff, 0xff, 0xff]),
            "////",
            "a whole group needs no padding"
        );
    }
    #[test]
    fn selection_takes_whole_lines_between_the_ends() {
        let rows = vec![row("first line here  "), row("second"), row("third line")];
        assert_eq!(
            selection_text(&rows, (0, 6), (2, 4)),
            "line here\nsecond\nthird"
        );
        assert_eq!(
            selection_text(&rows, (2, 4), (0, 6)),
            "line here\nsecond\nthird",
            "dragging up and left reads the same"
        );
        assert_eq!(selection_text(&rows, (1, 0), (1, 5)), "second");
        assert_eq!(selection_text(&rows, (0, 0), (0, 4)), "first");
        assert_eq!(
            selection_text(&rows, (1, 3), (2, 99)),
            "ond\nthird line",
            "an end column past the line is the line's end"
        );
        assert_eq!(
            selection_text(&rows, (1, 0), (1, 9)),
            "second",
            "trailing blank cells add nothing"
        );
    }
    #[test]
    fn a_wide_character_is_copied_once() {
        let rows = vec![row("한글 wide")];
        assert_eq!(
            selection_text(&rows, (0, 0), (0, 3)),
            "한글",
            "two wide characters take four cells"
        );
        assert_eq!(
            selection_text(&rows, (0, 1), (0, 2)),
            "한글",
            "a column inside one copies it once"
        );
        assert_eq!(selection_text(&rows, (0, 5), (0, 8)), "wide");
    }
    #[test]
    fn selection_reads_the_rows_the_view_holds_and_trims_spaces() {
        // While scrolled back the view holds the older rows; the text comes from those, not the pane.
        let rows = vec![row("old line"), row("newer")];
        assert_eq!(selection_text(&rows, (0, 0), (0, 2)), "old");
        assert_eq!(selection_text(&[], (0, 0), (3, 9)), "");
        assert_eq!(
            selection_text(&[row("blank row     ")], (0, 0), (0, 14)),
            "blank row",
            "trailing spaces come off"
        );
        assert_eq!(
            selection_text(&[row("          ")], (0, 0), (0, 9)),
            "",
            "a row of spaces copies nothing"
        );
    }
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
