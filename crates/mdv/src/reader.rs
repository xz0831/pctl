//! Reader state and source-independent selection; no terminal IO.
use crate::{Doc, Kind, Line, Style, block_text, to_ansi};
use std::{collections::HashMap, ops::Range};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Off,
    Tree,
    Inbox,
}
impl Side {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Tree,
            Self::Tree => Self::Inbox,
            Self::Inbox => Self::Off,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tree => "Tree",
            Self::Inbox => "Inbox",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CopyMode {
    #[default]
    Text,
    Markdown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub block: usize,
    pub byte: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: Position,
    pub end: Position,
}
impl Selection {
    pub fn ordered(self) -> (Position, Position) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }
}
#[derive(Default)]
pub struct State {
    pub side: Side,
    pub outline: bool,
    pub pinned: bool,
    pub updated: bool,
    pub focus: Option<usize>,
    pub copy_mode: CopyMode,
    pub selection: Option<Selection>,
    pub top: usize,
    pub tree_cursor: usize,
    pub tree_scroll: usize,
    pub inbox_cursor: usize,
    pub inbox_scroll: usize,
    pub outline_cursor: usize,
    pub side_focus: bool,
}
impl State {
    pub fn cycle(&mut self) {
        self.outline = false;
        self.side = self.side.next();
        self.side_focus = false;
    }
    pub fn outline(&mut self) {
        self.outline = !self.outline;
        self.side_focus = false;
    }
    pub fn new_doc(&mut self) {
        self.top = 0;
        self.focus = None;
        self.selection = None;
        self.copy_mode = CopyMode::Text;
        self.updated = false;
    }
    pub fn file_changed(&mut self) -> bool {
        self.updated = true;
        !self.pinned
    }
    pub fn toggle_copy(&mut self) {
        self.copy_mode = match self.copy_mode {
            CopyMode::Text => CopyMode::Markdown,
            CopyMode::Markdown => CopyMode::Text,
        };
    }
    pub fn move_focus(&mut self, doc: &Doc, forward: bool) {
        let ids: Vec<_> = doc
            .blocks
            .iter()
            .filter(|b| b.kind != Kind::Definition)
            .map(|b| b.id)
            .collect();
        if ids.is_empty() {
            self.focus = None;
            return;
        }
        let at = self.focus.and_then(|id| ids.iter().position(|x| *x == id));
        let next = match (at, forward) {
            (Some(i), true) => (i + 1).min(ids.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
            (None, true) => 0,
            (None, false) => ids.len() - 1,
        };
        self.focus = Some(ids[next]);
    }
}
#[derive(Debug, Clone)]
pub struct Offer {
    pub id: String,
    pub path: String,
    pub agent: String,
    pub project: String,
    pub title: String,
    pub ts: u64,
    pub repeats: usize,
}
#[derive(Default)]
pub struct Inbox {
    pub entries: Vec<Offer>,
    pub arrivals: usize,
    initialized: bool,
    observed: HashMap<String, usize>,
}
impl Inbox {
    pub fn update(&mut self, offers: Vec<Offer>) {
        let mut by_id: HashMap<String, Offer> = HashMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut latest = HashMap::new();
        for (sequence, offer) in offers.into_iter().enumerate() {
            *counts.entry(offer.id.clone()).or_default() += 1;
            latest.insert(offer.id.clone(), sequence);
            by_id.insert(offer.id.clone(), offer);
        }
        if self.initialized {
            self.arrivals += counts
                .iter()
                .map(|(id, n)| n.saturating_sub(*self.observed.get(id).unwrap_or(&0)))
                .sum::<usize>();
        }
        self.initialized = true;
        self.observed = counts;
        self.entries = by_id
            .into_values()
            .map(|mut offer| {
                offer.repeats = self.observed[&offer.id];
                offer
            })
            .collect();
        self.entries
            .sort_by_key(|o| std::cmp::Reverse(latest[&o.id]));
    }
}
#[derive(Debug, Clone)]
pub struct Row {
    pub line: Line,
    pub range: Range<usize>,
    pub discussion: bool,
}
pub struct Layout {
    pub rows: Vec<Row>,
    pub texts: Vec<String>,
}
impl Layout {
    pub fn new(doc: &Doc, src: &str, width: usize, style: &Style) -> Self {
        let texts: Vec<_> = doc
            .blocks
            .iter()
            .map(|b| block_text(doc, b.id, style))
            .collect();
        let mut offsets = vec![0; doc.blocks.len()];
        let rows = to_ansi(doc, src, width, style)
            .into_iter()
            .map(|line| {
                let text = &texts[line.block_id];
                let from = offsets[line.block_id];
                let replacement = line.plain == "?"
                    && text[from..]
                        .graphemes(true)
                        .next()
                        .is_some_and(|g| UnicodeWidthStr::width(g) > width);
                let start = if replacement || line.plain.is_empty() {
                    from
                } else {
                    text[from..].find(&line.plain).map_or(from, |n| from + n)
                };
                let end = if replacement {
                    start + text[start..].graphemes(true).next().unwrap().len()
                } else {
                    (start + line.plain.len()).min(text.len())
                };
                let (start, end) = line
                    .source_offsets
                    .as_ref()
                    .map_or((start, end), |mapping| {
                        (
                            *mapping.first().unwrap_or(&start),
                            *mapping.iter().max().unwrap_or(&end),
                        )
                    });
                let (start, end) = (start.min(text.len()), end.min(text.len()));
                offsets[line.block_id] = end;
                Row {
                    line,
                    range: start..end,
                    discussion: false,
                }
            })
            .collect();
        Self { rows, texts }
    }
    pub fn point(&self, row: usize, cell: usize) -> Option<Position> {
        let row = self.rows.get(row)?;
        if row.discussion {
            return None;
        }
        let byte = cell_byte(&row.line.plain, cell);
        if let Some(offsets) = &row.line.source_offsets {
            return Some(Position {
                block: row.line.block_id,
                byte: offsets[byte].min(self.texts[row.line.block_id].len()),
            });
        }
        Some(Position {
            block: row.line.block_id,
            byte: if byte == row.line.plain.len() {
                row.range.end
            } else {
                (row.range.start + byte).min(row.range.end)
            },
        })
    }
    pub fn row_for(&self, pos: Position) -> usize {
        if let Some(index) = self.rows.iter().position(|r| {
            r.line.block_id == pos.block
                && r.line.source_offsets.as_ref().is_some_and(|mapping| {
                    mapping
                        .windows(2)
                        .any(|pair| pair[0] == pos.byte && pair[1] == pos.byte + 1)
                })
        }) {
            return index;
        }
        self.rows
            .iter()
            .position(|r| r.line.block_id == pos.block && r.range.end > pos.byte)
            .or_else(|| self.rows.iter().position(|r| r.line.block_id == pos.block))
            .unwrap_or(0)
    }
    pub fn anchor(&self, top: usize) -> Option<Position> {
        self.rows.get(top).map(|r| Position {
            block: r.line.block_id,
            byte: r
                .line
                .source_offsets
                .as_ref()
                .and_then(|mapping| {
                    r.line
                        .plain
                        .char_indices()
                        .find(|(_, c)| !c.is_whitespace() && !matches!(c, '|' | '-'))
                        .map(|(byte, _)| mapping[byte])
                })
                .unwrap_or(r.range.start),
        })
    }
    pub fn text_copy(&self, selection: Selection) -> String {
        let (a, b) = selection.ordered();
        (a.block..=b.block)
            .filter_map(|id| {
                let s = self.texts.get(id)?;
                let start = if id == a.block {
                    a.byte.min(s.len())
                } else {
                    0
                };
                let end = if id == b.block {
                    b.byte.min(s.len())
                } else {
                    s.len()
                };
                Some(s[start..end].to_owned())
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
pub fn cell_byte(text: &str, cell: usize) -> usize {
    let mut cells = 0;
    for (i, g) in text.grapheme_indices(true) {
        let w = UnicodeWidthStr::width(g);
        if cells + w > cell {
            return i;
        }
        cells += w;
    }
    text.len()
}
pub fn markdown_copy(doc: &Doc, src: &str, selection: Selection) -> String {
    let (a, b) = selection.ordered();
    let last = if b.byte == 0 && b.block > a.block {
        b.block - 1
    } else {
        b.block
    };
    src[doc.blocks[a.block].range.start..doc.blocks[last].range.end].to_owned()
}
pub fn command_key(c: char, shift: bool) -> char {
    // Same two-set positions as yazi/gen_keymap.sh; search never calls this.
    let lower = "ㅂㅈㄷㄱㅅㅛㅕㅑㅐㅔㅁㄴㅇㄹㅎㅗㅓㅏㅣㅋㅌㅊㅍㅠㅜㅡ";
    let latin = "qwertyuiopasdfghjklzxcvbnm";
    let mapped = lower
        .chars()
        .position(|x| x == c)
        .and_then(|i| latin.chars().nth(i));
    let c = match c {
        'ㅃ' => 'Q',
        'ㅉ' => 'W',
        'ㄸ' => 'E',
        'ㄲ' => 'R',
        'ㅆ' => 'T',
        'ㅒ' => 'O',
        'ㅖ' => 'P',
        _ => mapped.unwrap_or(c),
    };
    if shift { c.to_ascii_uppercase() } else { c }
}
pub fn osc52(text: &str) -> String {
    const B: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        for i in 0..4 {
            encoded.push(if i > chunk.len() {
                '='
            } else {
                B[((n >> (18 - 6 * i)) & 63) as usize] as char
            });
        }
    }
    format!("\x1b]52;c;{encoded}\x07")
}
