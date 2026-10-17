//! How the terminal area is divided: a tree of splits, each leaf one cell. A split puts two parts
//! side by side (`Dir::Right`, a one-column divider between them) or one above the other
//! (`Dir::Down`, no extra row: the lower part's title row is the divider). Cells are counted in
//! reading order (left to right, top to bottom), and one of them has the focus. Nothing here knows
//! what a cell shows, so the whole tree can be tested without a terminal.
use ratatui::layout::Rect;
use serde_json::{Value, json};

/// A ratio is the first part's share of a split in thousandths, so 5% is 50.
pub const WHOLE: u16 = 1000;
pub const STEP: u16 = 50;
const LEAST: u16 = 50;
const MOST: u16 = WHOLE - LEAST;
/// The smallest a part is kept while the area has room for it.
const MIN_WIDTH: u16 = 10;
const MIN_HEIGHT: u16 = 3;
/// A tree from a file or a run holds at most this many cells.
pub const MAX_CELLS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Side by side.
    Right,
    /// One above the other.
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}
impl Side {
    fn dir(self) -> Dir {
        match self {
            Side::Left | Side::Right => Dir::Right,
            Side::Up | Side::Down => Dir::Down,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node<T> {
    Leaf(T),
    Split {
        dir: Dir,
        ratio: u16,
        a: Box<Node<T>>,
        b: Box<Node<T>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree<T> {
    root: Node<T>,
    /// Index of the focused cell.
    pub focus: usize,
    /// Only the focused cell is shown, over the whole area.
    pub zoomed: bool,
}

/// A line the mouse can drag, and what moving it changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// Which child to take at each split, from the root: false is the first.
    pub path: Vec<bool>,
    pub dir: Dir,
    /// Where it is drawn and hit.
    pub rect: Rect,
    /// The area of the split it belongs to.
    pub area: Rect,
}
impl Divider {
    /// The ratio that puts this divider at the pointer.
    pub fn ratio_at(&self, column: u16, row: u16) -> u16 {
        let (pos, total) = match self.dir {
            Dir::Right => (
                column.saturating_sub(self.area.x),
                self.area.width.saturating_sub(1),
            ),
            Dir::Down => (row.saturating_sub(self.area.y), self.area.height),
        };
        if total == 0 {
            return WHOLE / 2;
        }
        let ratio = (u32::from(pos) * u32::from(WHOLE) + u32::from(total) / 2) / u32::from(total);
        (ratio.min(u32::from(WHOLE)) as u16).clamp(LEAST, MOST)
    }
}

/// Where the cells and dividers are for one area.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// (index, area) of every cell shown.
    pub cells: Vec<(usize, Rect)>,
    pub dividers: Vec<Divider>,
}
impl Geometry {
    pub fn cell_at(&self, column: u16, row: u16) -> Option<usize> {
        self.cells
            .iter()
            .find(|(_, r)| contains(*r, column, row))
            .map(|(i, _)| *i)
    }
    #[cfg(test)]
    pub fn divider_at(&self, column: u16, row: u16) -> Option<&Divider> {
        self.dividers.iter().find(|d| contains(d.rect, column, row))
    }
}

fn contains(r: Rect, column: u16, row: u16) -> bool {
    column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// The two lengths of a split of `total` with a gap of `gap` between them.
fn sizes(total: u16, ratio: u16, gap: u16, least: u16) -> (u16, u16) {
    let room = total.saturating_sub(gap);
    let keep = least.min(room / 2);
    let first = ((u32::from(room) * u32::from(ratio) + 500) / 1000) as u16;
    let first = first.clamp(keep, room - keep);
    (first, room - first)
}

impl<T: Clone> Node<T> {
    fn count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.count() + b.count(),
        }
    }
    fn first(&self) -> &T {
        match self {
            Node::Leaf(t) => t,
            Node::Split { a, .. } => a.first(),
        }
    }
    fn nth(&self, i: usize) -> Option<&T> {
        match self {
            Node::Leaf(t) => (i == 0).then_some(t),
            Node::Split { a, b, .. } => {
                let n = a.count();
                if i < n { a.nth(i) } else { b.nth(i - n) }
            }
        }
    }
    fn nth_mut(&mut self, i: usize) -> Option<&mut T> {
        match self {
            Node::Leaf(t) => (i == 0).then_some(t),
            Node::Split { a, b, .. } => {
                let n = a.count();
                if i < n {
                    a.nth_mut(i)
                } else {
                    b.nth_mut(i - n)
                }
            }
        }
    }
    fn leaves<'a>(&'a self, out: &mut Vec<&'a T>) {
        match self {
            Node::Leaf(t) => out.push(t),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }
    /// Cell `i` becomes a split of itself and `new`.
    fn split(&mut self, i: usize, dir: Dir, new: T) {
        match self {
            Node::Leaf(t) => {
                let old = t.clone();
                *self = Node::Split {
                    dir,
                    ratio: WHOLE / 2,
                    a: Box::new(Node::Leaf(old)),
                    b: Box::new(Node::Leaf(new)),
                };
            }
            Node::Split { a, b, .. } => {
                let n = a.count();
                if i < n {
                    a.split(i, dir, new)
                } else {
                    b.split(i - n, dir, new)
                }
            }
        }
    }
    /// Take cell `i` out; its sibling fills the place of their split. Some(true) when it was the
    /// second of its two.
    fn remove(&mut self, i: usize) -> Option<bool> {
        let Node::Split { a, b, .. } = self else {
            return None;
        };
        let n = a.count();
        if i < n {
            if n == 1 {
                let sibling = (**b).clone();
                *self = sibling;
                return Some(false);
            }
            a.remove(i)
        } else {
            if b.count() == 1 {
                let sibling = (**a).clone();
                *self = sibling;
                return Some(true);
            }
            b.remove(i - n)
        }
    }
    fn path(&self, i: usize, out: &mut Vec<bool>) {
        if let Node::Split { a, b, .. } = self {
            let n = a.count();
            if i < n {
                out.push(false);
                a.path(i, out);
            } else {
                out.push(true);
                b.path(i - n, out);
            }
        }
    }
    fn at_mut(&mut self, path: &[bool]) -> Option<&mut Node<T>> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((second, rest)), Node::Split { a, b, .. }) => {
                if *second { b } else { a }.at_mut(rest)
            }
            (Some(_), Node::Leaf(_)) => None,
        }
    }
    fn place(
        &self,
        area: Rect,
        path: &mut Vec<bool>,
        cells: &mut Vec<Rect>,
        dividers: &mut Vec<Divider>,
    ) {
        let Node::Split { dir, ratio, a, b } = self else {
            cells.push(area);
            return;
        };
        let (first, second, rect) = match dir {
            Dir::Right => {
                let (wa, wb) = sizes(area.width, *ratio, 1, MIN_WIDTH);
                (
                    Rect { width: wa, ..area },
                    Rect {
                        x: area.x + wa + 1,
                        width: wb,
                        ..area
                    },
                    Rect {
                        x: area.x + wa,
                        width: 1,
                        ..area
                    },
                )
            }
            Dir::Down => {
                let (ha, hb) = sizes(area.height, *ratio, 0, MIN_HEIGHT);
                (
                    Rect { height: ha, ..area },
                    Rect {
                        y: area.y + ha,
                        height: hb,
                        ..area
                    },
                    Rect {
                        y: area.y + ha,
                        height: 1,
                        ..area
                    },
                )
            }
        };
        dividers.push(Divider {
            path: path.clone(),
            dir: *dir,
            rect,
            area,
        });
        for (child, part, second) in [(a, first, false), (b, second, true)] {
            path.push(second);
            child.place(part, path, cells, dividers);
            path.pop();
        }
    }
}

impl<T: Clone> Tree<T> {
    pub fn new(leaf: T) -> Self {
        Self {
            root: Node::Leaf(leaf),
            focus: 0,
            zoomed: false,
        }
    }
    pub fn len(&self) -> usize {
        self.root.count()
    }
    pub fn leaves(&self) -> Vec<&T> {
        let mut out = vec![];
        self.root.leaves(&mut out);
        out
    }
    pub fn focused(&self) -> &T {
        self.root
            .nth(self.focus)
            .unwrap_or_else(|| self.root.first())
    }
    /// Cells shown: all of them, or only the focused one when zoomed.
    pub fn shown(&self) -> Vec<usize> {
        if self.zoomed {
            vec![self.focus]
        } else {
            (0..self.len()).collect()
        }
    }
    /// Whether cells carry a title row: with a single cell on screen the area is all its own.
    pub fn titled(&self) -> bool {
        self.len() > 1 && !self.zoomed
    }

    /// Split the focused cell; the new one comes second and takes the focus. False at the limit.
    pub fn split(&mut self, dir: Dir, new: T) -> bool {
        if self.len() >= MAX_CELLS {
            return false;
        }
        self.root.split(self.focus, dir, new);
        self.focus += 1;
        self.zoomed = false;
        true
    }
    /// Close cell `i` (never the last one) and hand it back. The focus stays on the cell it was on,
    /// or goes to the one that took the closed cell's place.
    pub fn close(&mut self, i: usize) -> Option<T> {
        let gone = self.root.nth(i)?.clone();
        let second = self.root.remove(i)?;
        self.focus = match self.focus {
            f if f > i => f - 1,
            f if f == i && second => i - 1,
            f => f,
        };
        self.zoomed = false;
        Some(gone)
    }
    /// Exchange two cells; the focus goes along with the one that was focused.
    pub fn swap(&mut self, i: usize, j: usize) -> bool {
        let (Some(x), Some(y)) = (self.root.nth(i).cloned(), self.root.nth(j).cloned()) else {
            return false;
        };
        if let Some(t) = self.root.nth_mut(i) {
            *t = y;
        }
        if let Some(t) = self.root.nth_mut(j) {
            *t = x;
        }
        if self.focus == i {
            self.focus = j;
        } else if self.focus == j {
            self.focus = i;
        }
        true
    }
    pub fn zoom(&mut self) -> bool {
        if self.len() < 2 {
            return false;
        }
        self.zoomed = !self.zoomed;
        true
    }
    /// Move the divider nearest the focused cell on that side by `step`. False when nothing moved.
    pub fn resize(&mut self, toward: Side, step: u16) -> bool {
        let mut path = vec![];
        self.root.path(self.focus, &mut path);
        for depth in (0..path.len()).rev() {
            let Some(Node::Split { dir, ratio, .. }) = self.root.at_mut(&path[..depth]) else {
                continue;
            };
            if *dir != toward.dir() {
                continue;
            }
            let wanted = match toward {
                Side::Right | Side::Down => ratio.saturating_add(step),
                Side::Left | Side::Up => ratio.saturating_sub(step),
            }
            .clamp(LEAST, MOST);
            let moved = wanted != *ratio;
            *ratio = wanted;
            return moved;
        }
        false
    }
    /// Put a divider (found by `Geometry`) at a ratio.
    pub fn set_ratio(&mut self, path: &[bool], to: u16) {
        if let Some(Node::Split { ratio, .. }) = self.root.at_mut(path) {
            *ratio = to.clamp(LEAST, MOST);
        }
    }
    /// The cell on that side of cell `from`, given every cell's area in order.
    pub fn neighbor(cells: &[Rect], from: usize, toward: Side) -> Option<usize> {
        let f = *cells.get(from)?;
        let (fx, fy) = (i32::from(f.x), i32::from(f.y));
        let (fr, fb) = (fx + i32::from(f.width), fy + i32::from(f.height));
        let overlap = |a0: i32, a1: i32, b0: i32, b1: i32| a1.min(b1) - a0.max(b0);
        cells
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != from)
            .filter_map(|(j, c)| {
                let (cx, cy) = (i32::from(c.x), i32::from(c.y));
                let (cr, cb) = (cx + i32::from(c.width), cy + i32::from(c.height));
                let (gap, along) = match toward {
                    Side::Left => (fx - cr, overlap(fy, fb, cy, cb)),
                    Side::Right => (cx - fr, overlap(fy, fb, cy, cb)),
                    Side::Up => (fy - cb, overlap(fx, fr, cx, cr)),
                    Side::Down => (cy - fb, overlap(fx, fr, cx, cr)),
                };
                (gap >= 0 && along > 0).then_some((j, along, gap))
            })
            .min_by_key(|(j, along, gap)| (-along, *gap, *j))
            .map(|(j, ..)| j)
    }
    /// Every cell's area, ignoring the zoom.
    pub fn areas(&self, area: Rect) -> Vec<Rect> {
        let (mut cells, mut dividers) = (vec![], vec![]);
        self.root
            .place(area, &mut vec![], &mut cells, &mut dividers);
        cells
    }
    /// What is on screen in `area`.
    pub fn geometry(&self, area: Rect) -> Geometry {
        if self.zoomed {
            return Geometry {
                cells: vec![(self.focus, area)],
                dividers: vec![],
            };
        }
        let (mut cells, mut dividers) = (vec![], vec![]);
        self.root
            .place(area, &mut vec![], &mut cells, &mut dividers);
        Geometry {
            cells: cells.into_iter().enumerate().collect(),
            dividers,
        }
    }

    /// `{"focus": n, "tree": …}`; a leaf is whatever `leaf` makes of it.
    pub fn to_json(&self, leaf: &dyn Fn(&T) -> Value) -> Value {
        fn node<T>(n: &Node<T>, leaf: &dyn Fn(&T) -> Value) -> Value {
            match n {
                Node::Leaf(t) => leaf(t),
                Node::Split { dir, ratio, a, b } => json!({
                    "split": match dir { Dir::Right => "right", Dir::Down => "down" },
                    "ratio": ratio, "a": node(a, leaf), "b": node(b, leaf),
                }),
            }
        }
        json!({"focus": self.focus, "tree": node(&self.root, leaf)})
    }
    /// The tree `to_json` wrote. A file that was edited by hand or by another version gives a
    /// tree that works (ratios clamped, cells capped) or None.
    pub fn from_json(v: &Value, leaf: &mut dyn FnMut(&Value) -> Option<T>) -> Option<Self> {
        fn node<T>(
            v: &Value,
            depth: usize,
            left: &mut usize,
            leaf: &mut dyn FnMut(&Value) -> Option<T>,
        ) -> Option<Node<T>> {
            let dir = match v["split"].as_str() {
                None => {
                    *left = left.checked_sub(1)?;
                    return leaf(v).map(Node::Leaf);
                }
                Some("right") => Dir::Right,
                Some("down") => Dir::Down,
                Some(_) => return None,
            };
            if depth >= MAX_CELLS {
                return None;
            }
            let ratio = v["ratio"]
                .as_u64()
                .map_or(WHOLE / 2, |r| r.min(u64::from(WHOLE)) as u16)
                .clamp(LEAST, MOST);
            Some(Node::Split {
                dir,
                ratio,
                a: Box::new(node(&v["a"], depth + 1, left, leaf)?),
                b: Box::new(node(&v["b"], depth + 1, left, leaf)?),
            })
        }
        let root = node(&v["tree"], 0, &mut MAX_CELLS.clone(), leaf)?;
        let focus = v["focus"].as_u64().unwrap_or(0) as usize;
        let focus = focus.min(root.count() - 1);
        Some(Self {
            root,
            focus,
            zoomed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 101, 40)
    }
    /// Cells a, b, c: a | (b over c).
    fn three() -> Tree<char> {
        let mut t = Tree::new('a');
        assert!(t.split(Dir::Right, 'b'));
        assert!(t.split(Dir::Down, 'c'));
        t
    }
    fn order(t: &Tree<char>) -> String {
        t.leaves().into_iter().collect()
    }

    #[test]
    fn a_split_adds_a_cell_after_the_focused_one_and_focuses_it() {
        let t = three();
        assert_eq!((order(&t), t.focus, t.len()), ("abc".into(), 2, 3));
        // Splitting the first cell puts the new one right after it.
        let mut t = three();
        t.focus = 0;
        t.split(Dir::Down, 'd');
        assert_eq!((order(&t), t.focus), ("adbc".into(), 1));
    }
    #[test]
    fn the_cell_count_is_limited() {
        let mut t = Tree::new(0);
        for n in 1..MAX_CELLS {
            assert!(t.split(Dir::Right, n));
        }
        assert!(!t.split(Dir::Down, 99));
        assert_eq!(t.len(), MAX_CELLS);
    }
    #[test]
    fn closing_keeps_the_focus_on_its_cell_or_the_one_that_takes_the_place() {
        // Closing the focused second-of-two goes back to the first of them.
        let mut t = three();
        assert_eq!(t.close(2), Some('c'));
        assert_eq!((order(&t), t.focus), ("ab".into(), 1), "b took c's place");
        // Closing the focused first cell moves the focus to what fills the gap.
        let mut t = three();
        t.focus = 0;
        assert_eq!(t.close(0), Some('a'));
        assert_eq!((order(&t), t.focus), ("bc".into(), 0));
        // Closing another cell leaves the focus on the cell it was on.
        let mut t = three();
        t.focus = 2;
        t.close(0);
        assert_eq!((order(&t), *t.focused()), ("bc".into(), 'c'));
        t.focus = 0;
        t.close(1);
        assert_eq!((order(&t), *t.focused()), ("b".into(), 'b'));
        // The last cell cannot be closed.
        assert_eq!(t.close(0), None);
        assert_eq!(t.len(), 1);
        // A cell that is not there cannot be closed.
        assert_eq!(three().close(3), None);
    }
    #[test]
    fn closing_drops_the_split_and_gives_the_space_to_the_sibling() {
        let mut t = three();
        t.close(2);
        let g = t.geometry(area());
        assert_eq!(g.cells.len(), 2);
        assert_eq!(g.dividers.len(), 1, "b and a share one split");
        assert_eq!(g.cells[1].1.height, 40, "b has the whole height again");
    }
    #[test]
    fn swapping_exchanges_cells_and_the_focus_follows() {
        let mut t = three();
        t.focus = 0;
        assert!(t.swap(0, 1));
        assert_eq!((order(&t), t.focus), ("bac".into(), 1));
        assert!(t.swap(1, 2));
        assert_eq!((order(&t), t.focus), ("bca".into(), 2));
        // The cell that was not focused moves under a focus that stays on its own.
        let mut t = three();
        t.focus = 1;
        t.swap(0, 2);
        assert_eq!((order(&t), t.focus), ("cba".into(), 1));
        assert!(!t.swap(0, 3));
    }
    #[test]
    fn geometry_fills_the_area_without_overlap() {
        let t = three();
        let g = t.geometry(area());
        // Side by side: 50 + divider + 50. The right part is split in two, 20 rows each.
        let rects: Vec<Rect> = g.cells.iter().map(|(_, r)| *r).collect();
        assert_eq!(rects[0], Rect::new(0, 0, 50, 40));
        assert_eq!(rects[1], Rect::new(51, 0, 50, 20));
        assert_eq!(rects[2], Rect::new(51, 20, 50, 20));
        assert_eq!(g.dividers.len(), 2);
        assert_eq!(g.dividers[0].rect, Rect::new(50, 0, 1, 40));
        assert_eq!(
            g.dividers[1].rect,
            Rect::new(51, 20, 50, 1),
            "the lower part's first row is the divider"
        );
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(a.intersection(*b).is_empty(), "{a:?} overlaps {b:?}");
            }
        }
        let cells: u32 = rects
            .iter()
            .map(|r| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(
            cells + 40,
            101 * 40,
            "cells and the one divider column cover it all"
        );
    }
    #[test]
    fn a_zoomed_cell_has_the_whole_area_and_no_title() {
        let mut t = three();
        assert!(t.titled());
        assert!(t.zoom());
        assert!(!t.titled());
        assert_eq!(t.geometry(area()).cells, vec![(2, area())]);
        assert!(t.geometry(area()).dividers.is_empty());
        assert_eq!(t.shown(), vec![2]);
        assert!(t.zoom());
        assert_eq!(t.geometry(area()).cells.len(), 3);
        // One cell has nothing to zoom, and a split or a close ends the zoom.
        assert!(!Tree::new('a').zoom());
        t.zoom();
        t.split(Dir::Right, 'd');
        assert!(!t.zoomed);
        t.zoom();
        t.close(0);
        assert!(!t.zoomed);
    }
    #[test]
    fn sizes_keep_every_part_visible_while_there_is_room() {
        assert_eq!(sizes(101, 500, 1, 10), (50, 50));
        assert_eq!(sizes(101, 50, 1, 10), (10, 90), "least width");
        assert_eq!(sizes(101, 950, 1, 10), (90, 10));
        assert_eq!(sizes(40, 500, 0, 3), (20, 20));
        // Too small for the least: split evenly, never underflow.
        assert_eq!(sizes(5, 100, 1, 10), (2, 2));
        assert_eq!(sizes(0, 500, 1, 10), (0, 0));
        assert_eq!(sizes(1, 500, 1, 10), (0, 0));
        assert_eq!(
            sizes(u16::MAX, WHOLE, 1, 10).0 + 1 + sizes(u16::MAX, WHOLE, 1, 10).1,
            u16::MAX
        );
    }
    #[test]
    fn resizing_moves_the_nearest_divider_on_that_axis_in_steps_and_clamps() {
        let mut t = three();
        // c is under b: Down/Up move their divider, Left/Right the one with a.
        t.focus = 2;
        assert!(t.resize(Side::Up, STEP));
        assert_eq!(t.geometry(area()).cells[1].1.height, 18, "b lost 5% of 40");
        assert!(t.resize(Side::Right, STEP));
        assert_eq!(t.geometry(area()).cells[0].1.width, 55);
        assert!(t.resize(Side::Left, STEP * 2));
        assert_eq!(t.geometry(area()).cells[0].1.width, 45);
        // Pushing a divider to its end stops at 5% (or the least size, whichever is more).
        for _ in 0..40 {
            t.resize(Side::Left, STEP);
        }
        assert!(!t.resize(Side::Left, STEP), "already at the end");
        assert_eq!(t.geometry(area()).cells[0].1.width, 10, "kept visible");
        for _ in 0..40 {
            t.resize(Side::Right, STEP);
        }
        assert!(!t.resize(Side::Right, STEP));
        assert_eq!(t.geometry(area()).cells[1].1.width, 10);
        // a has no vertical split to move.
        t.focus = 0;
        assert!(!t.resize(Side::Down, STEP));
        // A single cell has nothing to resize.
        assert!(!Tree::new('a').resize(Side::Right, STEP));
    }
    #[test]
    fn dragging_a_divider_puts_it_under_the_pointer_within_limits() {
        let mut t = three();
        let g = t.geometry(area());
        let d = g.dividers[0].clone();
        assert_eq!(d.rect.x, 50);
        t.set_ratio(&d.path, d.ratio_at(30, 5));
        assert_eq!(t.geometry(area()).cells[0].1.width, 30);
        assert_eq!(
            t.geometry(area()).dividers[0].rect.x,
            30,
            "the divider is where it was dragged to"
        );
        // Far past either end: kept inside.
        t.set_ratio(&d.path, d.ratio_at(0, 5));
        assert_eq!(t.geometry(area()).cells[0].1.width, 10);
        t.set_ratio(&d.path, d.ratio_at(500, 5));
        assert_eq!(t.geometry(area()).cells[0].1.width, 90);
        // The horizontal divider between b and c moves with the row.
        let h = t.geometry(area()).dividers[1].clone();
        t.set_ratio(&h.path, h.ratio_at(60, 10));
        assert_eq!(t.geometry(area()).cells[1].1.height, 10);
        assert_eq!(t.geometry(area()).cells[2].1.y, 10);
        // A path to nowhere changes nothing.
        let before = t.clone();
        t.set_ratio(&[true, true, true, true], 100);
        assert_eq!(t, before);
    }
    #[test]
    fn hit_testing_finds_cells_and_dividers() {
        let t = three();
        let g = t.geometry(area());
        assert_eq!(g.cell_at(0, 0), Some(0));
        assert_eq!(g.cell_at(49, 39), Some(0));
        assert_eq!(g.cell_at(50, 10), None, "the divider column is no cell");
        assert_eq!(g.cell_at(60, 19), Some(1));
        assert_eq!(g.cell_at(60, 20), Some(2));
        assert_eq!(g.cell_at(100, 39), Some(2));
        assert_eq!(g.cell_at(101, 5), None, "outside");
        assert_eq!(g.divider_at(50, 0).map(|d| d.dir), Some(Dir::Right));
        assert_eq!(g.divider_at(50, 39).map(|d| d.dir), Some(Dir::Right));
        assert_eq!(g.divider_at(70, 20).map(|d| d.dir), Some(Dir::Down));
        assert_eq!(g.divider_at(70, 21), None);
        assert_eq!(g.divider_at(49, 5), None);
        // A divider hit is drawn over the cell it shares a row with.
        assert_eq!(g.cell_at(70, 20), Some(2));
    }
    #[test]
    fn focus_moves_to_the_neighbour_with_the_most_shared_edge() {
        let t = three();
        let cells = t.areas(area());
        let go = |from, toward| Tree::<char>::neighbor(&cells, from, toward);
        assert_eq!(
            go(0, Side::Right),
            Some(1),
            "b shares the top, c the bottom: equal, first wins"
        );
        assert_eq!(go(1, Side::Left), Some(0));
        assert_eq!(go(2, Side::Left), Some(0));
        assert_eq!(go(1, Side::Down), Some(2));
        assert_eq!(go(2, Side::Up), Some(1));
        assert_eq!(go(0, Side::Left), None);
        assert_eq!(go(0, Side::Up), None);
        assert_eq!(go(2, Side::Down), None);
        assert_eq!(go(1, Side::Right), None);
        assert_eq!(go(9, Side::Right), None);
        // A taller neighbour on the left is reached from either cell of the right column.
        let mut t = three();
        t.focus = 1;
        t.split(Dir::Right, 'd');
        let cells = t.areas(area());
        let go = |from, toward| Tree::<char>::neighbor(&cells, from, toward);
        assert_eq!(order(&t), "abdc");
        assert_eq!(go(2, Side::Left), Some(1), "d's left is b");
        assert_eq!(go(3, Side::Left), Some(0), "c is wide, its left is a");
        assert_eq!(
            go(3, Side::Up),
            Some(1),
            "above c: b and d share its edge alike, the first wins"
        );
    }
    #[test]
    fn the_tree_survives_json() {
        let mut t = three();
        t.resize(Side::Right, STEP);
        t.focus = 1;
        let v = t.to_json(&|c| json!(c.to_string()));
        let back = Tree::from_json(&v, &mut |l| l.as_str()?.chars().next()).expect("parses");
        assert_eq!(back, t);
        assert_eq!(back.geometry(area()), t.geometry(area()));
        // Through text as well, as the app writes it.
        let text = v.to_string();
        let again: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(
            Tree::from_json(&again, &mut |l| l.as_str()?.chars().next()),
            Some(t)
        );
    }
    #[test]
    fn a_damaged_tree_is_refused_or_made_safe() {
        let leaf = &mut |l: &Value| l.as_str()?.chars().next();
        // Nothing, wrong kinds, a missing half.
        assert_eq!(Tree::<char>::from_json(&json!({}), leaf), None);
        assert_eq!(Tree::<char>::from_json(&json!({"tree": 5}), leaf), None);
        assert_eq!(
            Tree::<char>::from_json(&json!({"tree": {"split": "sideways"}}), leaf),
            None
        );
        assert_eq!(
            Tree::<char>::from_json(&json!({"tree": {"split": "right", "a": "x"}}), leaf),
            None
        );
        // A ratio out of range and a focus past the end are clamped.
        let v = json!({"focus": 99, "tree": {"split": "down", "ratio": 5000, "a": "x", "b": "y"}});
        let t = Tree::from_json(&v, leaf).expect("parses");
        assert_eq!(t.focus, 1);
        let r = t.geometry(Rect::new(0, 0, 40, 100));
        assert_eq!(r.cells[0].1.height, 95);
        let v = json!({"tree": {"split": "right", "ratio": 0, "a": "x", "b": "y"}});
        let r = Tree::from_json(&v, leaf)
            .expect("parses")
            .geometry(Rect::new(0, 0, 201, 10));
        assert_eq!(r.cells[0].1.width, 10);
        // More cells than allowed, and a tree that is nested without end, are refused.
        let mut deep = json!("x");
        for _ in 0..MAX_CELLS + 1 {
            deep = json!({"split": "right", "a": deep, "b": "y"});
        }
        assert_eq!(Tree::<char>::from_json(&json!({"tree": deep}), leaf), None);
        let mut wide = json!("x");
        for _ in 0..MAX_CELLS {
            wide = json!({"split": "down", "a": "y", "b": wide});
        }
        assert_eq!(
            Tree::<char>::from_json(&json!({"tree": wide}), leaf),
            None,
            "17 cells"
        );
    }
}
