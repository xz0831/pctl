use mdv::{Kind, LinkMap, Style, parse, to_ansi, to_html};
#[test]
fn definitions_indentation_and_rewrap() {
    let src = "   # Title\n\n[r]: one.md\n\n[r]: two.md\n\n    x  \n";
    let doc = parse(src);
    assert_eq!(&src[doc.blocks[0].range.clone()], "   # Title\n");
    assert!(
        doc.blocks
            .iter()
            .filter(|b| b.kind == Kind::Definition)
            .count()
            >= 2
    );
    let lines = to_ansi(&doc, src, 40, &Style::default());
    assert!(lines.iter().any(|l| l.plain == "x  "));
    assert_eq!(
        lines.len(),
        to_ansi(&doc, "unused cached source", 40, &Style::default()).len()
    );
}
#[test]
fn paragraph_math_and_unicode_footnote_equivalence() {
    let src = "- ordinary\n\n- $$x$$\n\nOne[^SS] two[^ß].\n\n[^ß]: body\n";
    let html = to_html(&parse(src), src, &LinkMap::default());
    assert!(html.contains("<p>ordinary</p>"));
    assert!(html.contains("<div class=\"paragraph\">"));
    assert_eq!(html.matches("href=\"#fn-c39f\"").count(), 2);
}
#[test]
fn nested_code_keeps_authored_wrap_whitespace() {
    let src = "- item\n\n  ```\n  abc    def    ghi  \n  ```\n";
    let doc = parse(src);
    let lines = to_ansi(&doc, src, 7, &Style::default());
    let text: String = lines
        .iter()
        .filter(|l| !l.plain.starts_with('•'))
        .map(|l| l.plain.as_str())
        .collect();
    assert_eq!(text, "abc    def    ghi  ");
}
