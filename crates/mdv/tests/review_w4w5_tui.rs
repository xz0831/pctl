// Test the private production App without starting a terminal or sending any message.
#[allow(dead_code)]
mod tui {
    include!("../src/view.rs");

    fn fixture(src: &str) -> App {
        let style = Style::default();
        let doc = parse(src);
        let layout = Layout::new(&doc, src, 118, &style);
        App {
            copy_hook: Hooks::default().copy,
            ask_hook: Arc::new(Mutex::new(Hooks::default().ask)),
            state: State::default(),
            doc,
            src: src.into(),
            layout,
            path: PathBuf::new(),
            project: None,
            tree: PathBuf::new(),
            entries: vec![],
            inbox: Inbox::default(),
            feed: PathBuf::new(),
            feed_stamp: None,
            file_stamp: None,
            style,
            size: (120, 24),
            search: None,
            query: String::new(),
            matches: vec![],
            match_at: 0,
            menu: None,
            composer: None,
            threads: vec![],
            thread_stamp: None,
            pending_thread: None,
            message: String::new(),
            converted: false,
            content_available: true,
            conversion: None,
            horizontal: 0,
            seen_store: None,
            changes: Vec::new(),
            changed_blocks: Vec::new(),
            lens_warning: None,
            host: "review".into(),
        }
    }
    fn event(app: &App, block_id: usize, text: &str) -> mdv::threads::Event {
        let b = &app.doc.blocks[block_id];
        mdv::threads::Event {
            kind: "comment".into(),
            req_id: mdv::threads::new_id(),
            path: String::new(),
            feed_id: None,
            project: None,
            block_id,
            range: [b.range.start, b.range.end],
            fingerprint: format!("{:016x}", b.fingerprint),
            text: text.into(),
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
    fn review_repeated_block_does_not_duplicate_discussion() {
        let mut app = fixture("# Intro\n\n## Same\n\n## Same\n\nbody\n");
        app.threads = vec![event(&app, 2, "only-second-heading")];
        app.rewrap();
        let attached: Vec<_> = app
            .layout
            .rows
            .iter()
            .filter(|r| r.discussion && r.line.plain.contains("only-second-heading"))
            .map(|r| r.line.block_id)
            .collect();
        assert_eq!(
            attached,
            vec![2],
            "one comment must remain at its requested block"
        );
    }
    #[test]
    fn review_reply_keeps_content_at_top() {
        let src = (0..50)
            .map(|i| format!("paragraph {i}\n\n"))
            .collect::<String>();
        let mut app = fixture(&src);
        app.state.top = 20;
        let before = app.layout.anchor(app.state.top);
        if env::var_os("MDV_REVIEW_CHILD").is_none() {
            let p =
                std::env::temp_dir().join(format!("review-tui-{}.jsonl", mdv::threads::new_id()));
            mdv::threads::append(&p, event(&app, 0, &"reply ".repeat(100))).unwrap();
            let result = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tui::review_reply_keeps_content_at_top",
                    "--nocapture",
                ])
                .env("MDV_THREADS_FILE", p)
                .env("MDV_REVIEW_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let changed = app.poll_threads();
        assert!(changed);
        assert_eq!(
            app.layout.anchor(app.state.top),
            before,
            "numeric row stayed put but the reading passage moved"
        );
    }

    #[test]
    fn changed_quote_is_detached_from_new_sentence_in_real_layout() {
        let old = "이 구현은 메모리 사용량이 줄어들기 때문에 유리하다.";
        let new = "이 구현은 처리 지연이 줄어들기 때문에 유리하다.";
        let original = format!("# 합성 문서\n\n{old}\n\n아래 본문입니다.\n");
        let doc = parse(&original);
        let b = &doc.blocks[1];
        let mut e: mdv::threads::Event = serde_json::from_value(serde_json::json!({
            "kind":"ask", "req_id":"w7-fixture", "path":"", "feed_id":null,
            "block_id":b.id,"range":[b.range.start,b.range.end],"fingerprint":format!("{:016x}",b.fingerprint),
            "text":"근거는?", "source":"terminal"
        })).unwrap();
        mdv::threads::capture(&mut e, &original, &doc).unwrap();
        for look in [mdv::Look::Portal, mdv::Look::Garage] {
            let mut app = fixture(&original.replace(old, new));
            app.style = Style::for_look(look, false);
            let mut answer = e.clone();
            answer.kind = "answer".into();
            answer.text = "메모리 주장에 대한 답입니다.".into();
            app.threads = vec![e.clone(), answer];
            app.rewrap();
            let rows = &app.layout.rows;
            let quote = rows
                .iter()
                .position(|r| r.discussion && r.line.plain.contains(old))
                .unwrap();
            let tail = rows.iter().rposition(|r| !r.discussion).unwrap();
            assert!(
                quote > tail,
                "old question must be detached at document end"
            );
            assert!(
                rows.iter()
                    .any(|r| !r.discussion && r.line.plain.contains(new))
            );
            assert!(
                !rows
                    .iter()
                    .any(|r| r.discussion && r.line.plain.contains(new))
            );
            let plain = rows
                .iter()
                .map(|r| r.line.plain.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(plain.contains("질문 당시 원문"));
            assert!(plain.contains("연결 끊김"));
            app.src.clear();
            app.doc = parse("");
            app.rewrap();
            let empty_text = app
                .layout
                .rows
                .iter()
                .map(|r| r.line.plain.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                empty_text.contains(old),
                "empty current document still retains the old question"
            );
        }
    }
}
