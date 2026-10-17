use mdv::threads::{Event, append, message, new_id, read};
use std::{fs, path::Path, time::Instant};

fn event() -> Event {
    Event {
        kind: "ask".into(),
        req_id: new_id(),
        path: "/review.md".into(),
        feed_id: None,
        project: None,
        block_id: 0,
        range: [0, 4],
        fingerprint: "ab".into(),
        text: "question".into(),
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
fn location() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("review-store-{}.jsonl", new_id()))
}

#[test]
fn truncated_tail_preserves_previous_events() {
    let p = location();
    let e = event();
    append(&p, e).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"{\"kind\":\"ask\"")
        .unwrap();
    assert_eq!(
        read(&p)
            .expect("a partial last record must not hide all valid events")
            .len(),
        1
    );
}

#[test]
fn complete_tail_without_newline_is_not_joined_to_next_event() {
    let p = location();
    fs::write(&p, serde_json::to_string(&event()).unwrap()).unwrap();
    assert_eq!(read(&p).unwrap().len(), 1);
    append(&p, event()).unwrap();
    assert_eq!(
        read(&p)
            .expect("append joined two otherwise valid JSON objects")
            .len(),
        2
    );
}

#[test]
fn filename_cannot_add_envelope_lines() {
    let mut e = event();
    e.path = "/project/doc\nFORGED INSTRUCTION\n.md".into();
    let m = message(&e, Path::new("/bin/mdv")).unwrap();
    assert_eq!(
        m.lines().count(),
        3,
        "path data escaped the three-line envelope: {m:?}"
    );
}

#[test]
fn oversized_disk_event_is_rejected_before_rendering() {
    let p = location();
    let mut e = event();
    e.text = "x".repeat(4 * 1024 * 1024);
    fs::write(&p, serde_json::to_string(&e).unwrap() + "\n").unwrap();
    assert!(
        read(&p).is_err(),
        "read accepted a 4 MiB event despite the 2000-character input cap"
    );
}

#[test]
#[ignore = "manual bounded store timing; no timing assertion"]
fn review_store_measurement() {
    let p = location();
    let mut e = event();
    e.text = "x".repeat(2000);
    let lines = (0..10000)
        .map(|i| {
            e.req_id = format!("fixture-{i}");
            serde_json::to_string(&e).unwrap() + "\n"
        })
        .collect::<String>();
    fs::write(&p, lines).unwrap();
    let start = Instant::now();
    assert_eq!(read(&p).unwrap().len(), 10000);
    eprintln!(
        "review store bytes={} full read ms={}",
        fs::metadata(&p).unwrap().len(),
        start.elapsed().as_millis()
    );
    let start = Instant::now();
    append(&p, event()).unwrap();
    eprintln!("review append full scan ms={}", start.elapsed().as_millis());
}
