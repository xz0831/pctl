use mdv::{Style, parse, parse_text, reader::*};
fn offer(id: &str, ts: u64) -> Offer {
    Offer {
        id: id.into(),
        path: format!("/{id}.md"),
        agent: "builder".into(),
        project: "52".into(),
        title: id.into(),
        ts,
        repeats: 1,
    }
}
#[test]
fn transitions_preserve_reading_and_independent_cursors() {
    let mut s = State {
        top: 17,
        tree_cursor: 3,
        tree_scroll: 2,
        inbox_cursor: 5,
        inbox_scroll: 4,
        selection: Some(Selection {
            start: Position { block: 1, byte: 2 },
            end: Position { block: 1, byte: 6 },
        }),
        ..Default::default()
    };
    for side in [Side::Tree, Side::Inbox, Side::Off] {
        s.cycle();
        assert_eq!(s.side, side);
        assert_eq!(s.top, 17);
        assert!(s.selection.is_some());
    }
    s.cycle();
    s.outline();
    assert!(s.outline);
    s.outline();
    assert_eq!(s.side, Side::Tree);
    s.outline();
    s.cycle();
    assert!(!s.outline);
    assert_eq!(s.side, Side::Inbox);
    assert_eq!(
        (s.tree_cursor, s.tree_scroll, s.inbox_cursor, s.inbox_scroll),
        (3, 2, 5, 4)
    );
    s.pinned = true;
    assert!(!s.file_changed());
    assert!(s.updated);
    s.pinned = false;
    assert!(s.file_changed());
    s.toggle_copy();
    assert_eq!(s.copy_mode, CopyMode::Markdown);
    s.new_doc();
    assert_eq!(s.copy_mode, CopyMode::Text);
    let doc = parse("# a\n\n[r]: a\n\nb\n");
    s.move_focus(&doc, true);
    assert_eq!(s.focus, Some(0));
    s.move_focus(&doc, true);
    assert_eq!(s.focus, Some(2));
    s.move_focus(&doc, false);
    assert_eq!(s.focus, Some(0));
}
#[test]
fn inbox_dedup_and_arrivals() {
    let mut inbox = Inbox::default();
    inbox.update(vec![offer("a", 1), offer("b", 2), offer("a", 3)]);
    assert_eq!(inbox.entries.len(), 2);
    assert_eq!(inbox.entries[0].id, "a");
    assert_eq!(inbox.entries[0].repeats, 2);
    assert_eq!(inbox.arrivals, 0);
    let next = vec![
        offer("a", 1),
        offer("b", 2),
        offer("a", 3),
        offer("b", 4),
        offer("c", 5),
    ];
    inbox.update(next.clone());
    assert_eq!(inbox.arrivals, 2);
    inbox.update(next);
    assert_eq!(inbox.arrivals, 2);
}
#[test]
fn selection_wrap_korean_nfd_and_resize() {
    let src = "한글 한글 👩🏽‍💻 abc def ghi";
    let doc = parse(src);
    let style = Style::default();
    let layout = Layout::new(&doc, src, 8, &style);
    let all = Selection {
        start: Position { block: 0, byte: 0 },
        end: Position {
            block: 0,
            byte: src.len(),
        },
    };
    assert_eq!(layout.text_copy(all), src);
    assert_eq!(layout.point(0, 1), Some(Position { block: 0, byte: 0 }));
    assert_eq!(layout.point(0, 2), Some(Position { block: 0, byte: 3 }));
    let point = layout.point(1, 2).unwrap();
    let selection = Selection {
        start: Position { block: 0, byte: 3 },
        end: point,
    };
    let wide = Layout::new(&doc, src, 40, &style);
    assert_eq!(layout.text_copy(selection), wide.text_copy(selection));
    let anchor = layout.anchor(2).unwrap();
    assert!(wide.row_for(anchor) <= 2);
    let src = "abc def ghi";
    let layout = Layout::new(&parse(src), src, 7, &style);
    assert_eq!(
        layout.text_copy(Selection {
            start: layout.point(0, 0).unwrap(),
            end: layout.point(1, 7).unwrap()
        }),
        src
    );
}
#[test]
fn markdown_copy_selection_touched_blocks_only() {
    let src = "# title\n\nFirst **bold**.\n\n[r]: a.md\n\nSecond.\n";
    let doc = parse(src);
    let selection = Selection {
        start: Position { block: 1, byte: 2 },
        end: Position { block: 3, byte: 3 },
    };
    assert_eq!(
        markdown_copy(&doc, src, selection),
        "First **bold**.\n\n[r]: a.md\n\nSecond.\n"
    );
}
#[test]
fn osc52_has_exact_utf8_payload_and_padding() {
    assert_eq!(osc52(""), "\x1b]52;c;\x07");
    assert_eq!(osc52("f"), "\x1b]52;c;Zg==\x07");
    assert_eq!(osc52("fo"), "\x1b]52;c;Zm8=\x07");
    assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    assert_eq!(osc52("한글"), "\x1b]52;c;7ZWc6riA\x07");
}
#[test]
fn keyboard_and_inert_text() {
    for (ko, en) in [
        ('ㅂ', 'q'),
        ('ㅓ', 'j'),
        ('ㅏ', 'k'),
        ('ㅅ', 't'),
        ('ㅆ', 'T'),
        ('ㅒ', 'O'),
        ('ㅡ', 'm'),
        ('ㅜ', 'n'),
    ] {
        assert_eq!(command_key(ko, false), en);
    }
    assert_eq!(command_key('ㅎ', true), 'G');
    let src = "# source\n\x1b]52;c;attack\x07\n";
    let doc = parse_text(src);
    let layout = Layout::new(&doc, src, 80, &Style::default());
    assert!(layout.texts[0].starts_with("# source"));
    assert!(!layout.texts[0].contains('\x1b'));
    assert!(layout.texts[0].contains('␛'));
}
#[test]
fn tiny_viewport_replacement_keeps_unicode_selection_boundary() {
    let doc = parse("한?글");
    let l = Layout::new(&doc, "한?글", 1, &Style::default());
    let sel = Selection {
        start: l.point(0, 0).unwrap(),
        end: l.point(0, 1).unwrap(),
    };
    assert_eq!(l.text_copy(sel), "한");
    assert_eq!(
        l.text_copy(Selection {
            start: l.point(2, 0).unwrap(),
            end: l.point(2, 1).unwrap()
        }),
        "글"
    );
}
