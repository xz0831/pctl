//! Independent W2 review cases. Known bugs remain active failing regressions.
use mdv::{Kind, LinkMap, Style, parse, to_ansi, to_html};
use unicode_width::UnicodeWidthStr;

fn plain(src: &str) -> Vec<String> {
    to_ansi(&parse(src), src, 40, &Style::default())
        .into_iter()
        .map(|l| l.plain)
        .collect()
}

fn no_controls(s: &str) {
    assert!(s.chars().all(|c| !c.is_control() || c == '\n'), "{s:?}");
}

#[test]
fn indented_code_source_keeps_first_line_indentation() {
    for src in ["    let x = 1;\n", "\tlet x = 1;\n"] {
        let doc = parse(src);
        assert!(matches!(doc.blocks[0].kind, Kind::Code { .. }));
        assert_eq!(&src[doc.blocks[0].range.clone()], src);
    }
}

#[test]
fn reference_definition_has_a_source_address() {
    let src = "[label][r]\n\n[r]: local.md\n\nAfter.\n";
    let doc = parse(src);
    assert!(
        doc.blocks
            .iter()
            .any(|b| &src[b.range.clone()] == "[r]: local.md\n")
    );
    assert!(!parse("[r]: local.md\n").blocks.is_empty());
}

#[test]
fn nested_table_keeps_rows_and_columns_in_ansi() {
    let src = "- item\n\n  | A | B |\n  | - | - |\n  | x | y |\n";
    let lines = plain(src);
    assert!(lines.iter().any(|l| l.contains("A | B")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("x | y")), "{lines:?}");
}

#[test]
fn nested_heading_and_paragraph_are_separate_ansi_lines() {
    let lines = plain("- first\n\n  ## heading\n\n  paragraph\n");
    assert!(lines.iter().any(|l| l.trim() == "heading"), "{lines:?}");
    assert!(lines.iter().any(|l| l.trim() == "paragraph"), "{lines:?}");
}

#[test]
fn footnote_case_variants_resolve_to_the_same_definition() {
    let src = "One[^A] two[^a].\n\n[^A]: body\n";
    let html = to_html(&parse(src), src, &LinkMap::default());
    let definition_id = html
        .split("<aside id=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_eq!(
        html.matches(&format!("href=\"#{definition_id}\"")).count(),
        2,
        "{html}"
    );
}

#[test]
fn heading_and_footnote_ids_do_not_collide() {
    let src = "# fn-61\n\nOne[^a].\n\n[^a]: body\n";
    let html = to_html(&parse(src), src, &LinkMap::default());
    let ids: Vec<_> = html
        .split(" id=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "{html}");
}

#[test]
fn wrapped_prose_does_not_add_trailing_spaces() {
    let src = "word ".repeat(20) + "last";
    let lines = plain(&src);
    assert!(lines.iter().all(|l| !l.ends_with(' ')), "{lines:?}");
}

#[test]
fn exact_ranges_for_other_requested_block_shapes() {
    for (src, expected) in [
        (
            "Title\n=====\n\nAfter.\n",
            vec!["Title\n=====\n", "After.\n"],
        ),
        (
            "---\na: b\n---\n\nAfter.\n",
            vec!["---\na: b\n---", "After.\n"],
        ),
        (
            "[^n]: body\n    more\n\nAfter.\n",
            vec!["[^n]: body\n    more\n\n", "After.\n"],
        ),
        (
            "- item\n  > quote\n  >\n  > ```rust\n  > x();\n  > ```\n\nAfter.\n",
            vec![
                "- item\n  > quote\n  >\n  > ```rust\n  > x();\n  > ```\n\n",
                "After.\n",
            ],
        ),
        (
            "<div>\nx\n</div>\n\nAfter.\n",
            vec!["<div>\nx\n</div>\n", "After.\n"],
        ),
        (
            "| A | B |\n| - | - |\n| x | y |\n\nAfter.\n",
            vec!["| A | B |\n| - | - |\n| x | y |\n", "After.\n"],
        ),
    ] {
        let doc = parse(src);
        let actual: Vec<_> = doc.blocks.iter().map(|b| &src[b.range.clone()]).collect();
        assert_eq!(actual, expected);
        let mut end = 0;
        for (id, b) in doc.blocks.iter().enumerate() {
            assert_eq!(b.id, id);
            assert!(b.range.start >= end);
            end = b.range.end;
        }
    }
}

#[test]
fn new_hostile_inputs_are_inert_in_all_output_channels() {
    let controls: Vec<_> = (0..=0x9f)
        .filter(|n| *n < 32 || *n >= 127)
        .filter_map(char::from_u32)
        .filter(|c| *c != '\n')
        .collect();
    let style = Style::default();
    for c in controls {
        let attack = format!("{c}\"'><script>alert(1)</script>");
        let src = format!(
            "# {attack}\n\n```{c}\"onload=evil\n{attack}\n```\n\n\
             ![{attack}](local.png \"title &quot; onerror=evil\")\n\n\
             [link](local.md \"title &quot; onmouseover=evil\")\n\n\
             | {attack} | x |\n| :- | -: |\n| y | z |\n\n\
             ref[^x{c}].\n\n[^x{c}]: {attack}\n\n<div>{attack}</div>\n"
        );
        let doc = parse(&src);
        let html = to_html(&doc, &src, &LinkMap::default());
        no_controls(&html);
        for bad in [
            "<script",
            "<svg",
            " onerror=\"",
            " onload=\"",
            " onmouseover=\"",
        ] {
            assert!(!html.contains(bad), "{html:?}");
        }
        for line in to_ansi(&doc, &src, 40, &style) {
            no_controls(&line.plain);
            let mut stripped = line.styled;
            for seq in [
                &style.heading,
                &style.code,
                &style.quote,
                &style.table,
                &style.reset,
            ] {
                stripped = stripped.replace(seq, "");
            }
            assert_eq!(stripped, line.plain);
            no_controls(&stripped);
        }
    }
    for dest in [
        "jav&#x61;script:alert%281%29",
        "javascript&#58;evil",
        "JaVaScRiPt:evil",
        "data:text/html,evil",
        "vbscript:evil",
        "file:///tmp/x",
        "blob:evil",
    ] {
        let src = format!("[x]({dest}) ![x]({dest})");
        let html = to_html(&parse(&src), &src, &LinkMap::default());
        assert!(!html.contains("<a ") && !html.contains("<img "), "{html}");
    }
    let src = "[x](local) ![y](local.png)";
    for dest in [
        "javascript:evil",
        "\thttps://example.invalid",
        "\u{009b}evil",
        "\\\\example.invalid/x",
    ] {
        let map = LinkMap {
            rewrite: &|_| dest.into(),
        };
        let html = to_html(&parse(src), src, &map);
        assert!(!html.contains("<a ") && !html.contains("<img "), "{html}");
    }
    let map = LinkMap {
        rewrite: &|_| "/safe/\"'><script>evil</script>".into(),
    };
    let html = to_html(&parse(src), src, &map);
    assert!(!html.contains("<script"));
    assert!(html.contains("/safe/&quot;&#39;&gt;&lt;script&gt;"));
}

#[test]
fn width_and_row_block_mapping_with_deep_prefixes_at_40() {
    let deep = (0..25)
        .map(|n| format!("{}- item {}\n", "  ".repeat(n), "word ".repeat(15)))
        .collect::<String>();
    let src = format!(
        "{deep}\n{}text\n\nFoot[^n].\n\n[^n]: {}\n",
        "> ".repeat(25),
        "word ".repeat(30)
    );
    let doc = parse(&src);
    for width in [40, 80, 120] {
        for line in to_ansi(&doc, &src, width, &Style::default()) {
            assert!(UnicodeWidthStr::width(line.plain.as_str()) <= width);
            assert!(!line.horizontally_scrollable);
            let block = &doc.blocks[line.block_id];
            assert_eq!(block.id, line.block_id);
            assert!(!src[block.range.clone()].is_empty());
        }
    }
}

#[test]
#[ignore = "manual timing and whitespace observations; no timing threshold"]
fn review_measurements() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/gallery/hangul_wrap.md"
    ))
    .unwrap();
    for width in [40, 80, 120] {
        let lines = to_ansi(&parse(&src), &src, width, &Style::default());
        println!(
            "hangul width={width}: {} / {} lines end in a space",
            lines.iter().filter(|l| l.plain.ends_with(' ')).count(),
            lines.len()
        );
    }
    for n in [1000, 2000, 4000, 8000] {
        let src = "- item\n\n".repeat(n);
        let doc = parse(&src);
        let start = std::time::Instant::now();
        let html = to_html(&doc, &src, &LinkMap::default());
        println!(
            "loose-list n={n}, html_bytes={}, elapsed={:?}",
            html.len(),
            start.elapsed()
        );
    }
}
