#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
#[link(name = "util")]
unsafe extern "C" {}

const TAB: &[u8] = b"\t";
const ENTER: &[u8] = b"\r";
const ESC: &[u8] = b"\x1b";
const DOWN: &[u8] = b"\x1b[B";
const CTRL_C: &[u8] = b"\x03";
const TIMEOUT: Duration = Duration::from_secs(4);
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    doc: PathBuf,
}
impl Fixture {
    fn new(name: &str, source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "mdv-pty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let folder = root.join("Projects/fixture");
        fs::create_dir_all(&folder).unwrap();
        let doc = folder.join(name);
        fs::write(&doc, source).unwrap();
        let bin = root.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let session = serde_json::json!({"name":"fixture-main", "project":"fixture", "agent_id":"fixture-agent", "run_id":"fixture-run", "process":"running"});
        fs::write(root.join("session"), session.to_string()).unwrap();
        fs::write(
            root.join("sessions"),
            serde_json::json!([session]).to_string(),
        )
        .unwrap();
        for (name, script) in [
            (
                "pctl",
                "#!/bin/sh\ncase \"$1 $2\" in\n'session list') /bin/cat \"$HOME/sessions\" ;;\n'session get') /bin/cat \"$HOME/session\" ;;\nesac\n",
            ),
            ("msg", "#!/bin/sh\n: > \"$HOME/msg-called\"\nexit 99\n"),
        ] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root, doc }
    }
    fn gallery(name: &str) -> Self {
        Self::new(
            name,
            &fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/gallery")
                    .join(name),
            )
            .unwrap(),
        )
    }
    fn command(&self, dry: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mdv"));
        cmd.env_clear()
            .env("HOME", &self.root)
            .env("MDV_THREADS_FILE", self.root.join("threads.jsonl"))
            .env("MDV_ASK_DRYRUN", self.root.join(dry))
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8");
        cmd
    }
    fn events(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join("threads.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn answer(&self, id: &str, answer: &str) {
        let output = self
            .command("answer-dry.json")
            .args(["answer", id, answer])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(
            !self.root.join("msg-called").exists(),
            "dry-run invoked msg"
        );
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Screen {
    parser: vt100::Parser,
    raw: Vec<u8>,
}
type Shared = Arc<(Mutex<Screen>, Condvar)>;
struct Pty {
    child: Child,
    master: File,
    _slave: File,
    slave_path: PathBuf,
    before: libc::termios,
    shared: Shared,
    reader: Option<JoinHandle<()>>,
}
fn termios(file: &File) -> libc::termios {
    let mut value = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(file.as_raw_fd(), &mut value) }, 0);
    value
}
fn terminal_flags(
    t: &libc::termios,
) -> (
    libc::tcflag_t,
    libc::tcflag_t,
    libc::tcflag_t,
    libc::tcflag_t,
    Vec<libc::cc_t>,
    libc::speed_t,
    libc::speed_t,
) {
    (
        t.c_iflag,
        t.c_oflag,
        t.c_cflag,
        t.c_lflag,
        t.c_cc.to_vec(),
        unsafe { libc::cfgetispeed(t) },
        unsafe { libc::cfgetospeed(t) },
    )
}
fn flat(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
impl Pty {
    fn new(f: &Fixture, args: &[&str], dry: &str) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let before = termios(&slave);
        let mut name = [0; 256];
        assert_eq!(
            unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) },
            0
        );
        let slave_path = PathBuf::from(
            unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
                .to_str()
                .unwrap(),
        );
        let mut cmd = f.command(dry);
        cmd.args(args)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let shared = Arc::new((
            Mutex::new(Screen {
                parser: vt100::Parser::new(40, 120, 0),
                raw: Vec::new(),
            }),
            Condvar::new(),
        ));
        let output = Arc::clone(&shared);
        let mut input = master.try_clone().unwrap();
        // Keep the slave open for the restoration check; nonblocking reads let cleanup join.
        assert_eq!(
            unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        let reader = thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match input.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut s = output.0.lock().unwrap();
                        s.parser.process(&buf[..n]);
                        s.raw.extend_from_slice(&buf[..n]);
                        output.1.notify_all();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if Arc::strong_count(&output) == 1 {
                            break;
                        }
                        thread::park_timeout(Duration::from_millis(5));
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            master,
            _slave: slave,
            slave_path,
            before,
            shared,
            reader: Some(reader),
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        if bytes == ESC {
            thread::sleep(Duration::from_millis(40));
        }
    }
    fn screen(&self) -> String {
        self.shared.0.lock().unwrap().parser.screen().contents()
    }
    #[track_caller]
    fn wait(&self, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        let mut s = self.shared.0.lock().unwrap();
        loop {
            let screen = s.parser.screen().contents();
            if predicate(&screen) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "screen condition timed out:\n{screen}"
            );
            s = self
                .shared
                .1
                .wait_timeout(s, remaining.min(Duration::from_millis(20)))
                .unwrap()
                .0;
        }
    }
    #[track_caller]
    fn wait_for(&self, text: &str) {
        self.wait(|s| flat(s).contains(&flat(text)));
    }
    fn resize(&self, cols: u16, rows: u16) {
        let mut s = self.shared.0.lock().unwrap();
        s.parser.screen_mut().set_size(rows, cols);
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
    }
    fn exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child did not exit");
            thread::park_timeout(Duration::from_millis(5));
        }
    }
    fn quit(&mut self) {
        self.send(b"q");
        assert!(self.exit().success());
        // macOS revokes the old slave descriptor when the session leader exits.
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&self.slave_path)
            .unwrap();
        assert_eq!(
            terminal_flags(&self.before),
            terminal_flags(&termios(&slave))
        );
        self.wait_raw_restored();
    }
    fn wait_raw_restored(&self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let s = self.shared.0.lock().unwrap();
            let set = s
                .raw
                .windows(5)
                .rposition(|w| w == b"\x1b]11;")
                .expect("background was set");
            if s.raw[set..]
                .windows(b"\x1b]111\x07\x1b]110\x07".len())
                .any(|w| w == b"\x1b]111\x07\x1b]110\x07")
            {
                return;
            }
            assert!(Instant::now() < deadline, "OSC colors were not reset");
            drop(s);
            thread::park_timeout(Duration::from_millis(5));
        }
    }
    fn compose(&mut self, ask: bool, text: &str) {
        self.send(ENTER);
        self.wait_for("Ask the agent");
        for _ in 0..if ask { 4 } else { 3 } {
            self.send(DOWN);
        }
        self.send(ENTER);
        self.wait_for("Enter send");
        self.send(text.as_bytes());
        self.wait_for(text);
        self.send(ENTER);
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.master.write_all(CTRL_C);
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // Release the reader's only other Arc so it drains output and stops.
        let replacement = Arc::new((
            Mutex::new(Screen {
                parser: vt100::Parser::new(1, 1, 0),
                raw: Vec::new(),
            }),
            Condvar::new(),
        ));
        drop(std::mem::replace(&mut self.shared, replacement));
        self.reader.take().unwrap().join().unwrap();
    }
}

fn top_line(screen: &str) -> String {
    let flat = flat(screen);
    let start = flat.find("합성줄").expect("top synthetic line");
    flat[start..].chars().take(9).collect()
}

#[test]
fn reader_navigation_resize_and_terminal_restoration() {
    let f = Fixture::gallery("long.md");
    let mut p = Pty::new(&f, &["view", f.doc.to_str().unwrap()], "dry.json");
    p.wait_for("T -> Tree");
    p.send(b"T");
    p.wait_for("mdv | Tree");
    p.wait_for("T -> Inbox");
    p.send(b"o");
    p.wait_for("mdv | Outline");
    p.send(b"o");
    p.wait_for("mdv | Tree");
    p.send(b"T");
    p.wait_for("mdv | Inbox +");
    p.send(b"T");
    p.wait_for("T -> Tree");
    p.send(TAB);
    p.wait(|s| s.lines().any(|l| l.starts_with("> ")));
    p.send(ENTER);
    p.wait_for("Ask the agent");
    p.send(ESC);
    p.send(b"m");
    p.wait_for("y markdown (m)");
    p.send(b"m");
    p.send("/합성\r".as_bytes());
    p.wait_for("Search 1/");
    p.send(b"n");
    p.wait_for("Search 2/");
    p.send(b"N");
    p.wait_for("Search 1/");
    p.send("ㅅ".as_bytes());
    p.wait_for("mdv | Tree");
    p.send(ESC);
    p.send(b"g");
    for _ in 0..8 {
        p.send(DOWN);
    }
    p.send(b"m");
    p.wait_for("y markdown (m)");
    let anchor = top_line(&p.screen());
    let old_header = p.screen().lines().next().unwrap().to_owned();
    p.resize(80, 40);
    p.wait(|s| s.lines().next().unwrap_or("") != old_header && s.contains("y markdown (m)"));
    assert_eq!(anchor, top_line(&p.screen()));
    p.send(b"T");
    p.wait(|s| s.contains("(Esc back)") || s.contains("mdv | Tree"));
    p.send(ESC);
    p.quit();
}

#[test]
fn folder_hostile_log_and_file_shortcut() {
    for name in ["code.md", "hostile.log"] {
        let f = Fixture::gallery(name);
        let mut p = Pty::new(&f, &[f.doc.to_str().unwrap()], "dry.json");
        p.wait_for(if name == "hostile.log" {
            "␛]52"
        } else {
            "T -> Tree"
        });
        p.quit();
    }
    let f = Fixture::gallery("code.md");
    let mut p = Pty::new(
        &f,
        &["view", f.doc.parent().unwrap().to_str().unwrap()],
        "dry.json",
    );
    p.wait_for("mdv | Tree");
    p.quit();
}

#[test]
fn composer_comment_and_live_answer() {
    let f = Fixture::gallery("headings.md");
    let mut p = Pty::new(&f, &["view", f.doc.to_str().unwrap()], "dry.json");
    p.wait_for("T -> Tree");
    p.send(TAB);
    p.wait(|s| s.lines().any(|l| l.starts_with("> ")));
    p.send(ENTER);
    p.wait_for("Ask the agent");
    for _ in 0..3 {
        p.send(DOWN);
    }
    p.send(ENTER);
    p.wait_for("Enter send");
    p.send("한글 q T draft".as_bytes());
    p.wait_for("한글 q T draft");
    p.send(ESC);
    p.wait_for("Escape again to discard");
    p.send(ESC);
    p.wait_for("T -> Tree");
    p.compose(false, "terminal comment fixture");
    p.wait_for("comment: terminal comment fixture");
    p.compose(true, "terminal dry-run question");
    p.wait_for("Not confirmed");
    assert!(f.root.join("dry.json").is_file());
    let events = f.events();
    let ask = events.iter().find(|e| e["kind"] == "ask").unwrap();
    f.answer(ask["req_id"].as_str().unwrap(), "terminal synthetic reply");
    p.wait_for("Answer ready: terminal synthetic reply");
    p.quit();
}

#[test]
fn original_quote_survives_edit_in_all_looks() {
    let source = "# 합성 검토 문서\n\n이 구현은 메모리 사용량이 줄어들기 때문에 유리하다.\n\n다음 문단은 별도 주장입니다.\n";
    let f = Fixture::new("claim.md", source);
    let mut p = Pty::new(&f, &["view", f.doc.to_str().unwrap()], "dry.json");
    p.wait_for("T -> Tree");
    p.send(TAB);
    p.send(TAB);
    p.wait_for("> 이 구현은");
    p.compose(true, "이 판단의 근거가 무엇인가요?");
    p.wait_for("Not confirmed");
    p.quit();
    drop(p);
    let events = f.events();
    let ask = events.iter().find(|e| e["kind"] == "ask").unwrap();
    assert_eq!(ask["source"], "terminal");
    assert_eq!(
        ask["quote"],
        "이 구현은 메모리 사용량이 줄어들기 때문에 유리하다.\n"
    );
    assert_eq!(ask["before"], "# 합성 검토 문서\n\n");
    assert_eq!(ask["after"], "\n다음 문단은 별도 주장입니다.\n");
    assert_eq!(
        ask["doc_hash"],
        format!("{:x}", Sha256::digest(source.as_bytes()))
    );
    assert!(
        fs::read_to_string(f.root.join("dry.json"))
            .unwrap()
            .contains("질문 당시 원문(JSON)")
    );
    f.answer(ask["req_id"].as_str().unwrap(), "terminal synthetic reply");
    let mut legacy = ask.clone();
    legacy["kind"] = "comment".into();
    legacy["req_id"] = "legacy".into();
    legacy["text"] = "이전 형식 기록입니다.".into();
    for key in ["quote", "selected", "doc_hash", "before", "after"] {
        legacy[key] = serde_json::Value::Null;
    }
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(f.root.join("threads.jsonl"))
            .unwrap(),
        "{legacy}"
    )
    .unwrap();
    fs::write(&f.doc, source.replace("메모리 사용량이", "처리 지연이")).unwrap();
    for look in ["portal", "garage"] {
        for dark in [false, true] {
            let mut args = vec!["view", f.doc.to_str().unwrap(), "--look", look];
            if dark {
                args.push("--dark");
            }
            let mut p = Pty::new(&f, &args, "unused-dry.json");
            for text in [
                "이 구현은 처리 지연이 줄어들기 때문에 유리하다.",
                "이 구현은 메모리 사용량이 줄어들기 때문에 유리하다.",
                "질문 당시 원문",
                "연결 끊김",
                "질문 당시 원문 기록 없음 (이전 기록)",
                "terminal synthetic reply",
            ] {
                p.wait_for(text);
            }
            let screen = flat(&p.screen());
            assert!(
                screen.find(&flat("다음 문단은 별도 주장입니다.")).unwrap()
                    < screen.find(&flat("질문 당시 원문")).unwrap()
            );
            p.quit();
        }
    }
    assert!(!f.root.join("unused-dry.json").exists());
}

#[test]
fn explicit_project_routes_a_document_outside_projects() {
    let mut f = Fixture::new("outside.md", "# Outside document\n");
    let outside = f.root.join("outside.md");
    fs::rename(&f.doc, &outside).unwrap();
    f.doc = outside;
    for project in [false, true] {
        let mut args = vec!["view", f.doc.to_str().unwrap()];
        if project {
            args.extend(["--project", "fixture"]);
        }
        let mut p = Pty::new(&f, &args, "project-dry.json");
        p.wait_for("T -> Tree");
        p.send(TAB);
        p.compose(
            true,
            if project {
                "explicit question"
            } else {
                "path question"
            },
        );
        p.wait_for(if project {
            "Not confirmed"
        } else {
            "No recipient"
        });
        if project {
            let dry: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(f.root.join("project-dry.json")).unwrap())
                    .unwrap();
            assert_eq!(dry[1], "fixture-main");
            let ask = f
                .events()
                .into_iter()
                .find(|e| e["text"] == "explicit question")
                .unwrap();
            assert_eq!(ask["project"], "fixture");
        } else {
            assert!(!f.root.join("project-dry.json").exists());
        }
        p.quit();
    }
}

#[test]
fn saved_question_keeps_its_project_on_retry() {
    let f = Fixture::new(
        "retry.md", "# Retry
",
    );
    fs::write(f.root.join("sessions"), "[]").unwrap();
    let mut p = Pty::new(
        &f,
        &["view", f.doc.to_str().unwrap(), "--project", "fixture"],
        "retry.json",
    );
    p.wait_for("T -> Tree");
    p.send(TAB);
    p.compose(true, "saved question");
    p.wait_for("No recipient");
    p.quit();
    drop(p);
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.root.join("session")).unwrap()).unwrap();
    fs::write(
        f.root.join("sessions"),
        serde_json::json!([session]).to_string(),
    )
    .unwrap();
    let mut p = Pty::new(
        &f,
        &["view", f.doc.to_str().unwrap(), "--project", "other"],
        "retry.json",
    );
    p.wait_for("T -> Tree");
    p.send(TAB);
    p.send(b"s");
    p.wait_for("Saved · Not confirmed");
    let dry: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.root.join("retry.json")).unwrap()).unwrap();
    assert_eq!(dry[1], "fixture-main");
    p.quit();
}

fn assert_input_cursor(p: &Pty, text: &str) {
    p.wait_for(text);
    let s = p.shared.0.lock().unwrap();
    let screen = s.parser.screen();
    let (rows, cols) = screen.size();
    let row = rows - 1;
    let mut line = String::new();
    for col in 0..cols {
        line.push_str(&screen.cell(row, col).unwrap().contents());
        if line.ends_with(text) {
            // The final Hangul syllable occupies this cell and the next one.
            assert_eq!(screen.cursor_position(), (row, col + 2));
            return;
        }
    }
    panic!("input missing from footer: {line}");
}

#[test]
fn question_and_comment_cursor_follows_hangul() {
    for ask in [true, false] {
        let f = Fixture::new("input.md", "# Input\n");
        let mut p = Pty::new(&f, &["view", f.doc.to_str().unwrap()], "dry.json");
        p.wait_for("T -> Tree");
        p.send(TAB);
        p.send(ENTER);
        p.wait_for("Ask the agent");
        for _ in 0..if ask { 4 } else { 3 } {
            p.send(DOWN);
        }
        p.send(ENTER);
        p.wait_for("Enter send");
        p.send("한글".as_bytes());
        assert_input_cursor(&p, "한글");
        p.send(format!("{}한글", "가".repeat(80)).as_bytes());
        assert_input_cursor(&p, "가한글");
        p.send(ESC);
        p.wait_for("Escape again");
        p.send(ESC);
        p.quit();
    }
}

#[test]
fn search_cursor_follows_hangul() {
    let f = Fixture::new("search.md", "# Search\n");
    let mut p = Pty::new(&f, &["view", f.doc.to_str().unwrap()], "dry.json");
    p.wait_for("T -> Tree");
    p.send("/한글".as_bytes());
    assert_input_cursor(&p, "한글");
    p.send(format!("{}한글", "가".repeat(80)).as_bytes());
    assert_input_cursor(&p, "가한글");
    p.send(ESC);
    p.quit();
}
