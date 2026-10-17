//! Source-addressed Markdown rendering.
extern crate self as mdv;
pub mod lens;
pub mod reader;
pub mod threads;
pub mod view;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::{collections::HashSet, ops::Range};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Heading {
        level: u8,
    },
    Paragraph,
    List,
    Code {
        language: String,
    },
    Table,
    Quote,
    Math,
    Mermaid,
    RawHtml,
    Rule,
    Image,
    Footnote,
    FrontMatter,
    /// Addressable source-only definition; no reader focus or rendered rows.
    Definition,
}
#[derive(Debug, Clone)]
pub struct Block {
    pub id: usize,
    pub kind: Kind,
    pub range: Range<usize>,
    /// Heading text from ancestors, including this block when it is a heading.
    pub heading_path: Vec<String>,
    /// Deterministic FNV-1a of exact source bytes; a locator, not a security hash.
    pub fingerprint: u64,
    event_range: Range<usize>,
}
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    events: Vec<(Event<'static>, Range<usize>)>,
}
#[derive(Debug, Clone)]
pub struct Line {
    pub styled: String,
    pub plain: String,
    pub block_id: usize,
    pub horizontally_scrollable: bool,
    /// Rendered byte boundaries mapped to canonical block text for wrapped tables.
    pub source_offsets: Option<Vec<usize>>,
}
/// All renderer-generated ANSI styles and decorative glyphs live here.
#[derive(Debug, Clone)]
pub struct Style {
    /// Maximum terminal prose width in display cells.
    pub measure: usize,
    pub heading: String,
    pub code: String,
    pub quote: String,
    pub table: String,
    pub body: String,
    pub reset: String,
    pub bullet: String,
    pub quote_prefix: String,
    pub table_separator: String,
    pub table_rule: String,
    pub checked: String,
    pub unchecked: String,
    pub rule: String,
    pub image_prefix: String,
    pub paper: String,
    pub ink: String,
    pub focus_gutter: String,
    pub idle_gutter: String,
    pub selected: String,
    pub separator: String,
    pub return_mark: String,
    pub broken_mark: String,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            measure: 80,
            heading: "\x1b[1;34m".into(),
            code: "\x1b[38;5;242m".into(),
            quote: "\x1b[38;5;244m".into(),
            table: "\x1b[38;5;240m".into(),
            body: String::new(),
            reset: "\x1b[0m".into(),
            bullet: "• ".into(),
            quote_prefix: "│ ".into(),
            table_separator: " | ".into(),
            table_rule: "-".into(),
            checked: "[x] ".into(),
            unchecked: "[ ] ".into(),
            rule: "--------".into(),
            image_prefix: "Image: ".into(),
            paper: "#FCFCFB".into(),
            ink: "#0B0B0B".into(),
            focus_gutter: "> ".into(),
            idle_gutter: "  ".into(),
            selected: "\x1b[7m".into(),
            separator: " | ".into(),
            return_mark: "+>".into(),
            broken_mark: "+ / >".into(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Portal,
    Garage,
}
impl Style {
    pub fn for_look(look: Look, dark: bool) -> Self {
        let mut style = Self {
            paper: if dark { "#151918" } else { "#FCFCFB" }.into(),
            ink: if dark { "#E8ECE4" } else { "#0B0B0B" }.into(),
            ..Self::default()
        };
        let accent = match (look, dark) {
            (Look::Portal, false) => "\x1b[38;2;56;107;39m",
            (Look::Portal, true) => "\x1b[38;2;168;216;102m",
            (Look::Garage, false) => "\x1b[38;2;0;102;108m",
            (Look::Garage, true) => "\x1b[38;2;120;205;210m",
        };
        style.heading = "\x1b[1m".into();
        style.quote = accent.into();
        style.code = if dark {
            "\x1b[38;2;172;182;170m"
        } else {
            "\x1b[38;2;85;94;82m"
        }
        .into();
        style.table = style.code.clone();
        style.focus_gutter = if look == Look::Portal { "[ " } else { "| " }.into();
        style.return_mark = if look == Look::Portal {
            "+>"
        } else {
            "+--[ 답변 ]/"
        }
        .into();
        style.broken_mark = if look == Look::Portal {
            "+ / >"
        } else {
            "+-- / [ 연결 끊김 ]/"
        }
        .into();
        if look == Look::Portal {
            // Portal Lab document colors; chrome accents stay readable on each paper.
            style.paper = if dark { "#182125" } else { "#F3F2E7" }.into();
            style.ink = if dark { "#E8EFE9" } else { "#222A27" }.into();
            style.quote = if dark {
                "\x1b[38;2;158;225;233m"
            } else {
                "\x1b[38;2;31;101;113m"
            }
            .into();
            style.code = if dark {
                "\x1b[38;2;183;198;190m"
            } else {
                "\x1b[38;2;88;99;93m"
            }
            .into();
            style.table = style.code.clone();
        }
        style
    }
}
/// Only relative destinations are rewritten. Rewritten URLs are validated again.
/// Paths (including NFD names) remain unchanged unless the caller rewrites them.
pub struct LinkMap<'a> {
    pub rewrite: &'a dyn Fn(&str) -> String,
}
impl Default for LinkMap<'static> {
    fn default() -> Self {
        Self {
            rewrite: &identity_link,
        }
    }
}
fn identity_link(s: &str) -> String {
    s.to_owned()
}
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}
fn fingerprint(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}
fn kind(tag: &Tag<'_>) -> Kind {
    match tag {
        Tag::Heading { level, .. } => Kind::Heading {
            level: *level as u8,
        },
        Tag::List(_) => Kind::List,
        Tag::BlockQuote(_) => Kind::Quote,
        Tag::CodeBlock(CodeBlockKind::Fenced(info)) => {
            let language = info.split_whitespace().next().unwrap_or("").to_owned();
            if language.eq_ignore_ascii_case("mermaid") {
                Kind::Mermaid
            } else {
                Kind::Code { language }
            }
        }
        Tag::CodeBlock(_) => Kind::Code {
            language: String::new(),
        },
        Tag::Table(_) => Kind::Table,
        Tag::HtmlBlock => Kind::RawHtml,
        Tag::FootnoteDefinition(_) => Kind::Footnote,
        Tag::MetadataBlock(_) => Kind::FrontMatter,
        Tag::Image { .. } => Kind::Image,
        _ => Kind::Paragraph,
    }
}
pub fn parse(src: &str) -> Doc {
    let parser = Parser::new_ext(src, options());
    let definitions: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, d)| d.span.clone())
        .collect();
    let events: Vec<_> = parser
        .into_offset_iter()
        .map(|(e, r)| (e.into_static(), r))
        .collect();
    let mut doc = Doc::default();
    let mut depth = 0;
    let mut start = 0;
    let mut first = 0;
    let mut current = Kind::Paragraph;
    let mut headings: Vec<(u8, String)> = Vec::new();
    for (i, (event, range)) in events.iter().enumerate() {
        let completed = match event {
            Event::Start(tag) => {
                if depth == 0 {
                    start = range.start;
                    first = i;
                    current = kind(tag);
                }
                depth += 1;
                None
            }
            Event::End(_) => {
                depth -= 1;
                (depth == 0).then_some(start..range.end)
            }
            Event::Rule if depth == 0 => {
                current = Kind::Rule;
                first = i;
                Some(range.clone())
            }
            Event::DisplayMath(_) if depth == 0 => {
                current = Kind::Math;
                first = i;
                Some(range.clone())
            }
            Event::Html(_) if depth == 0 => {
                current = Kind::RawHtml;
                first = i;
                Some(range.clone())
            }
            _ => None,
        };
        if let Some(mut range) = completed {
            let line_start = src[..range.start].rfind('\n').map_or(0, |n| n + 1);
            if src[line_start..range.start]
                .chars()
                .all(|c| c == ' ' || c == '\t')
            {
                range.start = line_start;
            }
            if current == Kind::Paragraph {
                let meaningful: Vec<_> = events[first..=i]
                    .iter()
                    .filter(|(e, _)| {
                        !matches!(
                            e,
                            Event::Start(Tag::Paragraph)
                                | Event::End(TagEnd::Paragraph)
                                | Event::SoftBreak
                        )
                    })
                    .collect();
                if meaningful.len() == 1 && matches!(meaningful[0].0, Event::DisplayMath(_)) {
                    current = Kind::Math;
                } else if matches!(
                    meaningful.first().map(|x| &x.0),
                    Some(Event::Start(Tag::Image { .. }))
                ) && matches!(
                    meaningful.last().map(|x| &x.0),
                    Some(Event::End(TagEnd::Image))
                ) && meaningful
                    .iter()
                    .filter(|(e, _)| matches!(e, Event::Start(Tag::Image { .. })))
                    .count()
                    == 1
                {
                    current = Kind::Image;
                }
            }
            if let Kind::Heading { level } = current {
                let title = events[first..=i]
                    .iter()
                    .filter_map(|(e, _)| match e {
                        Event::Text(s) | Event::Code(s) | Event::InlineMath(s) => Some(s.as_ref()),
                        _ => None,
                    })
                    .collect::<String>();
                while headings.last().is_some_and(|(l, _)| *l >= level) {
                    headings.pop();
                }
                headings.push((level, visible(&title)));
            }
            doc.blocks.push(Block {
                id: doc.blocks.len(),
                kind: current.clone(),
                fingerprint: fingerprint(&src[range.clone()]),
                event_range: first..i + 1,
                range,
                heading_path: headings.iter().map(|(_, s)| s.clone()).collect(),
            });
        }
    }
    // Effective definitions plus un-emitted nonblank gaps (including duplicates).
    let mut covered: Vec<_> = doc.blocks.iter().map(|b| b.range.clone()).collect();
    let mut spans = Vec::new();
    for range in definitions {
        if !covered
            .iter()
            .any(|b| b.start < range.end && range.start < b.end)
        {
            covered.push(range.clone());
            spans.push(range);
        }
    }
    covered.sort_by_key(|r| r.start);
    let mut end = 0;
    for range in covered {
        if end < range.start {
            spans.push(end..range.start);
        }
        end = end.max(range.end);
    }
    if end < src.len() {
        spans.push(end..src.len());
    }
    for mut range in spans {
        while range.start < range.end {
            let next = src[range.clone()]
                .find('\n')
                .map_or(range.end, |n| range.start + n + 1);
            if !src[range.start..next].trim().is_empty() {
                break;
            }
            range.start = next;
        }
        while range.end > range.start && src[range.clone()].ends_with("\n\n") {
            range.end -= 1;
        }
        // Definition spans may omit their final newline; retain that physical terminator.
        if range.end < src.len() && src.as_bytes()[range.end] == b'\n' {
            range.end += 1;
        }
        if src[range.clone()].trim().is_empty() {
            continue;
        }
        doc.blocks.push(Block {
            id: 0,
            kind: Kind::Definition,
            fingerprint: fingerprint(&src[range.clone()]),
            range,
            event_range: 0..0,
            heading_path: Vec::new(),
        });
    }
    doc.blocks.sort_by_key(|b| b.range.start);
    for (id, b) in doc.blocks.iter_mut().enumerate() {
        b.id = id;
    }
    doc.events = events;
    doc
}
/// Newlines are layout; tabs are four spaces; all other C0/C1 controls are visible.
pub fn visible(src: &str) -> String {
    let mut out = String::new();
    for c in src.chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\0'..='\u{1f}' => out.push(char::from_u32(0x2400 + c as u32).unwrap()),
            '\u{7f}' => out.push('␡'),
            '\u{80}'..='\u{9f}' => out.push_str(&format!("⟦U+{:04X}⟧", c as u32)),
            _ => out.push(c),
        }
    }
    out
}
fn escaped(s: &str) -> String {
    visible(s)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Wrap at extended grapheme boundaries, preferring the last space that fits.
/// An indivisible grapheme wider than the viewport is shown as '?' (width 0 emits no text).
fn wrap(s: &str, width: usize, prose: bool) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for logical in s.split('\n') {
        let gs: Vec<_> = logical.graphemes(true).collect();
        if gs.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < gs.len() {
            let mut end = start;
            let mut used = 0;
            let mut space = None;
            while end < gs.len() {
                let w = UnicodeWidthStr::width(gs[end]);
                if used + w > width {
                    break;
                }
                used += w;
                if gs[end] == " " {
                    space = Some(end);
                }
                end += 1;
            }
            if end == start {
                out.push("?".into());
                start += 1;
                continue;
            }
            if end < gs.len()
                && let Some(at) = space.filter(|at| *at > start)
            {
                end = at + 1;
            }
            let text = gs[start..end].concat();
            out.push(if prose && end < gs.len() {
                text.trim_end_matches(' ').to_owned()
            } else {
                text
            });
            start = end;
        }
    }
    out
}

// Numeric tokens (including signs, decimals and units without spaces) stay intact.
fn table_cell_lines(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    if UnicodeWidthStr::width(text) <= width {
        return vec![text.to_owned()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for token in text.split_inclusive(char::is_whitespace) {
        let token = token.trim_end();
        if token.is_empty() {
            continue;
        }
        let token_width = UnicodeWidthStr::width(token);
        if !line.is_empty() && UnicodeWidthStr::width(line.as_str()) + 1 + token_width > width {
            out.push(std::mem::take(&mut line));
        }
        if token_width <= width || token.chars().any(|c| c.is_ascii_digit()) {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(token);
        } else {
            let pieces = wrap(token, width, true);
            for piece in &pieces[..pieces.len() - 1] {
                out.push(piece.clone());
            }
            line = pieces.last().cloned().unwrap_or_default();
        }
    }
    out.push(line);
    out
}
fn table_lines(events: &[Event<'_>], style: &Style, available: usize) -> Vec<String> {
    table_render(events, style, available)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}
fn table_render(
    events: &[Event<'_>],
    style: &Style,
    available: usize,
) -> Vec<(String, Vec<usize>)> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut align = Vec::new();
    for e in events {
        match e {
            Event::Start(Tag::Table(a)) => align = a.clone(),
            Event::Start(Tag::TableHead | Tag::TableRow) => row.clear(),
            Event::Start(Tag::TableCell) => cell.clear(),
            Event::Text(s)
            | Event::Code(s)
            | Event::InlineMath(s)
            | Event::DisplayMath(s)
            | Event::Html(s)
            | Event::InlineHtml(s) => cell.push_str(&visible(s).replace('\n', " ")),
            Event::SoftBreak | Event::HardBreak => cell.push(' '),
            Event::TaskListMarker(yes) => cell.push_str(if *yes {
                &style.checked
            } else {
                &style.unchecked
            }),
            Event::FootnoteReference(s) => cell.push_str(&format!("[{}]", visible(s))),
            Event::End(TagEnd::TableCell) => row.push(cell.clone()),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => rows.push(row.clone()),
            _ => {}
        }
    }
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths: Vec<_> = (0..n)
        .map(|i| {
            rows.iter()
                .map(|r| {
                    r.get(i)
                        .map(|s| UnicodeWidthStr::width(s.as_str()))
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        })
        .collect();
    let natural_widths = widths.clone();
    let minimum: Vec<_> = (0..n)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .flat_map(|s| s.split_whitespace())
                .filter(|token| token.chars().any(|c| c.is_ascii_digit()))
                .map(UnicodeWidthStr::width)
                .max()
                .unwrap_or(2)
                .max(2)
                .min(widths[i])
        })
        .collect();
    let separators = n.saturating_sub(1) * UnicodeWidthStr::width(style.table_separator.as_str());
    while widths.iter().sum::<usize>() + separators > available {
        let widest = (0..n)
            .filter(|&i| widths[i] > minimum[i])
            .max_by_key(|&i| widths[i]);
        let Some(i) = widest else {
            break;
        };
        widths[i] -= 1;
    }
    let mut out = Vec::new();
    let mut canonical_offset = 0;
    for (idx, r) in rows.iter().enumerate() {
        let canonical_cells: Vec<_> = (0..n)
            .map(|i| {
                let text = r.get(i).map(String::as_str).unwrap_or("");
                let pad = natural_widths[i] - UnicodeWidthStr::width(text);
                let left = match align.get(i) {
                    Some(Alignment::Right) => pad,
                    Some(Alignment::Center) => pad / 2,
                    _ => 0,
                };
                (
                    format!("{}{}{}", " ".repeat(left), text, " ".repeat(pad - left)),
                    left,
                )
            })
            .collect();
        let canonical_line = canonical_cells
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(&style.table_separator);
        let mut consumed = vec![0; n];
        let wrapped: Vec<_> = (0..n)
            .map(|i| table_cell_lines(r.get(i).map(String::as_str).unwrap_or(""), widths[i]))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for row_line in 0..height {
            let mut mappings = Vec::new();
            let mut cell_start = canonical_offset;
            let cells: Vec<_> = (0..n)
                .map(|i| {
                    let text = wrapped[i].get(row_line).map(String::as_str).unwrap_or("");
                    let pad = widths[i].saturating_sub(UnicodeWidthStr::width(text));
                    let left = match align.get(i) {
                        Some(Alignment::Right) => pad,
                        Some(Alignment::Center) => pad / 2,
                        _ => 0,
                    };
                    let original = r.get(i).map(String::as_str).unwrap_or("");
                    let from = if text.is_empty() {
                        consumed[i]
                    } else {
                        original[consumed[i]..]
                            .find(text)
                            .map_or(consumed[i], |at| consumed[i] + at)
                    };
                    let source = cell_start + canonical_cells[i].1 + from;
                    let mut mapping = vec![source; left];
                    mapping.extend((0..text.len()).map(|byte| source + byte));
                    mapping.extend(std::iter::repeat_n(source + text.len(), pad - left));
                    if i + 1 < n {
                        mapping.extend(
                            (0..style.table_separator.len())
                                .map(|byte| cell_start + canonical_cells[i].0.len() + byte),
                        );
                    }
                    mappings.extend(mapping);
                    consumed[i] = from + text.len();
                    cell_start += canonical_cells[i].0.len() + style.table_separator.len();
                    format!("{}{}{}", " ".repeat(left), text, " ".repeat(pad - left))
                })
                .collect();
            mappings.push(canonical_offset + canonical_line.len());
            out.push((cells.join(&style.table_separator), mappings));
        }
        canonical_offset += canonical_line.len() + 1;
        if idx == 0 {
            let rule = widths
                .iter()
                .map(|w| style.table_rule.repeat(*w))
                .collect::<Vec<_>>()
                .join(&style.table_separator);
            let natural_rule = natural_widths
                .iter()
                .map(|w| style.table_rule.repeat(*w))
                .collect::<Vec<_>>()
                .join(&style.table_separator);
            let mapping = (0..=rule.len())
                .map(|byte| canonical_offset + byte.min(natural_rule.len()))
                .collect();
            out.push((rule, mapping));
            canonical_offset += natural_rule.len() + 1;
        }
    }
    out
}
fn plain_events(events: &[Event<'_>], style: &Style) -> String {
    let mut out = String::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote: usize = 0;
    let mut line_start = true;
    let mut i = 0;
    while i < events.len() {
        let e = &events[i];
        i += 1;
        let mut chunk = String::new();
        match e {
            Event::Start(Tag::Table(_)) => {
                let begin = i - 1;
                while i < events.len() && !matches!(events[i], Event::End(TagEnd::Table)) {
                    i += 1;
                }
                i += 1;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                for row in table_lines(&events[begin..i], style, usize::MAX) {
                    out.push_str(&row);
                    out.push('\n');
                }
                line_start = true;
                continue;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                line_start = true;
            }
            Event::Start(Tag::List(n)) => lists.push(*n),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                line_start = true;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                quote += 1;
                line_start = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote = quote.saturating_sub(1);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                line_start = true;
            }
            Event::Start(Tag::Item) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                chunk.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                if let Some(Some(n)) = lists.last_mut() {
                    chunk.push_str(&format!("{}. ", n));
                    *n = n.saturating_add(1);
                } else {
                    chunk.push_str(&style.bullet);
                }
                line_start = true;
            }
            Event::End(
                TagEnd::Item | TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock,
            ) => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                line_start = true;
            }
            Event::Text(s)
            | Event::Code(s)
            | Event::InlineMath(s)
            | Event::DisplayMath(s)
            | Event::Html(s)
            | Event::InlineHtml(s) => chunk = visible(s),
            Event::SoftBreak => chunk.push(' '),
            Event::HardBreak => chunk.push('\n'),
            Event::TaskListMarker(yes) => chunk.push_str(if *yes {
                &style.checked
            } else {
                &style.unchecked
            }),
            Event::Rule => chunk.push_str(&style.rule),
            Event::FootnoteReference(s) => chunk.push_str(&format!("[{}]", visible(s))),
            Event::Start(Tag::FootnoteDefinition(s)) => {
                chunk.push_str(&format!("[{}]: ", visible(s)))
            }
            Event::Start(Tag::Image { .. }) => chunk.push_str(&style.image_prefix),
            Event::End(TagEnd::Image) => {}
            _ => {}
        }
        if !chunk.is_empty() {
            if line_start && quote > 0 {
                out.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                out.push_str(&style.quote_prefix.repeat(quote));
            }
            line_start = chunk.ends_with('\n');
            out.push_str(&chunk);
        }
    }
    out.trim_end_matches('\n').to_owned()
}
pub fn to_ansi(doc: &Doc, _src: &str, width: usize, style: &Style) -> Vec<Line> {
    let events = &doc.events;
    let mut out = Vec::new();
    for block in &doc.blocks {
        if block.kind == Kind::Definition {
            continue;
        }
        let es: Vec<_> = events[block.event_range.clone()]
            .iter()
            .map(|(e, _)| e.clone())
            .collect();
        let mut raw = Vec::new();
        let mut begin = 0;
        let mut i = 0;
        while i < es.len() {
            let table = matches!(es[i], Event::Start(Tag::Table(_)));
            let code = matches!(es[i], Event::Start(Tag::CodeBlock(_)));
            if table || code {
                if begin < i {
                    raw.push((plain_events(&es[begin..i], style), false, true, None));
                }
                let start = i;
                while !matches!(
                    (&es[i], table),
                    (Event::End(TagEnd::Table), true) | (Event::End(TagEnd::CodeBlock), false)
                ) {
                    i += 1;
                }
                if table {
                    let natural = table_lines(&es[start..=i], style, usize::MAX).join("\n");
                    let base = block_text(doc, block.id, style).find(&natural).unwrap_or(0);
                    raw.extend(table_render(&es[start..=i], style, width).into_iter().map(
                        |(s, offsets)| {
                            (
                                s,
                                true,
                                false,
                                Some(offsets.into_iter().map(|at| at + base).collect::<Vec<_>>()),
                            )
                        },
                    ));
                } else {
                    raw.push((plain_events(&es[start..=i], style), false, false, None));
                }
                begin = i + 1;
            }
            i += 1;
        }
        if begin < es.len() {
            raw.push((
                plain_events(&es[begin..], style),
                false,
                !matches!(block.kind, Kind::RawHtml | Kind::FrontMatter),
                None,
            ));
        }
        let ansi = match block.kind {
            Kind::Heading { .. } => &style.heading,
            Kind::Code { .. } | Kind::Mermaid | Kind::Math => &style.code,
            Kind::Quote => &style.quote,
            Kind::Table => &style.table,
            _ => &style.body,
        };
        for (text, table, prose, source_offsets) in raw {
            let lines = if table {
                vec![text]
            } else {
                if prose {
                    wrap(&text, width.min(style.measure), true)
                } else {
                    text.split('\n').map(str::to_owned).collect()
                }
            };
            for plain in lines {
                let scroll = (table || !prose) && UnicodeWidthStr::width(plain.as_str()) > width;
                let styled = if ansi.is_empty() {
                    plain.clone()
                } else {
                    format!("{}{}{}", ansi, plain, style.reset)
                };
                out.push(Line {
                    styled,
                    plain,
                    block_id: block.id,
                    horizontally_scrollable: scroll,
                    source_offsets: source_offsets.clone(),
                });
            }
        }
        if block.id + 1 < doc.blocks.len() {
            out.push(Line {
                styled: String::new(),
                plain: String::new(),
                block_id: block.id,
                horizontally_scrollable: false,
                source_offsets: None,
            });
        }
    }
    out
}

#[derive(PartialEq)]
enum UrlKind {
    Remote,
    Relative,
    Anchor,
    Mail,
}
fn url_kind(s: &str) -> Option<UrlKind> {
    // Browser URL parsing ignores some controls; reject them before classification.
    if s.chars().any(char::is_control) || s.trim() != s || s.contains('\\') {
        return None;
    }
    if s.starts_with('#') {
        return Some(UrlKind::Anchor);
    }
    if s.starts_with("//") {
        return Some(UrlKind::Remote);
    }
    if let Some(colon) = s.find(':') {
        let boundary = s.find(['/', '?', '#']).unwrap_or(s.len());
        if colon < boundary {
            return match s[..colon].to_ascii_lowercase().as_str() {
                "http" | "https" => Some(UrlKind::Remote),
                "mailto" => Some(UrlKind::Mail),
                _ => None,
            };
        }
    }
    Some(UrlKind::Relative)
}
fn link(s: &str, map: &LinkMap<'_>) -> Option<(String, UrlKind)> {
    let k = url_kind(s)?;
    let rewritten = if k == UrlKind::Relative {
        (map.rewrite)(s)
    } else {
        s.into()
    };
    let k = url_kind(&rewritten)?;
    Some((rewritten, k))
}
fn slug(text: &str, id: usize, used: &mut HashSet<String>) -> String {
    let mut base = String::new();
    let mut sep = false;
    for c in text.nfc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' {
            if sep && !base.is_empty() {
                base.push('-');
            }
            base.push(c);
            sep = false;
        } else {
            sep = true;
        }
    }
    if base.is_empty() {
        base = format!("heading-{id}");
    }
    let mut value = base.clone();
    let mut n = 2;
    while !used.insert(value.clone()) {
        value = format!("{base}-{n}");
        n += 1;
    }
    value
}
fn image_alt(events: &[Event<'_>]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(s) | Event::Code(s) | Event::InlineMath(s) | Event::DisplayMath(s) => {
                Some(visible(s))
            }
            Event::SoftBreak | Event::HardBreak => Some(" ".into()),
            _ => None,
        })
        .collect()
}
/// Render the full parser event stream once, retaining reference links and footnotes.
pub fn to_html(doc: &Doc, _src: &str, map: &LinkMap<'_>) -> String {
    to_html_with_images(doc, _src, map, map)
}
/// Separate local image rewriting from document navigation.
pub fn to_html_with_images(
    doc: &Doc,
    _src: &str,
    map: &LinkMap<'_>,
    images: &LinkMap<'_>,
) -> String {
    let events = &doc.events;
    // Reuse the parser's Unicode label equivalence without a second case-fold implementation.
    let footnotes: String = events
        .iter()
        .filter_map(|(e, _)| match e {
            Event::Start(Tag::FootnoteDefinition(s)) => {
                Some(format!("[{}]: #fn-{}\n", s, footnote_id(s)))
            }
            _ => None,
        })
        .collect();
    let resolver = Parser::new(&footnotes);
    let target = |s: &str| {
        resolver
            .reference_definitions()
            .get(s)
            .map(|d| d.dest.to_string())
            .unwrap_or_else(|| format!("#fn-{}", footnote_id(s)))
    };
    let mut used: HashSet<String> = events
        .iter()
        .filter_map(|(e, _)| match e {
            Event::Start(Tag::FootnoteDefinition(s)) => {
                Some(target(s).trim_start_matches('#').to_owned())
            }
            _ => None,
        })
        .collect();
    let mut out = String::new();
    for block in &doc.blocks {
        if block.kind == Kind::Definition {
            continue;
        }
        let es: Vec<_> = events[block.event_range.clone()]
            .iter()
            .map(|(e, _)| e.clone())
            .collect();
        out.push_str(&format!(
            "<section data-b=\"{}\" data-src=\"{}-{}\">",
            block.id, block.range.start, block.range.end
        ));
        let mut math_paragraphs = HashSet::new();
        let mut paragraph = None;
        for (idx, e) in es.iter().enumerate() {
            match e {
                Event::Start(Tag::Paragraph) => paragraph = Some(idx),
                Event::End(TagEnd::Paragraph) => paragraph = None,
                Event::DisplayMath(_) => {
                    if let Some(p) = paragraph {
                        math_paragraphs.insert(p);
                    }
                }
                _ => {}
            }
        }
        let mut i = 0;
        let mut closes: Vec<String> = Vec::new();
        let mut table_align = Vec::new();
        let mut col = 0;
        let mut head = false;
        while i < es.len() {
            match &es[i] {
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    let begin = i + 1;
                    let mut depth = 1;
                    while depth > 0 {
                        i += 1;
                        match &es[i] {
                            Event::Start(Tag::Image { .. }) => depth += 1,
                            Event::End(TagEnd::Image) => depth -= 1,
                            _ => {}
                        }
                    }
                    let alt = image_alt(&es[begin..i]);
                    match link(dest_url, images) {
                        Some((url, UrlKind::Relative)) => out.push_str(&format!(
                            "<img src=\"{}\" alt=\"{}\" title=\"{}\">",
                            escaped(&url),
                            escaped(&alt),
                            escaped(title)
                        )),
                        Some((url, _)) => out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escaped(&url),
                            escaped(&alt)
                        )),
                        None => out.push_str(&escaped(&alt)),
                    }
                }
                Event::Start(tag) => {
                    let (open, close) = match tag {
                        Tag::Paragraph if block.kind == Kind::Math => {
                            (String::new(), String::new())
                        }
                        Tag::Paragraph if math_paragraphs.contains(&i) => {
                            ("<div class=\"paragraph\">".into(), "</div>".into())
                        }
                        Tag::Paragraph => ("<p>".into(), "</p>".into()),
                        Tag::Heading { level, .. } => {
                            let title = es[i + 1..]
                                .iter()
                                .take_while(|e| !matches!(e, Event::End(TagEnd::Heading(_))))
                                .filter_map(|e| match e {
                                    Event::Text(s) | Event::Code(s) | Event::InlineMath(s) => {
                                        Some(s.as_ref())
                                    }
                                    _ => None,
                                })
                                .collect::<String>();
                            (
                                format!(
                                    "<h{} id=\"{}\">",
                                    *level as u8,
                                    slug(&title, block.id, &mut used)
                                ),
                                format!("</h{}>", *level as u8),
                            )
                        }
                        Tag::BlockQuote(_) => ("<blockquote>".into(), "</blockquote>".into()),
                        Tag::CodeBlock(info) => {
                            let language = match info {
                                CodeBlockKind::Fenced(s) => {
                                    s.split_whitespace().next().unwrap_or("")
                                }
                                _ => "",
                            };
                            if language.eq_ignore_ascii_case("mermaid") {
                                ("<pre class=\"mermaid\">".into(), "</pre>".into())
                            } else {
                                (
                                    format!("<pre><code class=\"language-{}\">", escaped(language)),
                                    "</code></pre>".into(),
                                )
                            }
                        }
                        Tag::HtmlBlock => ("<pre class=\"raw-html\">".into(), "</pre>".into()),
                        Tag::List(Some(n)) => (format!("<ol start=\"{}\">", n), "</ol>".into()),
                        Tag::List(None) => ("<ul>".into(), "</ul>".into()),
                        Tag::Item => ("<li>".into(), "</li>".into()),
                        Tag::Emphasis => ("<em>".into(), "</em>".into()),
                        Tag::Strong => ("<strong>".into(), "</strong>".into()),
                        Tag::Strikethrough => ("<del>".into(), "</del>".into()),
                        Tag::Link {
                            dest_url, title, ..
                        } => match link(dest_url, map) {
                            Some((url, _)) => (
                                format!(
                                    "<a href=\"{}\" title=\"{}\">",
                                    escaped(&url),
                                    escaped(title)
                                ),
                                "</a>".into(),
                            ),
                            None => (String::new(), String::new()),
                        },
                        Tag::Table(a) => {
                            table_align = a.clone();
                            ("<table>".into(), "</tbody></table>".into())
                        }
                        Tag::TableHead => {
                            head = true;
                            col = 0;
                            ("<thead><tr>".into(), "</tr></thead><tbody>".into())
                        }
                        Tag::TableRow => {
                            head = false;
                            col = 0;
                            ("<tr>".into(), "</tr>".into())
                        }
                        Tag::TableCell => {
                            let name = if head { "th" } else { "td" };
                            let class = match table_align.get(col) {
                                Some(Alignment::Left) => "left",
                                Some(Alignment::Right) => "right",
                                Some(Alignment::Center) => "center",
                                _ => "none",
                            };
                            col += 1;
                            (
                                format!("<{name} class=\"align-{class}\">"),
                                format!("</{name}>"),
                            )
                        }
                        Tag::FootnoteDefinition(s) => (
                            format!(
                                "<aside id=\"{}\" class=\"footnote\">",
                                escaped(target(s).trim_start_matches('#'))
                            ),
                            "</aside>".into(),
                        ),
                        Tag::MetadataBlock(_) => {
                            ("<pre class=\"front-matter\">".into(), "</pre>".into())
                        }
                        _ => (String::new(), String::new()),
                    };
                    out.push_str(&open);
                    closes.push(close);
                }
                Event::End(_) => {
                    out.push_str(&closes.pop().unwrap_or_default());
                }
                Event::Text(s) | Event::Html(s) | Event::InlineHtml(s) => out.push_str(&escaped(s)),
                Event::Code(s) => out.push_str(&format!("<code>{}</code>", escaped(s))),
                Event::InlineMath(s) => {
                    out.push_str(&format!("<span class=\"math\">{}</span>", escaped(s)))
                }
                Event::DisplayMath(s) => {
                    out.push_str(&format!("<div class=\"math\">{}</div>", escaped(s)))
                }
                Event::SoftBreak => out.push('\n'),
                Event::HardBreak => out.push_str("<br>\n"),
                Event::Rule => out.push_str("<hr>"),
                Event::TaskListMarker(yes) => out.push_str(if *yes {
                    "<input type=\"checkbox\" disabled checked>"
                } else {
                    "<input type=\"checkbox\" disabled>"
                }),
                Event::FootnoteReference(s) => out.push_str(&format!(
                    "<sup><a href=\"{}\">{}</a></sup>",
                    escaped(&target(s)),
                    escaped(s)
                )),
            }
            i += 1;
        }
        out.push_str("</section>\n");
    }
    out
}
fn footnote_id(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Canonical displayed text before wrapping; selection offsets index these bytes.
pub fn block_text(doc: &Doc, id: usize, style: &Style) -> String {
    let b = &doc.blocks[id];
    let es: Vec<_> = doc.events[b.event_range.clone()]
        .iter()
        .map(|(e, _)| e.clone())
        .collect();
    plain_events(&es, style)
}
/// Inert text mode: controls become visible, Markdown syntax stays literal.
pub fn parse_text(src: &str) -> Doc {
    let events = vec![
        (
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)),
            0..src.len(),
        ),
        (Event::Text(src.to_owned().into()), 0..src.len()),
        (Event::End(TagEnd::CodeBlock), 0..src.len()),
    ];
    Doc {
        blocks: vec![Block {
            id: 0,
            kind: Kind::Code {
                language: String::new(),
            },
            range: 0..src.len(),
            heading_path: Vec::new(),
            fingerprint: fingerprint(src),
            event_range: 0..3,
        }],
        events,
    }
}
