use mdv::{
    Look, Style, parse,
    threads::{self, Event},
};
use std::{fs, path::Path};
const OLD: &str = "이 구현은 메모리 사용량이 줄어들기 때문에 유리하다.";
const NEW: &str = "이 구현은 처리 지연이 줄어들기 때문에 유리하다.";
fn event(src: &str, id: usize) -> Event {
    let doc = parse(src);
    let b = &doc.blocks[id];
    serde_json::from_value(serde_json::json!({
        "kind":"ask", "req_id":threads::new_id(), "path":"/synthetic.md", "feed_id":null,
        "block_id":id,"range":[b.range.start,b.range.end],"fingerprint":format!("{:016x}",b.fingerprint),
        "text":"이 판단의 근거가 무엇인가요?", "source":"terminal"
    })).unwrap()
}
#[test]
fn ask_edit_answer_preserves_original_and_message_contains_only_quote() {
    let src = format!("# 이전 문맥\n\n{OLD}\n\n뒤 문맥입니다.\n");
    let mut e = event(&src, 1);
    e.selected = Some("메모리 사용량".into());
    threads::capture(&mut e, &src, &parse(&src)).unwrap();
    assert_eq!(e.quote.as_deref(), Some(format!("{OLD}\n").as_str()));
    assert_eq!(e.before.as_deref(), Some("# 이전 문맥\n\n"));
    assert_eq!(e.after.as_deref(), Some("\n뒤 문맥입니다.\n"));
    assert_eq!(e.doc_hash.as_ref().unwrap().len(), 64);
    let output = std::env::temp_dir().join(format!("mdv-w7-{}", e.req_id));
    fs::create_dir_all(&output).unwrap();
    let store = output.join(format!("snapshot-{}.jsonl", e.req_id));
    threads::append(&store, e.clone()).unwrap();
    let mut answer = e.clone();
    answer.kind = "answer".into();
    answer.text = "메모리 주장에 대한 합성 답변입니다.".into();
    threads::append(&store, answer).unwrap();
    let events = threads::read(&store).unwrap();
    let current = src.replace(OLD, NEW);
    let changed = parse(&current);
    for look in [Look::Portal, Look::Garage] {
        for dark in [false, true] {
            let style = Style::for_look(look, dark);
            assert_eq!(threads::target(&events[0], &changed), None);
            let text = threads::display(&events[0], true, &style);
            assert!(text.contains("질문 당시 원문\n"));
            assert!(text.contains(OLD));
            assert!(!text.contains(NEW));
            assert!(text.contains(&style.broken_mark));
            assert_eq!(events[0].doc_hash, events[1].doc_hash);
        }
    }
    let msg = threads::message(&e, Path::new("/bin/mdv")).unwrap();
    assert!(msg.contains(&serde_json::to_string(e.quote.as_ref().unwrap()).unwrap()));
    assert!(msg.contains("이 인용문에 대해 답하세요"));
    assert!(msg.contains("현재 파일이 질문 당시 파일과 다르면"));
    assert!(!msg.contains("이전 문맥"));
    assert!(!msg.contains("뒤 문맥"));
    fs::write(output.join("message.txt"), msg).unwrap();
}
#[test]
fn old_event_no_record_size_cap_context_and_controls() {
    let e = event("old\n", 0);
    for look in [Look::Portal, Look::Garage] {
        assert!(
            threads::display(&e, true, &Style::for_look(look, false))
                .contains("질문 당시 원문 기록 없음 (이전 기록)")
        );
    }
    let src = format!(
        "{}\n\n{}\n\n{}",
        "앞".repeat(200),
        "한\u{1b}\u{7}\u{85}".repeat(3000),
        "뒤".repeat(200)
    );
    let mut e = event(&src, 1);
    threads::capture(&mut e, &src, &parse(&src)).unwrap();
    let quote = e.quote.unwrap();
    assert!(quote.len() <= threads::QUOTE_LIMIT);
    assert!(quote.ends_with("[… 원문 생략]"));
    assert!(
        quote
            .chars()
            .all(|c| !c.is_control() || c == '\n' || c == '\t')
    );
    assert_eq!(e.before.unwrap().chars().count(), 160);
    assert_eq!(e.after.unwrap().chars().count(), 160);
    for n in 4080..4110 {
        assert!(threads::excerpt(&"한".repeat(n)).len() <= threads::QUOTE_LIMIT);
    }
    assert_eq!(threads::excerpt("a\tb\nc\r\u{1b}d"), "a\tb\ncd");
}
#[test]
fn hash_is_sha256_of_exact_bytes_and_selection_must_be_in_target() {
    let mut e = event("abc", 0);
    threads::capture(&mut e, "abc", &parse("abc")).unwrap();
    assert_eq!(
        e.doc_hash.as_ref().unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    e.selected = Some("elsewhere".into());
    assert!(threads::capture(&mut e, "abc", &parse("abc")).is_err());
}
