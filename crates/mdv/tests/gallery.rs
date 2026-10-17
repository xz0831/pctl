use mdv::{Doc, Kind, LinkMap, Style, parse, to_ansi, to_html, visible};
use std::{
    fs,
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn fixtures(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() && p.file_name().unwrap() != "checker" {
            fixtures(&p, out);
        } else if p.extension().is_some_and(|e| e == "md")
            && !matches!(
                p.file_name().unwrap().to_str(),
                Some("README.md" | "BASELINE.md")
            )
        {
            out.push(p);
        }
    }
}
fn sources() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    fixtures(&root().join("tests/fixtures/gallery"), &mut paths);
    paths.sort();
    assert_eq!(paths.len(), 18);
    paths
}
fn ranges(doc: &Doc, src: &str) {
    let mut end = 0;
    for (id, b) in doc.blocks.iter().enumerate() {
        assert_eq!(b.id, id);
        assert!(b.range.start >= end);
        assert!(b.range.end <= src.len());
        assert!(b.range.start < b.range.end);
        assert!(src.is_char_boundary(b.range.start) && src.is_char_boundary(b.range.end));
        assert_eq!(
            b.fingerprint,
            parse(&src[b.range.clone()])
                .blocks
                .first()
                .unwrap()
                .fingerprint
        );
        end = b.range.end;
    }
}
fn no_controls(text: &str) {
    assert!(
        text.chars().all(|c| !c.is_control() || c == '\n'),
        "control in output: {text:?}"
    );
}
#[test]
fn golden_gallery() {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for path in sources() {
        let src = fs::read_to_string(&path).unwrap();
        let doc = parse(&src);
        let name = path
            .strip_prefix(root().join("tests/fixtures/gallery"))
            .unwrap();
        let base = golden.join(name);
        fs::create_dir_all(base.parent().unwrap()).unwrap();
        let ansi = to_ansi(&doc, &src, 80, &Style::default())
            .iter()
            .map(|l| l.styled.clone() + "\n")
            .collect::<String>();
        for (suffix, actual) in [
            ("w80.ansi", ansi),
            ("html", to_html(&doc, &src, &LinkMap::default())),
        ] {
            let snapshot = base.with_extension(suffix);
            if std::env::var_os("MDV_UPDATE_GOLDEN").is_some() {
                fs::write(&snapshot, &actual).unwrap();
            }
            assert_eq!(
                fs::read_to_string(&snapshot).unwrap(),
                actual,
                "snapshot {}",
                snapshot.display()
            );
        }
    }
}
#[test]
fn properties_all_fixtures() {
    for path in sources() {
        let src = fs::read_to_string(&path).unwrap();
        let doc = parse(&src);
        ranges(&doc, &src);
        for width in [40, 80, 120] {
            for line in to_ansi(&doc, &src, width, &Style::default()) {
                no_controls(&line.plain);
                assert!(
                    UnicodeWidthStr::width(line.plain.as_str()) <= width
                        || (line.horizontally_scrollable
                            && matches!(
                                doc.blocks[line.block_id].kind,
                                Kind::Table
                                    | Kind::Code { .. }
                                    | Kind::Mermaid
                                    | Kind::Math
                                    | Kind::RawHtml
                                    | Kind::FrontMatter
                            )),
                    "{} width {}: {:?}",
                    path.display(),
                    width,
                    line.plain
                );
                let style = Style::default();
                let mut styled = line.styled;
                for seq in [
                    &style.heading,
                    &style.code,
                    &style.quote,
                    &style.table,
                    &style.reset,
                ] {
                    styled = styled.replace(seq, "");
                }
                no_controls(&styled);
                assert_eq!(styled, line.plain);
            }
        }
        let html = to_html(&doc, &src, &LinkMap::default());
        no_controls(&html);
        let lower = html.to_ascii_lowercase();
        for bad in [
            "href=\"javascript:",
            "href=\"data:",
            "href=\"vbscript:",
            "<script",
            "<iframe",
            " onerror=\"",
            " onload=\"",
        ] {
            assert!(!lower.contains(bad), "{}: {bad}", path.display());
        }
        assert_eq!(html.matches("<section data-b=").count(), doc.blocks.len());
    }
}
#[test]
fn block_mapping_and_heading_context() {
    let src = "# A\n\npara *b*\n\n## B\n\n- item\n  - nested\n\n> quote\n\n```rust\nlet x=1;\n```\n\n---\n";
    let doc = parse(src);
    ranges(&doc, src);
    assert_eq!(
        doc.blocks
            .iter()
            .map(|b| b.kind.clone())
            .collect::<Vec<_>>(),
        vec![
            Kind::Heading { level: 1 },
            Kind::Paragraph,
            Kind::Heading { level: 2 },
            Kind::List,
            Kind::Quote,
            Kind::Code {
                language: "rust".into()
            },
            Kind::Rule
        ]
    );
    assert_eq!(doc.blocks[1].heading_path, vec!["A"]);
    assert_eq!(doc.blocks[3].heading_path, vec!["A", "B"]);
    assert_eq!(&src[doc.blocks[3].range.clone()], "- item\n  - nested\n\n");
    let changed = src.replacen("para", "edited", 1);
    assert_ne!(
        doc.blocks[1].fingerprint,
        parse(&changed).blocks[1].fingerprint
    );
}
#[test]
fn unicode_wrap_does_not_split_graphemes() {
    let src = "한글 한글 👩🏽\u{200d}💻 🇰🇷 e\u{301}abcdefghijklmnopqrstuvwxyz";
    let doc = parse(src);
    let gs: Vec<_> = src.graphemes(true).collect();
    for width in [2, 3, 4, 5, 40, 80, 120] {
        let lines = to_ansi(&doc, src, width, &Style::default());
        assert_eq!(
            lines
                .iter()
                .map(|l| l.plain.replace(' ', ""))
                .collect::<String>(),
            src.replace(' ', "")
        );
        for line in lines {
            assert!(UnicodeWidthStr::width(line.plain.as_str()) <= width);
            for g in line.plain.graphemes(true) {
                assert!(gs.contains(&g));
            }
        }
    }
    for width in [0, 1] {
        for line in to_ansi(&doc, src, width, &Style::default()) {
            assert!(UnicodeWidthStr::width(line.plain.as_str()) <= width);
        }
    }
}
#[test]
fn hostile_bytes_are_visible_everywhere() {
    let controls = (0..=0x9fu32)
        .filter(|n| *n < 32 || *n >= 127)
        .filter_map(char::from_u32)
        .collect::<String>();
    let v = visible(&controls);
    no_controls(&v);
    assert!(v.contains('␛'));
    assert!(v.contains('␇'));
    assert!(v.contains("U+009B"));
    assert!(!v.contains('\t'));
    let src = fs::read_to_string(root().join("tests/fixtures/gallery/hostile.log")).unwrap();
    let doc = parse(&src);
    for l in to_ansi(&doc, &src, 40, &Style::default()) {
        no_controls(&l.plain);
    }
    no_controls(&to_html(&doc, &src, &LinkMap::default()));
}
#[test]
fn links_rewrites_and_images_are_safe() {
    let src = "[relative](a.md) [anchor](#a) [mail](mailto:x@example.invalid) [web](https://example.invalid) [blocked](JaVaScRiPt:alert%281%29)\n\n![relative](local.png) ![remote](https://example.invalid/no.png) ![protocol](//example.invalid/no.png) ![blocked](data:image/png;base64,AA)\n";
    let map = LinkMap {
        rewrite: &|s| format!("/safe/{s}"),
    };
    let html = to_html(&parse(src), src, &map);
    assert!(html.contains("href=\"/safe/a.md\""));
    assert!(html.contains("src=\"/safe/local.png\""));
    assert!(html.contains("href=\"#a\""));
    assert!(html.contains("href=\"https://example.invalid/no.png\""));
    assert_eq!(html.matches("<img ").count(), 1);
    assert!(!html.contains("JaVaScRiPt:"));
    assert!(!html.contains("data:image"));
    for rewrite in [
        "javascript:evil",
        "data:text/html,evil",
        "vbscript:evil",
        "\nhttps://example.invalid",
        "\\\\example.invalid",
    ] {
        let map = LinkMap {
            rewrite: &|_| rewrite.to_owned(),
        };
        let html = to_html(
            &parse("[x](relative) ![y](relative.png)"),
            "[x](relative) ![y](relative.png)",
            &map,
        );
        assert!(!html.contains("<a "));
        assert!(!html.contains("<img "));
    }
    let map = LinkMap {
        rewrite: &|_| "https://example.invalid/remote.png".into(),
    };
    let html = to_html(&parse("![x](local.png)"), "![x](local.png)", &map);
    assert!(!html.contains("<img "));
    assert!(html.contains("<a "));
}
#[test]
fn raw_html_math_mermaid_and_frontmatter() {
    let src = "---\ntitle: Synthetic\n---\n\n<script>alert(1)</script>\n\nInline <span onload=\"x\">hello</span> $a < b$.\n\n$$a < b$$\n\n```mermaid\nA[<script>] --> B\n```\n";
    let doc = parse(src);
    let html = to_html(&doc, src, &LinkMap::default());
    assert_eq!(doc.blocks[0].kind, Kind::FrontMatter);
    assert!(doc.blocks.iter().any(|b| b.kind == Kind::Math));
    assert!(doc.blocks.iter().any(|b| b.kind == Kind::Mermaid));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("<span class=\"math\">a &lt; b</span>"));
    assert!(html.contains("<div class=\"math\">a &lt; b</div>"));
    assert!(html.contains("<pre class=\"mermaid\">A[&lt;script&gt;]"));
    assert!(!html.contains("<script"));
}
#[test]
fn unique_heading_ids_even_with_slug_collisions() {
    let src = "# A\n\n# A\n\n# A-2\n\n# A\n\n# 한글\n\n# 한글\n\n# !!!\n";
    let html = to_html(&parse(src), src, &LinkMap::default());
    let ids: Vec<_> = html
        .split(" id=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    let set: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(set.len(), 7);
    assert!(ids.contains(&"한글-2"));
}
#[test]
fn references_survive_whole_document_parse() {
    let src = "[**label**][r]\n\n[r]: local.md\n\nFootnote[^a].\n\n[^a]: Footnote **body**.\n";
    let html = to_html(&parse(src), src, &LinkMap::default());
    assert!(html.contains("<a href=\"local.md\" title=\"\"><strong>label</strong></a>"));
    assert!(html.contains("href=\"#fn-61\""));
    assert!(html.contains("id=\"fn-61\""));
}
#[test]
fn tables_preserve_all_cells_and_alignment() {
    let src = fs::read_to_string(root().join("tests/fixtures/gallery/wide_table.md")).unwrap();
    let doc = parse(&src);
    let lines = to_ansi(&doc, &src, 80, &Style::default());
    assert!(
        lines
            .iter()
            .any(|l| l.plain.contains("합성1000") && l.horizontally_scrollable)
    );
    let html = to_html(&doc, &src, &LinkMap::default());
    assert!(html.contains("합성1000"));
    assert_eq!(html.matches("<th ").count(), 1031);
    assert_eq!(html.matches("<td ").count(), 1031);
    assert!(html.contains("class=\"align-right\""));
}
#[test]
fn cli_arguments_and_output() {
    use std::process::Command;
    let binary = env!("CARGO_BIN_EXE_mdv");
    let file = root().join("tests/fixtures/gallery/math.md");
    let output = Command::new(binary)
        .arg("render")
        .arg(&file)
        .args(["--html"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("<section data-b=")
    );
    let output = Command::new(binary)
        .arg("render")
        .arg(&file)
        .args(["--width", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = Command::new(binary)
        .arg("render")
        .arg(&file)
        .args(["--width", "40"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !Command::new(binary)
            .arg("render")
            .arg("/nonexistent/synthetic.md")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn quote_in_list_starts_its_own_line() {
    let src = "- item\n  > quote\n  >\n  > next\n";
    let text = to_ansi(&parse(src), src, 80, &Style::default())
        .into_iter()
        .map(|l| l.plain)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, "• item\n│ quote\n│ next");
}
#[test]
fn display_math_in_a_paragraph_uses_valid_block_container() {
    let src = "Before $$x$$ after.";
    let html = to_html(&parse(src), src, &LinkMap::default());
    assert!(!html.contains("<p>Before <div"));
    assert!(
        html.contains("<div class=\"paragraph\">Before <div class=\"math\">x</div> after.</div>")
    );
}
