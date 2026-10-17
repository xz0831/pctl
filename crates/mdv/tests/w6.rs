use mdv::threads::{Event, MAX_STORE_BYTES, append, new_id, read, target};
use mdv::{Style, parse, to_ansi};
use std::{fs, path::PathBuf};
use unicode_width::UnicodeWidthStr;
fn file() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("mdv-w6-{}", new_id()))
        .join(format!("store-{}.jsonl", new_id()));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}
fn event() -> Event {
    Event {
        kind: "comment".into(),
        req_id: new_id(),
        path: "/synthetic.md".into(),
        feed_id: None,
        block_id: 0,
        range: [0, 1],
        fingerprint: "ab".into(),
        text: "hello".into(),
        source: "web".into(),
        time: 0,
        agent: None,
        quote: None,
        before: None,
        after: None,
        doc_hash: None,
        selected: None,
    }
}
#[test]
fn prose_cap_and_custom_measure_leave_code_scrollable() {
    let src = format!(
        "{}\n\n```\n{}\n```\n",
        "한글 본문 ".repeat(60),
        "0123456789".repeat(30)
    );
    let doc = parse(&src);
    for (style, cap) in [
        (Style::default(), 80),
        (
            Style {
                measure: 60,
                ..Style::default()
            },
            60,
        ),
    ] {
        let lines = to_ansi(&doc, &src, 200, &style);
        assert!(
            lines
                .iter()
                .filter(|l| l.block_id == 0)
                .all(|l| UnicodeWidthStr::width(l.plain.as_str()) <= cap)
        );
        assert!(
            lines
                .iter()
                .any(|l| l.horizontally_scrollable && l.plain.len() == 300)
        );
    }
}
#[test]
fn text_tables_wrap_columns_and_numeric_tables_scroll_without_splitting() {
    let src = include_str!("fixtures/gallery/prose_table.md");
    let lines = to_ansi(&parse(src), src, 80, &Style::default());
    let table: Vec<_> = lines.iter().filter(|l| l.plain.contains(" | ")).collect();
    assert!(table.len() > 5);
    assert!(
        table
            .iter()
            .all(|l| UnicodeWidthStr::width(l.plain.as_str()) <= 80 && !l.horizontally_scrollable)
    );
    let separator_columns = |text: &str| {
        text.split(" | ")
            .map(UnicodeWidthStr::width)
            .collect::<Vec<_>>()
    };
    assert!(
        table
            .iter()
            .all(|l| separator_columns(&l.plain) == separator_columns(&table[0].plain))
    );
    for value in ["123456789.012", "-987654321.125"] {
        assert_eq!(table.iter().filter(|l| l.plain.contains(value)).count(), 1);
    }
    let numbers = "| a | b |\n|---|---|\n|12345678901234567890| -98765432109876543210 |\n";
    let lines = to_ansi(&parse(numbers), numbers, 12, &Style::default());
    assert!(lines.iter().any(|l| l.horizontally_scrollable
        && l.plain.contains("12345678901234567890")
        && l.plain.contains("-98765432109876543210")));
}
#[test]
fn tail_is_quarantined_exactly_and_interior_corruption_stays_an_error() {
    let p = file();
    append(&p, event()).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"{\"kind\":")
        .unwrap();
    assert_eq!(read(&p).unwrap().len(), 1);
    append(&p, event()).unwrap();
    assert_eq!(read(&p).unwrap().len(), 2);
    let prefix = format!("{}.", p.file_stem().unwrap().to_string_lossy());
    let preserved = fs::read_dir(p.parent().unwrap())
        .unwrap()
        .flatten()
        .find(|e| e.path() != p && e.file_name().to_string_lossy().starts_with(&prefix))
        .unwrap();
    assert_eq!(fs::read(preserved.path()).unwrap(), b"{\"kind\":");
    fs::write(
        &p,
        format!(
            "{}\nnot-json\n{}\n",
            serde_json::to_string(&event()).unwrap(),
            serde_json::to_string(&event()).unwrap()
        ),
    )
    .unwrap();
    let before = fs::read(&p).unwrap();
    assert!(read(&p).unwrap_err().to_string().contains("byte"));
    assert!(append(&p, event()).is_err());
    assert_eq!(fs::read(&p).unwrap(), before);
}
#[test]
fn store_limit_and_event_text_limits_preserve_disk_data() {
    let p = file();
    let f = fs::File::create(&p).unwrap();
    f.set_len(MAX_STORE_BYTES + 1).unwrap();
    assert!(read(&p).is_err());
    assert!(append(&p, event()).is_err());
    assert_eq!(fs::metadata(&p).unwrap().len(), MAX_STORE_BYTES + 1);
    let mut e = event();
    e.text = "한".repeat(2001);
    fs::write(&p, serde_json::to_string(&e).unwrap() + "\n").unwrap();
    assert!(read(&p).is_err());
}
#[test]
fn moved_unique_blocks_relocate_but_repeated_candidates_remain_ambiguous() {
    let doc = parse("same\n\nsame\n\nunique\n");
    let mut e = event();
    e.fingerprint = format!("{:016x}", doc.blocks[0].fingerprint);
    e.block_id = 100;
    e.range = [100, 101];
    assert_eq!(target(&e, &doc), None);
    e.block_id = 1;
    e.range = [doc.blocks[1].range.start, doc.blocks[1].range.end];
    assert_eq!(target(&e, &doc), Some(1));
    e.fingerprint = format!("{:016x}", doc.blocks[2].fingerprint);
    assert_eq!(target(&e, &doc), Some(2));
}
#[test]
fn wrapped_table_selection_maps_to_canonical_cells() {
    use mdv::reader::{Layout, Selection};
    let src = include_str!("fixtures/gallery/prose_table.md").to_owned() + "\nAfter table.\n";
    let doc = parse(&src);
    let layout = Layout::new(&doc, &src, 80, &Style::default());
    let number = "123456789.012";
    let (row, line) = layout
        .rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.line.plain.contains(number))
        .unwrap();
    let byte = line.line.plain.find(number).unwrap();
    let cell = UnicodeWidthStr::width(&line.line.plain[..byte]);
    let selected = Selection {
        start: layout.point(row, cell).unwrap(),
        end: layout.point(row, cell + number.len()).unwrap(),
    };
    assert_eq!(layout.text_copy(selected), number);
    let narrow = Layout::new(&doc, &src, 40, &Style::default());
    assert_eq!(narrow.text_copy(selected), number);
    assert_eq!(layout.rows.last().unwrap().line.plain, "After table.");
}
#[test]
fn interrupted_utf8_tail_preserves_valid_prefix_and_exact_tail_bytes() {
    let p = file();
    append(&p, event()).unwrap();
    use std::io::Write;
    let tail = b"{\"kind\":\"comment\",\"text\":\"\xed\x95";
    fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(tail)
        .unwrap();
    assert_eq!(read(&p).unwrap().len(), 1);
    append(&p, event()).unwrap();
    assert_eq!(read(&p).unwrap().len(), 2);
    let prefix = format!("{}.", p.file_stem().unwrap().to_string_lossy());
    let preserved = fs::read_dir(p.parent().unwrap())
        .unwrap()
        .flatten()
        .find(|e| e.path() != p && e.file_name().to_string_lossy().starts_with(&prefix))
        .unwrap();
    assert_eq!(fs::read(preserved.path()).unwrap(), tail);
}
