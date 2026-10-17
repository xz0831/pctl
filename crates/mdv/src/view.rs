use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute, queue,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use mdv::{
    Doc, Kind, Style, parse, parse_text,
    reader::{
        CopyMode, Inbox, Layout, Offer, Position, Selection, Side, State, cell_byte, command_key,
        markdown_copy, osc52,
    },
    visible,
};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub use mdv::threads::{AskHook, AskOutcome, AskRequest};

#[derive(Default)]
pub struct ViewOptions {
    pub project: Option<String>,
    pub look: Option<mdv::Look>,
    pub dark: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    SentToTerminal,
    Failed(String),
}
pub struct Hooks {
    pub copy: Box<dyn FnMut(&str) -> CopyOutcome>,
    pub ask: Box<dyn FnMut(&AskRequest) -> AskOutcome + Send>,
}
impl Default for Hooks {
    fn default() -> Self {
        Self {
            copy: Box::new(|text| {
                match io::stdout()
                    .write_all(osc52(text).as_bytes())
                    .and_then(|_| io::stdout().flush())
                {
                    Ok(()) => CopyOutcome::SentToTerminal,
                    Err(e) => CopyOutcome::Failed(e.to_string()),
                }
            }),
            ask: Box::new(mdv::threads::default_ask),
        }
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SIGNAL: AtomicI32 = AtomicI32::new(0);
#[cfg(target_os = "macos")]
const SUSPEND: i32 = 18;
#[cfg(target_os = "macos")]
const STOP: i32 = 17;
#[cfg(not(target_os = "macos"))]
const SUSPEND: i32 = 20;
#[cfg(not(target_os = "macos"))]
const STOP: i32 = 19;
unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn raise(sig: i32) -> i32;
}
extern "C" fn on_signal(sig: i32) {
    SIGNAL.store(sig, Ordering::SeqCst);
}
fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        let _ = io::stdout().write_all(b"\x1b[0m\x1b]111\x07\x1b]110\x07");
        let _ = io::stdout().flush();
    }
}
fn activate(style: &Style) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        Hide
    )?;
    write!(
        io::stdout(),
        "\x1b]11;{}\x07\x1b]10;{}\x07",
        style.paper,
        style.ink
    )?;
    io::stdout().flush()
}
struct Terminal {
    handlers: Vec<(i32, usize)>,
}
impl Terminal {
    fn enter(style: &Style) -> io::Result<Self> {
        let mut guard = Self {
            handlers: Vec::new(),
        };
        for sig in [2, 15, SUSPEND] {
            // POSIX signal handler only sets an atomic; IO happens in the event loop.
            guard
                .handlers
                .push((sig, unsafe { signal(sig, on_signal as *const () as usize) }));
        }
        activate(style)?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
        for (sig, handler) in &self.handlers {
            unsafe {
                signal(*sig, *handler);
            }
        }
    }
}
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}
fn age(ts: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let n = now.saturating_sub(ts);
    if n < 60 {
        format!("{n}s")
    } else if n < 3600 {
        format!("{}m", n / 60)
    } else if n < 86400 {
        format!("{}h", n / 3600)
    } else {
        format!("{}d", n / 86400)
    }
}
// Feed timestamps accept Unix seconds or ISO 8601 with a numeric offset.
fn epoch(value: &serde_json::Value) -> Option<u64> {
    if let Some(n) = value.as_f64() {
        return (n.is_finite() && n >= 0.0 && n < u64::MAX as f64).then_some(n.floor() as u64);
    }
    let s = value.as_str()?;
    let date_time = s.get(..19)?;
    let (date, time) = date_time.split_once('T')?;
    let date = date
        .split('-')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let time = time
        .split(':')
        .map(str::parse::<i64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let [year, month, day] = *date.as_slice() else {
        return None;
    };
    let [hour, minute, second] = *time.as_slice() else {
        return None;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || !(1..=days[(month - 1) as usize]).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    let mut zone = s.get(19..)?;
    if let Some(fraction) = zone.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        zone = &fraction[n..];
    }
    let offset = if zone == "Z" {
        0
    } else {
        let sign = match zone.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let digits = zone[1..].replace(':', "");
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let h: i64 = digits[..2].parse().ok()?;
        let m: i64 = digits[2..].parse().ok()?;
        if h >= 24 || m >= 60 {
            return None;
        }
        sign * (h * 3600 + m * 60)
    };
    // Gregorian civil date to days since 1970-01-01.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second - offset).ok()
}
fn read_feed(path: &Path) -> io::Result<Vec<Offer>> {
    let contents = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(contents
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            Some(Offer {
                id: v["id"].as_str()?.into(),
                path: v["path"].as_str()?.into(),
                agent: v["agent"].as_str().unwrap_or("unknown").into(),
                project: v["project"].as_str().unwrap_or_default().into(),
                title: v["title"].as_str().unwrap_or_default().into(),
                ts: epoch(&v["ts"]).unwrap_or(0),
                repeats: 1,
            })
        })
        .collect())
}
fn converter() -> PathBuf {
    env::var_os("MDV_CONVERTER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            use std::os::unix::fs::PermissionsExt;
            let found = env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join("unpress"))
                .find(|p| {
                    p.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                });
            found.unwrap_or_else(|| PathBuf::from("docconv"))
        })
}
struct Conversion {
    child: Child,
    folder: PathBuf,
    anchor: Option<(u64, Position)>,
    fresh: bool,
}
impl Drop for Conversion {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
/// Map physical source lines to rendered blocks; blank edits attach to the next block.
fn changed_blocks(src: &str, doc: &Doc, changes: &[mdv::lens::Change]) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.match_indices('\n')
            .map(|(at, _)| at + 1)
            .filter(|at| *at < src.len()),
    );
    let mut blocks = Vec::new();
    for change in changes {
        let start = starts.get(change.new.start).copied().unwrap_or(src.len());
        let end = starts.get(change.new.end).copied().unwrap_or(src.len());
        let before = blocks.len();
        if !change.new.is_empty() {
            blocks.extend(
                doc.blocks
                    .iter()
                    .filter(|b| {
                        b.kind != Kind::Definition && b.range.start < end && b.range.end > start
                    })
                    .map(|b| b.id),
            );
        }
        if blocks.len() == before {
            // At EOF there is no following block; keep the deletion visible on the last one.
            if let Some(b) = doc
                .blocks
                .iter()
                .filter(|b| b.kind != Kind::Definition)
                .find(|b| b.range.end > start)
                .or_else(|| doc.blocks.iter().rev().find(|b| b.kind != Kind::Definition))
            {
                blocks.push(b.id);
            }
        }
    }
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}
struct App {
    copy_hook: Box<dyn FnMut(&str) -> CopyOutcome>,
    ask_hook: Arc<Mutex<AskHook>>,
    state: State,
    doc: Doc,
    src: String,
    layout: Layout,
    path: PathBuf,
    project: Option<String>,
    tree: PathBuf,
    entries: Vec<PathBuf>,
    inbox: Inbox,
    feed: PathBuf,
    feed_stamp: Option<(SystemTime, u64)>,
    file_stamp: Option<(SystemTime, u64)>,
    style: Style,
    size: (u16, u16),
    search: Option<String>,
    query: String,
    matches: Vec<Position>,
    match_at: usize,
    menu: Option<usize>,
    composer: Option<(String, String, bool)>,
    threads: Vec<mdv::threads::Event>,
    thread_stamp: Option<(SystemTime, u64)>,
    pending_thread: Option<std::sync::mpsc::Receiver<Result<&'static str, String>>>,
    message: String,
    seen_store: Option<PathBuf>,
    changes: Vec<mdv::lens::Change>,
    changed_blocks: Vec<usize>,
    lens_warning: Option<String>,
    converted: bool,
    content_available: bool,
    conversion: Option<Conversion>,
    horizontal: usize,
    host: String,
}
impl App {
    fn new(path: &Path, hooks: Hooks) -> io::Result<Self> {
        let style = Style::default();
        let size = terminal::size()?;
        let home =
            PathBuf::from(env::var_os("HOME").ok_or_else(|| io::Error::other("HOME missing"))?);
        let host = env::var("HOSTNAME")
            .ok()
            .or_else(|| fs::read_to_string("/etc/hostname").ok())
            .unwrap_or_default()
            .trim()
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned();
        let empty = parse("");
        let layout = Layout::new(&empty, "", size.0 as usize, &style);
        let absolute = fs::canonicalize(path)?;
        let tree = if absolute.is_dir() {
            absolute.clone()
        } else {
            absolute.parent().unwrap_or(Path::new("/")).into()
        };
        let mut app = Self {
            copy_hook: hooks.copy,
            ask_hook: Arc::new(Mutex::new(hooks.ask)),
            state: State::default(),
            doc: empty,
            src: String::new(),
            layout,
            path: absolute.clone(),
            project: None,
            tree,
            entries: Vec::new(),
            inbox: Inbox::default(),
            feed: home.join(".local/state/mdv/feed.jsonl"),
            feed_stamp: None,
            file_stamp: None,
            style,
            size,
            search: None,
            query: String::new(),
            matches: Vec::new(),
            match_at: 0,
            menu: None,
            composer: None,
            threads: vec![],
            thread_stamp: None,
            pending_thread: None,
            message: String::new(),
            seen_store: mdv::lens::default_store(),
            changes: Vec::new(),
            changed_blocks: Vec::new(),
            lens_warning: None,
            converted: false,
            content_available: false,
            conversion: None,
            horizontal: 0,
            host,
        };
        app.list_tree()?;
        app.poll_feed();
        if absolute.is_dir() {
            app.state.side = Side::Tree;
            app.state.side_focus = true;
            app.show_text("Choose a file in Tree. Enter opens; Esc returns.");
        } else {
            app.open(&absolute, true)?;
        }
        Ok(app)
    }
    fn save_seen(&mut self, path: &Path, src: &str) {
        if let Some(store) = &self.seen_store
            && let Err(error) = mdv::lens::mark_seen(store, path, src)
        {
            self.lens_warning = Some(format!("지난번 본 내용 저장 실패: {error}"));
        }
    }
    fn leave_document(&mut self) {
        if self.content_available && mdv::lens::is_watched(&self.path) {
            self.save_seen(&self.path.clone(), &self.src.clone());
        }
    }
    fn update_changes(&mut self) {
        self.changes = self
            .seen_store
            .as_ref()
            .and_then(|store| mdv::lens::changes(store, &self.path, &self.src))
            .unwrap_or_default();
        self.changed_blocks = changed_blocks(&self.src, &self.doc, &self.changes);
    }
    fn next_change(&mut self) {
        let current = self
            .state
            .focus
            .or_else(|| self.layout.anchor(self.state.top).map(|p| p.block));
        if let Some(&block) = self
            .changed_blocks
            .iter()
            .find(|&&id| current.is_none_or(|at| id > at))
            .or_else(|| self.changed_blocks.first())
        {
            self.state.focus = Some(block);
            self.state.side_focus = false;
            self.state.top = self
                .layout
                .row_for(Position { block, byte: 0 })
                .min(self.max_top());
        }
    }
    fn side_visible(&self) -> bool {
        self.state.outline || self.state.side != Side::Off
    }
    fn overlay(&self) -> bool {
        self.side_visible() && self.size.0 < 80
    }
    fn side_width(&self) -> usize {
        if self.overlay() {
            self.size.0 as usize
        } else {
            (self.size.0 as usize / 4).clamp(24, 32)
        }
    }
    fn doc_x(&self) -> usize {
        if self.side_visible() && !self.overlay() {
            self.side_width() + self.style.separator.len()
        } else {
            0
        }
    }
    fn width(&self) -> usize {
        (self.size.0 as usize)
            .saturating_sub(self.doc_x() + 2)
            .max(1)
    }
    fn height(&self) -> usize {
        self.size.1.saturating_sub(3).max(1) as usize
    }
    fn max_top(&self) -> usize {
        self.layout.rows.len().saturating_sub(self.height())
    }
    fn rewrap(&mut self) {
        let row = self.layout.rows.get(self.state.top).cloned();
        let anchor = self.layout.anchor(self.state.top);
        self.layout = Layout::new(&self.doc, &self.src, self.width(), &self.style);
        self.insert_threads();
        if let Some(row) = row {
            let exact = self.layout.rows.iter().position(|r| {
                if row.discussion {
                    r.thread == row.thread
                } else {
                    !r.discussion
                        && r.line.block_id == row.line.block_id
                        && r.range == row.range
                        && r.line.plain == row.line.plain
                }
            });
            if let Some(top) = exact.or_else(|| anchor.map(|p| self.layout.row_for(p))) {
                self.state.top = top.min(self.max_top());
            }
        }
    }
    fn insert_threads(&mut self) {
        let mut rows = Vec::new();
        let original = std::mem::take(&mut self.layout.rows);
        let append = |rows: &mut Vec<mdv::reader::Row>,
                      e: &mdv::threads::Event,
                      event: usize,
                      block_id: usize,
                      changed: bool| {
            for (offset, mut line) in mdv::threads::lines(e, changed, self.width(), &self.style)
                .into_iter()
                .enumerate()
            {
                line.block_id = block_id;
                rows.push(mdv::reader::Row {
                    line,
                    range: 0..0,
                    discussion: true,
                    thread: Some((event, offset)),
                });
            }
        };
        for (i, row) in original.iter().enumerate() {
            rows.push(row.clone());
            if original
                .get(i + 1)
                .is_some_and(|next| next.line.block_id == row.line.block_id)
            {
                continue;
            }
            for (event, e) in self.threads.iter().enumerate().filter(|(_, e)| {
                e.path == self.path.to_string_lossy()
                    && mdv::threads::target(e, &self.doc) == Some(row.line.block_id)
            }) {
                append(&mut rows, e, event, row.line.block_id, false);
            }
        }
        // Detached history survives even if the entire current document is empty.
        for (event, e) in self.threads.iter().enumerate().filter(|(_, e)| {
            e.path == self.path.to_string_lossy() && mdv::threads::target(e, &self.doc).is_none()
        }) {
            append(
                &mut rows,
                e,
                event,
                self.doc.blocks.last().map_or(0, |b| b.id),
                true,
            );
        }
        self.layout.rows = rows;
    }
    fn poll_threads(&mut self) -> bool {
        if let Some(receiver) = &self.pending_thread
            && let Ok(result) = receiver.try_recv()
        {
            self.message = result
                .map(|state| format!("Saved · {state}"))
                .unwrap_or_else(|e| format!("Not confirmed: {e}"));
            self.pending_thread = None;
        }
        let store = mdv::threads::store_path();
        let observed = stamp(&store);
        if observed == self.thread_stamp {
            return false;
        }
        match mdv::threads::read(&store) {
            Ok(events) if events != self.threads => {
                self.thread_stamp = observed;
                if let Some(warning) = mdv::threads::warning(&store) {
                    self.message = warning;
                }
                self.threads = events;
                self.rewrap();
                true
            }
            Err(error) => {
                self.thread_stamp = observed;
                self.message = format!("Thread store: {error}");
                false
            }
            Ok(_) => {
                if let Some(warning) = mdv::threads::warning(&store) {
                    self.message = warning;
                }
                self.thread_stamp = observed;
                false
            }
        }
    }
    fn send_thread(&mut self, kind: String, text: String) -> io::Result<()> {
        let Some(block) = self.state.focus.and_then(|i| self.doc.blocks.get(i)) else {
            return Ok(());
        };
        let current = match fs::read_to_string(&self.path) {
            Ok(src) => src,
            Err(e) => {
                self.message = format!("Cannot send: {e}");
                self.composer = Some((kind, text, false));
                return Ok(());
            }
        };
        if current != self.src {
            self.message = "Original passage moved or changed; refresh before sending".into();
            self.composer = Some((kind, text, false));
            return Ok(());
        }
        let path = self.path.to_string_lossy().into_owned();
        let mut event = mdv::threads::Event {
            kind,
            req_id: mdv::threads::new_id(),
            project: Some(self.project()),
            feed_id: mdv::threads::feed(&path).and_then(|v| v["id"].as_str().map(String::from)),
            path,
            block_id: block.id,
            range: [block.range.start, block.range.end],
            fingerprint: format!("{:016x}", block.fingerprint),
            text,
            source: "terminal".into(),
            time: 0,
            agent: None,
            quote: None,
            before: None,
            after: None,
            doc_hash: None,
            selected: None,
        };
        if let Some(selection) = self.state.selection {
            let (a, b) = selection.ordered();
            if a.block <= block.id && b.block >= block.id && a != b {
                let within = Selection {
                    start: Position {
                        block: block.id,
                        byte: if a.block == block.id { a.byte } else { 0 },
                    },
                    end: Position {
                        block: block.id,
                        byte: if b.block == block.id {
                            b.byte
                        } else {
                            self.layout.texts[block.id].len()
                        },
                    },
                };
                event.selected = Some(self.layout.text_copy(within));
            }
        }
        mdv::threads::capture(&mut event, &current, &self.doc).map_err(io::Error::other)?;
        let binary = env::current_exe()?;
        let (sender, receiver) = std::sync::mpsc::channel();
        let ask = Arc::clone(&self.ask_hook);
        std::thread::spawn(move || {
            let result = ask
                .lock()
                .map_err(|e| e.to_string())
                .and_then(|mut hook| mdv::threads::submit_with_hook(event, &binary, &mut **hook));
            let _ = sender.send(result);
        });
        self.pending_thread = Some(receiver);
        self.message = "Saving".into();
        Ok(())
    }
    fn project(&self) -> String {
        self.project
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(String::from)
            .unwrap_or_else(|| mdv::threads::project(&self.path.to_string_lossy()))
    }
    fn focus_agent(&mut self) {
        self.message =
            mdv::threads::focus_agent(&self.path.to_string_lossy(), self.project.as_deref())
                .unwrap_or_else(|error| error);
        self.menu = None;
    }
    fn retry_question(&mut self) -> io::Result<()> {
        self.menu = None;
        if self.pending_thread.is_some() {
            return Ok(());
        }
        let events = mdv::threads::read(&mdv::threads::store_path())?;
        let event = events
            .iter()
            .rev()
            .find(|e| {
                e.kind == "ask"
                    && Path::new(&e.path) == self.path
                    && mdv::threads::target(e, &self.doc) == self.state.focus
                    && mdv::threads::retryable(&events, &e.req_id)
            })
            .cloned();
        if let Some(event) = event {
            let binary = env::current_exe()?;
            let (sender, receiver) = std::sync::mpsc::channel();
            let ask = Arc::clone(&self.ask_hook);
            std::thread::spawn(move || {
                let result = ask.lock().map_err(|e| e.to_string()).and_then(|mut hook| {
                    mdv::threads::submit_with_hook(event, &binary, &mut **hook)
                });
                let _ = sender.send(result);
            });
            self.pending_thread = Some(receiver);
            self.message = "Retrying saved question".into();
        } else {
            self.message = "No saved question to retry in this block".into();
        }
        Ok(())
    }
    fn show_text(&mut self, text: &str) {
        self.content_available = false;
        self.src = text.into();
        self.doc = parse_text(text);
        self.layout = Layout::new(&self.doc, &self.src, self.width(), &self.style);
    }
    fn list_tree(&mut self) -> io::Result<()> {
        let mut entries = fs::read_dir(&self.tree)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                !p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with('.')
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            b.is_dir()
                .cmp(&a.is_dir())
                .then(a.file_name().cmp(&b.file_name()))
        });
        self.entries = entries;
        self.state.tree_cursor = self
            .state
            .tree_cursor
            .min(self.entries.len().saturating_sub(1));
        Ok(())
    }
    fn reading_anchor(&self) -> Option<(u64, Position)> {
        let pos = self.layout.anchor(self.state.top)?;
        Some((self.doc.blocks[pos.block].fingerprint, pos))
    }
    fn install(
        &mut self,
        src: String,
        markdown: bool,
        anchor: Option<(u64, Position)>,
        fresh: bool,
    ) {
        self.content_available = true;
        self.src = src;
        self.doc = if markdown {
            parse(&self.src)
        } else {
            parse_text(&self.src)
        };
        self.layout = Layout::new(&self.doc, &self.src, self.width(), &self.style);
        if fresh {
            self.state.new_doc();
            self.query.clear();
            self.matches.clear();
            self.horizontal = 0;
        } else {
            self.state.selection = None;
            self.state.updated = false;
            if let Some((hash, p)) = anchor {
                let id = self
                    .doc
                    .blocks
                    .iter()
                    .position(|b| b.fingerprint == hash)
                    .unwrap_or_else(|| p.block.min(self.doc.blocks.len().saturating_sub(1)));
                self.state.top = self
                    .layout
                    .row_for(Position {
                        block: id,
                        byte: p.byte,
                    })
                    .min(self.max_top());
                self.state.focus = Some(id).filter(|i| {
                    self.doc
                        .blocks
                        .get(*i)
                        .is_some_and(|b| b.kind != Kind::Definition)
                });
            }
            self.message = "Reloaded in place; selection cleared".into();
            self.find();
        }
    }
    fn open(&mut self, path: &Path, fresh: bool) -> io::Result<()> {
        let path = fs::canonicalize(path)?;
        if path.is_dir() {
            self.tree = path;
            self.state.tree_cursor = 0;
            self.state.tree_scroll = 0;
            return self.list_tree();
        }
        let leaving =
            (path != self.path && self.content_available && mdv::lens::is_watched(&self.path))
                .then(|| (self.path.clone(), self.src.clone()));
        let anchor = if fresh { None } else { self.reading_anchor() };
        self.conversion = None;

        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        match ext.as_str() {
            "docx" | "xlsx" | "pptx" | "potx" | "pdf" => {
                let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
                use sha2::{Digest, Sha256};
                let mut hash = Sha256::new();
                io::copy(&mut fs::File::open(&path)?, &mut hash)?;
                let folder = home
                    .join(".cache/mdv/conv")
                    .join(format!("{:x}", hash.finalize()));
                fs::create_dir_all(&folder)?;
                let converter = converter();
                let log = fs::File::create(folder.join("conversion.log"))?;
                let child = Command::new(converter)
                    .arg("convert")
                    .arg(&path)
                    .arg("-o")
                    .arg(&folder)
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(log.try_clone()?))
                    .stderr(Stdio::from(log))
                    .spawn()?;
                self.show_text("Converting document... You can scroll, navigate or quit.");
                if fresh {
                    self.state.new_doc();
                }
                self.conversion = Some(Conversion {
                    child,
                    folder,
                    anchor,
                    fresh,
                });
                self.message = "Converting".into();
            }
            "md" | "markdown" | "txt" | "log" | "rs" | "py" | "sh" | "bash" | "zsh" | "js"
            | "ts" | "tsx" | "jsx" | "json" | "jsonl" | "toml" | "yaml" | "yml" | "csv" | "tsv"
            | "c" | "h" | "cpp" | "css" | "html" | "svg" | "xml" | "sql" | "java" | "go"
            | "swift" | "" => {
                let bytes = fs::read(&path)?;
                let src = String::from_utf8_lossy(&bytes).into_owned();
                self.install(
                    src,
                    matches!(ext.as_str(), "md" | "markdown"),
                    anchor,
                    fresh,
                );
                if fresh {
                    self.message.clear();
                }
            }
            _ => {
                self.show_text(&format!("Not shown here\n{}", path.display()));
                if fresh {
                    self.state.new_doc();
                }
                self.message = "Not shown here".into();
            }
        }
        if let Some((old_path, old_src)) = leaving {
            self.save_seen(&old_path, &old_src);
        }
        self.path = path.clone();
        self.update_changes();
        self.thread_stamp = None;
        self.threads = match mdv::threads::read(&mdv::threads::store_path()) {
            Ok(events) => events,
            Err(error) => {
                self.message = format!("Thread store: {error}");
                Vec::new()
            }
        };
        self.rewrap();
        self.composer = None;
        self.converted = false;
        self.file_stamp = stamp(&path);
        self.menu = None;
        if fresh {
            self.state.side_focus = false;
        }
        Ok(())
    }
    fn poll_feed(&mut self) {
        let current = stamp(&self.feed);
        if current == self.feed_stamp && self.inbox.entries.is_empty() {
            return;
        }
        if current == self.feed_stamp {
            return;
        }
        let cursor_id = self
            .inbox
            .entries
            .get(self.state.inbox_cursor)
            .map(|o| o.id.clone());
        match read_feed(&self.feed) {
            Ok(offers) => {
                self.inbox.update(offers);
                self.feed_stamp = current;
                if let Some(id) = cursor_id {
                    self.state.inbox_cursor = self
                        .inbox
                        .entries
                        .iter()
                        .position(|o| o.id == id)
                        .unwrap_or(0);
                }
            }
            Err(e) => self.message = visible(&e.to_string()),
        }
    }
    fn tick(&mut self) -> bool {
        let before = (
            self.feed_stamp,
            self.file_stamp,
            self.message.clone(),
            self.conversion.is_some(),
        );
        self.poll_feed();
        let threads_changed = self.poll_threads();
        if let Some(mut conv) = self.conversion.take() {
            match conv.child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        let stem = self.path.file_name().unwrap_or_default().to_string_lossy();
                        let md = conv.folder.join(format!("{stem}.md"));
                        match fs::read_to_string(md) {
                            Ok(src) => {
                                self.install(src, true, conv.anchor, conv.fresh);
                                self.converted = true;
                                self.message = "Converted view".into();
                            }
                            Err(e) => {
                                self.message = format!(
                                    "Conversion output unavailable: {}",
                                    visible(&e.to_string())
                                )
                            }
                        }
                    } else {
                        self.message = format!(
                            "Conversion failed ({status}); see {}",
                            conv.folder.join("conversion.log").display()
                        );
                    }
                }
                Ok(None) => self.conversion = Some(conv),
                Err(e) => self.message = format!("Conversion failed: {e}"),
            }
        }
        let current = stamp(&self.path);
        if current != self.file_stamp && !self.path.as_os_str().is_empty() {
            self.file_stamp = current;
            if current.is_none() {
                self.state.updated = true;
                self.message = "File unavailable · r to retry".into();
            } else if self.composer.is_some() {
                self.state.updated = true;
                self.message =
                    "File updated; draft retained · cancel and refresh before sending".into();
            } else if self.state.file_changed() {
                let p = self.path.clone();
                if let Err(e) = self.open(&p, false) {
                    self.message = format!("Reload failed: {e}");
                }
            } else {
                self.message = "File updated · r to refresh".into();
            }
        }
        threads_changed
            || before
                != (
                    self.feed_stamp,
                    self.file_stamp,
                    self.message.clone(),
                    self.conversion.is_some(),
                )
    }
    fn find(&mut self) {
        self.matches.clear();
        if self.query.is_empty() {
            return;
        }
        for (id, text) in self.layout.texts.iter().enumerate() {
            for (byte, _) in text.match_indices(&self.query) {
                self.matches.push(Position { block: id, byte });
            }
        }
        self.match_at = 0;
    }
    fn jump_match(&mut self, forward: bool, first: bool) {
        if self.matches.is_empty() {
            self.message = "No matches".into();
            return;
        }
        if !first {
            self.match_at = if forward {
                (self.match_at + 1) % self.matches.len()
            } else {
                (self.match_at + self.matches.len() - 1) % self.matches.len()
            };
        }
        let pos = self.matches[self.match_at];
        self.state.top = self.layout.row_for(pos).min(self.max_top());
        self.state.focus = Some(pos.block);
        self.message = format!(
            "Search {}/{}: {}",
            self.match_at + 1,
            self.matches.len(),
            visible(&self.query)
        );
    }
    fn copy(&mut self, link: bool) -> io::Result<()> {
        if !link && !self.content_available {
            self.message = "No document text available".into();
            return Ok(());
        }
        let selection = self
            .state
            .selection
            .filter(|s| s.start != s.end)
            .or_else(|| {
                self.state.focus.and_then(|id| {
                    self.layout.texts.get(id).map(|text| Selection {
                        start: Position { block: id, byte: 0 },
                        end: Position {
                            block: id,
                            byte: text.len(),
                        },
                    })
                })
            });
        let text = if link {
            self.inbox
                .entries
                .iter()
                .find(|o| Path::new(&o.path) == self.path)
                .map(|o| format!("/d/{}", o.id))
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
        } else if let Some(s) = selection {
            match self.state.copy_mode {
                CopyMode::Text => self.layout.text_copy(s),
                CopyMode::Markdown => markdown_copy(&self.doc, &self.src, s),
            }
        } else {
            self.message = "Focus a block with Tab or select text".into();
            return Ok(());
        };
        self.message = match (self.copy_hook)(&text) {
            CopyOutcome::Copied => "Copied".into(),
            CopyOutcome::Failed(reason) => format!("Copy failed: {reason}"),
            CopyOutcome::SentToTerminal => {
                if link {
                    "Link sent via OSC 52 (paste to verify)".into()
                } else if self.state.copy_mode == CopyMode::Markdown
                    && self.state.selection.is_some()
                {
                    "Containing block's Markdown sent via OSC 52 (paste to verify)".into()
                } else {
                    "Copy sent via OSC 52 (paste to verify)".into()
                }
            }
        };
        Ok(())
    }
    fn headings(&self) -> Vec<usize> {
        self.doc
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::Heading { .. }))
            .map(|b| b.id)
            .collect()
    }
    fn side_len(&self) -> usize {
        if self.state.outline {
            self.headings().len()
        } else {
            match self.state.side {
                Side::Tree => self.entries.len(),
                Side::Inbox => self.inbox.entries.len(),
                Side::Off => 0,
            }
        }
    }
    fn side_cursor(&self) -> usize {
        if self.state.outline {
            self.state.outline_cursor
        } else {
            match self.state.side {
                Side::Tree => self.state.tree_cursor,
                Side::Inbox => self.state.inbox_cursor,
                Side::Off => 0,
            }
        }
    }
    fn set_side_cursor(&mut self, n: usize) {
        if self.state.outline {
            self.state.outline_cursor = n
        } else {
            match self.state.side {
                Side::Tree => self.state.tree_cursor = n,
                Side::Inbox => self.state.inbox_cursor = n,
                Side::Off => {}
            }
        }
    }
    fn side_row_height(&self) -> usize {
        if self.state.side == Side::Inbox && !self.state.outline {
            3
        } else {
            1
        }
    }
    fn side_scroll(&self) -> usize {
        if self.state.outline {
            self.state.outline_cursor.saturating_sub(self.height() - 1)
        } else {
            match self.state.side {
                Side::Tree => self.state.tree_scroll,
                Side::Inbox => self.state.inbox_scroll,
                Side::Off => 0,
            }
        }
    }
    fn normalize_side_scroll(&mut self) {
        let height = (self.height() / self.side_row_height()).max(1);
        let cursor = self.side_cursor();
        let scroll = if self.state.outline {
            None
        } else {
            match self.state.side {
                Side::Tree => Some(&mut self.state.tree_scroll),
                Side::Inbox => Some(&mut self.state.inbox_scroll),
                Side::Off => None,
            }
        };
        if let Some(scroll) = scroll {
            if cursor < *scroll {
                *scroll = cursor;
            }
            if cursor >= *scroll + height {
                *scroll = cursor + 1 - height;
            }
        }
    }
    fn side_enter(&mut self) -> io::Result<()> {
        if self.state.outline {
            if let Some(id) = self.headings().get(self.state.outline_cursor) {
                self.state.top = self
                    .layout
                    .row_for(Position {
                        block: *id,
                        byte: 0,
                    })
                    .min(self.max_top());
                self.state.focus = Some(*id);
                self.state.side_focus = false;
                if self.overlay() {
                    self.state.outline = false;
                    self.rewrap();
                }
            }
        } else {
            match self.state.side {
                Side::Tree => {
                    if let Some(p) = self.entries.get(self.state.tree_cursor).cloned() {
                        self.open(&p, true)?;
                        if !p.is_dir() && self.overlay() {
                            self.state.side = Side::Off;
                            self.rewrap();
                        }
                    }
                }
                Side::Inbox => {
                    if let Some(p) = self
                        .inbox
                        .entries
                        .get(self.state.inbox_cursor)
                        .map(|o| PathBuf::from(&o.path))
                    {
                        self.open(&p, true)?;
                        self.inbox.arrivals = 0;
                        if self.overlay() {
                            self.state.side = Side::Off;
                            self.rewrap();
                        }
                    }
                }
                Side::Off => {}
            }
        }
        Ok(())
    }
    fn key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Ok(true),
                KeyCode::Char('z') => {
                    SIGNAL.store(SUSPEND, Ordering::SeqCst);
                    return Ok(false);
                }
                _ => {}
            }
        }
        if let Some((kind, mut text, mut cancel)) = self.composer.take() {
            match key.code {
                KeyCode::Esc if text.is_empty() || cancel => return Ok(false),
                KeyCode::Esc => {
                    cancel = true;
                    self.message = "Escape again to discard draft; any other key keeps it".into();
                }
                KeyCode::Enter if !text.trim().is_empty() => {
                    self.send_thread(kind, text)?;
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    text.pop();
                    cancel = false;
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && text.chars().count() < 2000 =>
                {
                    text.push(c);
                    cancel = false;
                }
                _ => {
                    cancel = false;
                }
            }
            self.composer = Some((kind, text, cancel));
            return Ok(false);
        }
        if self.search.is_some() {
            match key.code {
                KeyCode::Esc => self.search = None,
                KeyCode::Enter => {
                    self.query = self.search.take().unwrap_or_default();
                    self.find();
                    self.jump_match(true, true);
                }
                KeyCode::Backspace => {
                    if let Some(s) = self.search.as_mut()
                        && let Some((i, _)) = s.grapheme_indices(true).next_back()
                    {
                        s.truncate(i);
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search.as_mut().unwrap().push(c);
                }
                _ => {}
            }
            return Ok(false);
        }
        let code = match key.code {
            KeyCode::Char(c) => {
                KeyCode::Char(command_key(c, key.modifiers.contains(KeyModifiers::SHIFT)))
            }
            other => other,
        };
        if let Some(at) = self.menu {
            match code {
                KeyCode::Esc => self.menu = None,
                KeyCode::Up | KeyCode::Char('k') => self.menu = Some(at.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => self.menu = Some((at + 1).min(6)),
                KeyCode::Char('a' | 'A') => self.focus_agent(),
                KeyCode::Char('s' | 'S') => self.retry_question()?,
                KeyCode::Char('m') | KeyCode::Char(' ') => self.state.toggle_copy(),
                KeyCode::Enter => match at {
                    5 => self.focus_agent(),
                    6 => self.retry_question()?,
                    0 => {
                        self.copy(false)?;
                        self.menu = None;
                    }
                    1 => self.state.toggle_copy(),
                    2 => {
                        self.copy(true)?;
                        self.menu = None;
                    }
                    _ => {
                        if self.pending_thread.is_some() {
                            self.message = "Saving; please wait".into();
                        // shortcut: converted blocks lack original byte maps, enable when docconv supplies ranges.
                        } else if self.converted || !self.content_available {
                            self.message = "Ask/Comment requires source-addressed text".into();
                        } else {
                            self.composer = Some((
                                if at == 3 { "comment" } else { "ask" }.into(),
                                String::new(),
                                false,
                            ));
                            self.menu = None;
                        }
                    }
                },
                _ => {}
            }
            return Ok(false);
        }
        match code {
            KeyCode::Char('d' | 'D') => self.next_change(),
            KeyCode::Char('a' | 'A') => self.focus_agent(),
            KeyCode::Char('s' | 'S') => self.retry_question()?,
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('T' | 't') => {
                self.state.cycle();
                self.rewrap();
            }
            KeyCode::Char('o' | 'O') => {
                self.state.outline();
                self.rewrap();
            }
            KeyCode::Esc => {
                if self.side_visible() {
                    self.state.outline = false;
                    self.state.side = Side::Off;
                    self.state.side_focus = false;
                    self.rewrap();
                } else {
                    self.state.selection = None;
                }
            }
            KeyCode::Char('p' | 'P') => {
                self.state.pinned = !self.state.pinned;
                self.message = if self.state.pinned {
                    "Pinned".into()
                } else {
                    "Unpinned".into()
                };
                if !self.state.pinned && self.state.updated {
                    let p = self.path.clone();
                    self.open(&p, false)?;
                }
            }
            KeyCode::Char('r' | 'R') => {
                let p = self.path.clone();
                self.open(&p, false)?;
            }
            KeyCode::Char('m' | 'M') => self.state.toggle_copy(),
            KeyCode::Char('y' | 'Y') => self.copy(false)?,
            KeyCode::Char('/') => self.search = Some(String::new()),
            KeyCode::Char('n') => self.jump_match(true, false),
            KeyCode::Char('N') => self.jump_match(false, false),
            KeyCode::Tab | KeyCode::BackTab if self.content_available => {
                self.state.side_focus = false;
                self.state.move_focus(&self.doc, code == KeyCode::Tab);
                if let Some(id) = self.state.focus {
                    let row = self.layout.row_for(Position { block: id, byte: 0 });
                    if row < self.state.top || row >= self.state.top + self.height() {
                        self.state.top = row.min(self.max_top());
                    }
                }
            }
            KeyCode::Char('f' | 'F') if self.side_visible() => {
                self.state.side_focus = !self.state.side_focus
            }
            _ => {
                if self.side_visible() && (self.state.side_focus || self.overlay()) {
                    let cursor = self.side_cursor();
                    let last = self.side_len().saturating_sub(1);
                    match code {
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.set_side_cursor((cursor + 1).min(last))
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.set_side_cursor(cursor.saturating_sub(1))
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            self.set_side_cursor((cursor + self.height()).min(last))
                        }
                        KeyCode::PageUp => {
                            self.set_side_cursor(cursor.saturating_sub(self.height()))
                        }
                        KeyCode::Char('g') => self.set_side_cursor(0),
                        KeyCode::Char('G') => self.set_side_cursor(last),
                        KeyCode::Enter => self.side_enter()?,
                        KeyCode::Backspace | KeyCode::Char('h')
                            if self.state.side == Side::Tree && !self.state.outline =>
                        {
                            if let Some(parent) = self.tree.parent() {
                                self.tree = parent.into();
                                self.state.tree_cursor = 0;
                                self.state.tree_scroll = 0;
                                self.list_tree()?;
                            }
                        }
                        _ => {}
                    }
                    self.normalize_side_scroll();
                } else {
                    match code {
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.state.top = (self.state.top + 1).min(self.max_top())
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.state.top = self.state.top.saturating_sub(1)
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            self.state.top = (self.state.top + self.height()).min(self.max_top())
                        }
                        KeyCode::PageUp => {
                            self.state.top = self.state.top.saturating_sub(self.height())
                        }
                        KeyCode::Char('g') => self.state.top = 0,
                        KeyCode::Char('G') => self.state.top = self.max_top(),
                        KeyCode::Right => self.horizontal += 8,
                        KeyCode::Left => self.horizontal = self.horizontal.saturating_sub(8),
                        KeyCode::Enter if self.content_available && self.state.focus.is_some() => {
                            self.menu = Some(0);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(false)
    }
    fn mouse(&mut self, mouse: MouseEvent) -> io::Result<()> {
        if self.composer.is_some() {
            return Ok(());
        }
        let x = mouse.column as usize;
        let y = mouse.row as usize;
        if y == 0 || y > self.height() {
            return Ok(());
        }
        if self.menu.is_some() {
            return Ok(());
        }
        if self.side_visible() && (self.overlay() || x < self.side_width()) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.state.side_focus = true;
                    self.set_side_cursor(
                        (self.side_scroll() + (y - 1) / self.side_row_height())
                            .min(self.side_len().saturating_sub(1)),
                    );
                    self.side_enter()?;
                }
                MouseEventKind::ScrollDown => self.set_side_cursor(
                    (self.side_cursor() + 3).min(self.side_len().saturating_sub(1)),
                ),
                MouseEventKind::ScrollUp => {
                    self.set_side_cursor(self.side_cursor().saturating_sub(3))
                }
                _ => {}
            }
            self.normalize_side_scroll();
            return Ok(());
        }
        let row = self.state.top + y - 1;
        match mouse.kind {
            MouseEventKind::ScrollDown => self.state.top = (self.state.top + 3).min(self.max_top()),
            MouseEventKind::ScrollUp => self.state.top = self.state.top.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.state.side_focus = false;
                if let Some(r) = self.layout.rows.get(row).filter(|r| !r.discussion) {
                    self.state.focus = Some(r.line.block_id);
                }
                if x < self.doc_x() + 2 {
                    self.state.selection = None;
                } else if let Some(p) = self.mouse_point(row, x) {
                    self.state.selection = Some(Selection { start: p, end: p });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                if let Some(p) = self.mouse_point(row, x)
                    && let Some(s) = self.state.selection.as_mut()
                {
                    s.end = p;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn mouse_point(&self, row: usize, x: usize) -> Option<Position> {
        let r = self.layout.rows.get(row)?;
        self.layout.point(
            row,
            x.saturating_sub(self.doc_x() + 2)
                + if r.line.horizontally_scrollable {
                    self.horizontal
                } else {
                    0
                },
        )
    }
    fn side_label(&self) -> String {
        if self.state.outline {
            "Outline".into()
        } else {
            match self.state.side {
                Side::Tree => "Tree".into(),
                Side::Inbox => format!("Inbox +{}", self.inbox.arrivals),
                Side::Off => String::new(),
            }
        }
    }
    fn side_rows(&self) -> Vec<String> {
        if self.state.outline {
            self.headings()
                .iter()
                .map(|id| {
                    self.doc.blocks[*id]
                        .heading_path
                        .last()
                        .cloned()
                        .unwrap_or_default()
                })
                .collect()
        } else {
            match self.state.side {
                Side::Tree => self
                    .entries
                    .iter()
                    .map(|p| {
                        format!(
                            "{}{}",
                            p.file_name().unwrap_or_default().to_string_lossy(),
                            if p.is_dir() { "/" } else { "" }
                        )
                    })
                    .collect(),
                Side::Inbox => self
                    .inbox
                    .entries
                    .iter()
                    .flat_map(|o| {
                        let title = if o.title.is_empty() {
                            Path::new(&o.path)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            o.title.clone()
                        };
                        vec![
                            format!("{} | {}", o.agent, o.project),
                            title,
                            format!(
                                "{}{}",
                                age(o.ts),
                                if o.repeats > 1 {
                                    format!(" shown again ×{}", o.repeats)
                                } else {
                                    String::new()
                                }
                            ),
                        ]
                    })
                    .collect(),
                Side::Off => Vec::new(),
            }
        }
    }
    fn draw(&self) -> io::Result<()> {
        let mut out = io::stdout();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        let header = format!(
            "mdv{}{}{} | {}",
            if self.side_visible() {
                format!(
                    " | {}{}",
                    self.side_label(),
                    if self.overlay() { " (Esc back)" } else { "" }
                )
            } else {
                String::new()
            },
            if self.converted {
                " | Converted view"
            } else {
                ""
            },
            if self.state.pinned { " | Pin" } else { "" },
            self.path.display()
        );
        write!(out, "{}", clip(&visible(&header), 0, self.size.0 as usize))?;
        let side_rows = self.side_rows();
        let side_scroll = self.side_scroll() * self.side_row_height();
        for y in 0..self.height() {
            queue!(out, MoveTo(0, (y + 1) as u16))?;
            if self.side_visible() {
                let text = side_rows.get(side_scroll + y).cloned().unwrap_or_default();
                let selected = self.side_cursor() == (side_scroll + y) / self.side_row_height();
                let gutter = if selected {
                    &self.style.focus_gutter
                } else {
                    &self.style.idle_gutter
                };
                let side = clip(&visible(&format!("{gutter}{text}")), 0, self.side_width());
                write!(out, "{side}")?;
                if !self.overlay() {
                    let pad = self
                        .side_width()
                        .saturating_sub(UnicodeWidthStr::width(side.as_str()));
                    write!(out, "{}{}", " ".repeat(pad), self.style.separator)?;
                }
            }
            if self.overlay() {
                continue;
            }
            if let Some(row) = self.layout.rows.get(self.state.top + y) {
                let focus = self.state.focus == Some(row.line.block_id);
                write!(
                    out,
                    "{}",
                    if !row.discussion && self.changed_blocks.contains(&row.line.block_id) {
                        format!(
                            "{}!{}{}",
                            self.style.quote,
                            if focus { ">" } else { " " },
                            self.style.reset
                        )
                    } else if focus {
                        self.style.focus_gutter.clone()
                    } else {
                        self.style.idle_gutter.clone()
                    }
                )?;
                let scroll = if row.line.horizontally_scrollable {
                    self.horizontal
                } else {
                    0
                };
                let text = clip(&row.line.plain, scroll, self.width());
                if row.discussion {
                    write!(out, "{}{}{}", self.style.quote, text, self.style.reset)?;
                    continue;
                }
                let byte_start = cell_byte(&row.line.plain, scroll);
                let mut selected = false;
                for (i, g) in text.grapheme_indices(true) {
                    let p = Position {
                        block: row.line.block_id,
                        byte: row.range.start + byte_start + i,
                    };
                    let highlight = self.state.selection.is_some_and(|s| {
                        let (a, b) = s.ordered();
                        p >= a && p < b
                    }) || {
                        let at = self.matches.partition_point(|m| *m <= p);
                        at > 0
                            && self.matches[at - 1].block == p.block
                            && p.byte < self.matches[at - 1].byte + self.query.len()
                    };
                    if highlight != selected {
                        write!(
                            out,
                            "{}",
                            if highlight {
                                &self.style.selected
                            } else {
                                &self.style.reset
                            }
                        )?;
                        selected = highlight;
                    }
                    write!(out, "{g}")?;
                }
                if selected {
                    write!(out, "{}", self.style.reset)?;
                }
            }
        }
        if let Some(at) = self.menu {
            let labels = [
                "Copy".to_owned(),
                format!(
                    "{}Markdown source",
                    if self.state.copy_mode == CopyMode::Markdown {
                        &self.style.checked
                    } else {
                        &self.style.unchecked
                    }
                ),
                "Link".into(),
                "Comment".into(),
                "Ask the agent".into(),
                "a 에이전트로 가기".into(),
                "s 막힌 질문 다시 보내기".into(),
            ];
            for (i, label) in labels.iter().enumerate() {
                let y = (2 + i).min(self.height());
                queue!(
                    out,
                    MoveTo(
                        self.doc_x().min(self.size.0.saturating_sub(1) as usize) as u16,
                        y as u16
                    )
                )?;
                write!(
                    out,
                    "{}{}",
                    if at == i {
                        &self.style.focus_gutter
                    } else {
                        &self.style.idle_gutter
                    },
                    clip(
                        label,
                        0,
                        (self.size.0 as usize).saturating_sub(self.doc_x() + 2)
                    )
                )?;
            }
        }
        let project = match self.project() {
            project if !project.is_empty() => project,
            _ => self
                .path
                .parent()
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        };
        let offer = self
            .inbox
            .entries
            .iter()
            .find(|o| Path::new(&o.path) == self.path)
            .map(|o| format!(" | shown by {} | {}", o.agent, age(o.ts)))
            .unwrap_or_default();
        let percent = if self.max_top() == 0 {
            100
        } else {
            self.state.top * 100 / self.max_top()
        };
        let lens_status = if self.changes.is_empty() {
            String::new()
        } else {
            format!(" | 지난번 본 뒤 바뀐 곳 {} · d 다음", self.changes.len())
        };
        let warning = self
            .lens_warning
            .as_ref()
            .map(|s| format!(" | {s}"))
            .unwrap_or_default();
        let status = format!(
            "{} | {project}{lens_status}{warning}{offer} | {percent}% | {}",
            self.host, self.message
        );
        queue!(
            out,
            MoveTo(0, self.size.1.saturating_sub(2)),
            Clear(ClearType::CurrentLine)
        )?;
        write!(out, "{}", clip(&visible(&status), 0, self.size.0 as usize))?;
        let footer = if let Some((kind, text, _)) = &self.composer {
            format!(
                "{} · block {}: {} | Enter send · Esc cancel",
                kind,
                self.state.focus.unwrap_or(0),
                visible(text)
            )
        } else if let Some(s) = &self.search {
            format!("Search: /{}", visible(s))
        } else {
            format!(
                "T -> {} | y {} (m) | d 바뀐 곳 | a 에이전트로 가기 | s 재전송 | o Outline | f side | Tab block | / search | q quit{}",
                self.state.side.next().name(),
                if self.state.copy_mode == CopyMode::Text {
                    "text"
                } else if self.state.selection.is_some() {
                    "containing block's Markdown"
                } else if self.converted {
                    "converted Markdown"
                } else {
                    "markdown"
                },
                if self.state.updated {
                    " | File updated · r to refresh"
                } else {
                    ""
                }
            )
        };
        queue!(
            out,
            MoveTo(0, self.size.1.saturating_sub(1)),
            Clear(ClearType::CurrentLine)
        )?;
        let caret = if self.composer.is_some() {
            Some(footer.len() - " | Enter send · Esc cancel".len())
        } else {
            self.search.as_ref().map(|_| footer.len())
        };
        let width = self.size.0.max(1) as usize;
        let mut start = 0;
        if let Some(caret) = caret {
            let cells = UnicodeWidthStr::width(&footer[..caret]);
            start = cell_byte(&footer, cells.saturating_sub(width - 1));
            if UnicodeWidthStr::width(&footer[start..caret]) >= width {
                start += footer[start..].graphemes(true).next().map_or(0, str::len);
            }
        }
        write!(out, "{}", clip(&footer[start..], 0, width))?;
        if let Some(caret) = caret {
            queue!(
                out,
                MoveTo(
                    UnicodeWidthStr::width(&footer[start..caret]) as u16,
                    self.size.1.saturating_sub(1)
                )
            )?;
        }
        out.flush()
    }
}
fn clip(text: &str, start: usize, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for g in text[cell_byte(text, start)..].graphemes(true) {
        let w = UnicodeWidthStr::width(g);
        if used + w > width {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out
}
pub fn run(path: &Path, options: ViewOptions, hooks: Hooks) -> Result<(), String> {
    run_terminal(path, options, hooks).map_err(|e| e.to_string())
}
fn run_terminal(path: &Path, options: ViewOptions, hooks: Hooks) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "mdv view requires an interactive terminal",
        ));
    }
    let mut app = App::new(path, hooks)?;
    app.project = options.project;
    if options.look.is_some() || options.dark {
        app.style = Style::for_look(options.look.unwrap_or(mdv::Look::Portal), options.dark);
        app.rewrap();
    }
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        old_hook(info);
    }));
    let _terminal = Terminal::enter(&app.style)?;
    let mut last = Instant::now();
    let mut painted = Instant::now();
    let mut dirty = true;
    let result = (|| -> io::Result<()> {
        loop {
            match SIGNAL.swap(0, Ordering::SeqCst) {
                2 | 15 => break,
                s if s == SUSPEND => {
                    restore();
                    unsafe {
                        raise(STOP);
                    }
                    activate(&app.style)?;
                    app.size = terminal::size()?;
                    app.rewrap();
                    dirty = true;
                }
                _ => {}
            }
            if dirty || painted.elapsed() >= Duration::from_secs(60) {
                app.draw()?;
                dirty = false;
                painted = Instant::now();
            }
            if event::poll(Duration::from_millis(100))? {
                dirty = true;
                match event::read()? {
                    Event::Key(key) => match app.key(key) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) => app.message = visible(&e.to_string()),
                    },
                    Event::Mouse(mouse) => {
                        if let Err(e) = app.mouse(mouse) {
                            app.message = visible(&e.to_string());
                        }
                    }
                    Event::Resize(w, h) => {
                        app.size = (w, h);
                        app.rewrap();
                    }
                    Event::Paste(s) => {
                        if let Some((_, text, cancel)) = app.composer.as_mut() {
                            let remaining = 2000usize.saturating_sub(text.chars().count());
                            text.extend(s.chars().filter(|c| !c.is_control()).take(remaining));
                            *cancel = false;
                        } else if let Some(search) = app.search.as_mut() {
                            search.push_str(&s);
                        }
                    }
                    _ => {}
                }
            }
            if last.elapsed() >= Duration::from_millis(500) {
                dirty |= app.tick();
                last = Instant::now();
            }
        }
        Ok(())
    })();
    app.leave_document();
    drop(_terminal);
    if let Some(warning) = &app.lens_warning {
        eprintln!("{}", visible(warning));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn change_lines_map_to_blocks_and_deletions() {
        let src = "# 제목\n\n한글 문단\n다음 줄\n\n## 끝\n";
        let doc = parse(src);
        let change = |new| mdv::lens::Change { old: 0..0, new };
        assert_eq!(changed_blocks(src, &doc, &[change(2..4)]), vec![1]);
        assert_eq!(changed_blocks(src, &doc, &[change(1..1)]), vec![1]);
        assert_eq!(changed_blocks(src, &doc, &[change(5..5)]), vec![2]);
        assert_eq!(changed_blocks(src, &doc, &[change(6..6)]), vec![2]);
        assert!(changed_blocks("", &parse(""), &[change(0..0)]).is_empty());
        let mut app = fixture(src);
        app.changed_blocks = vec![0, 2];
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.state.focus, Some(2));
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.state.focus, Some(0));
    }
    #[test]
    fn lens_lifecycle() {
        if env::var_os("J4_LENS_CHILD").is_none() {
            let home = env::temp_dir().join(format!("mdv-j4-{}", mdv::threads::new_id()));
            fs::create_dir_all(&home).unwrap();
            let name = format!(
                "{}::lens_lifecycle",
                module_path!().split_once("::").unwrap().1
            );
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env("HOME", &home)
                .env("J4_LENS_CHILD", "1")
                .env("MDV_THREADS_FILE", home.join("threads.jsonl"))
                .env("MDV_ASK_DRYRUN", home.join("dry-run"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        let watched = home.join("STATE.md");
        let other = home.join("notes.md");
        fs::write(&watched, "# 처음\n\n본문\n").unwrap();
        fs::write(&other, "# 일반 문서\n").unwrap();
        let store = mdv::lens::default_store().unwrap();
        let mut app = fixture("");
        app.seen_store = Some(store.clone());
        app.open(&watched, true).unwrap();
        assert!(app.changes.is_empty());
        assert!(!store.exists(), "opening must not save a snapshot");
        app.open(&other, true).unwrap();
        assert_eq!(
            mdv::lens::changes(&store, &watched, "# 처음\n\n본문\n"),
            Some(vec![])
        );
        let snapshots = fs::read_dir(&store).unwrap().count();
        app.leave_document();
        assert_eq!(fs::read_dir(&store).unwrap().count(), snapshots);
        fs::write(&watched, "# 고침\n\n본문\n").unwrap();
        app.open(&watched, true).unwrap();
        assert_eq!(app.changed_blocks, vec![0]);
        let baseline = mdv::lens::changes(&store, &watched, "# 처음\n\n본문\n");
        assert_eq!(baseline, Some(vec![]));
        fs::write(&watched, "# 고침\n\n새 본문\n").unwrap();
        app.open(&watched, false).unwrap();
        assert_eq!(app.changed_blocks, vec![0, 1]);
        assert_eq!(
            mdv::lens::changes(&store, &watched, "# 처음\n\n본문\n"),
            baseline
        );
        // A second reader still sees the changes while the first reader stays open.
        let mut second = fixture("");
        second.seen_store = Some(store.clone());
        second.open(&watched, true).unwrap();
        assert_eq!(second.changed_blocks, app.changed_blocks);
        app.leave_document();
        assert_eq!(mdv::lens::changes(&store, &watched, &app.src), Some(vec![]));
        app.open(&other, true).unwrap();
        assert!(app.changes.is_empty());
        assert!(app.changed_blocks.is_empty());
        // Save failure must keep navigation and reading available.
        app.open(&watched, true).unwrap();
        let blocked = home.join("not-a-directory");
        fs::write(&blocked, "x").unwrap();
        app.seen_store = Some(blocked);
        app.open(&other, true).unwrap();
        assert!(app.content_available);
        assert!(app.lens_warning.as_ref().unwrap().contains("저장 실패"));
    }
    #[test]
    fn clipping() {
        assert_eq!(clip("가나다", 2, 3), "나");
    }
    #[test]
    fn feed_uses_json_parser() {
        let dir = env::temp_dir().join(format!("mdv-feed-{}", mdv::threads::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view_feed_unit.jsonl");
        fs::write(&path,"bad\n{\"id\":\"one\",\"path\":\"/a\\tb\",\"agent\":\"build\",\"project\":\"52\",\"ts\":\"2026-10-12T09:00:00+0900\"}\n").unwrap();
        let offers = read_feed(&path).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].path, "/a\tb");
        assert_eq!(offers[0].ts, 1791763200);
    }
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
            entries: Vec::new(),
            inbox: Inbox::default(),
            feed: PathBuf::new(),
            feed_stamp: None,
            file_stamp: None,
            style,
            size: (120, 24),
            search: None,
            query: String::new(),
            matches: Vec::new(),
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
            host: "test".into(),
        }
    }
    #[test]
    fn copy_hook_receives_text_and_reports_its_outcome() {
        let mut app = fixture("# Title\n\n본문\n");
        app.state.focus = Some(1);
        let received = Arc::new(Mutex::new(Vec::new()));
        let texts = Arc::clone(&received);
        let mut outcomes = vec![
            CopyOutcome::Copied,
            CopyOutcome::SentToTerminal,
            CopyOutcome::Failed("clipboard unavailable".into()),
        ]
        .into_iter();
        app.copy_hook = Box::new(move |text| {
            texts.lock().unwrap().push(text.to_owned());
            outcomes.next().unwrap()
        });
        app.copy(false).unwrap();
        assert_eq!(app.message, "Copied");
        app.copy(false).unwrap();
        assert_eq!(app.message, "Copy sent via OSC 52 (paste to verify)");
        app.copy(false).unwrap();
        assert_eq!(app.message, "Copy failed: clipboard unavailable");
        assert_eq!(*received.lock().unwrap(), vec!["본문", "본문", "본문"]);
    }
    #[test]
    fn converter_lookup_and_hash_use_no_external_utilities() {
        use std::os::unix::fs::PermissionsExt;
        if env::var_os("MDV_CONVERTER_CHILD").is_none() {
            let root = env::temp_dir().join(format!("mdv-converter-{}", mdv::threads::new_id()));
            fs::create_dir_all(&root).unwrap();
            for name in ["unpress", "override"] {
                let path = root.join(name);
                fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let name = format!(
                "{}::converter_lookup_and_hash_use_no_external_utilities",
                module_path!().split_once("::").unwrap().1
            );
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env("HOME", &root)
                .env("PATH", &root)
                .env("MDV_THREADS_FILE", root.join("threads.jsonl"))
                .env("MDV_ASK_DRYRUN", root.join("dry-run"))
                .env("MDV_CONVERTER_CHILD", "1")
                .env("MDV_CONVERTER", root.join("override"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = PathBuf::from(env::var_os("HOME").unwrap());
        assert_eq!(converter(), root.join("override"));
        // This is an isolated child running exactly one test.
        unsafe {
            env::remove_var("MDV_CONVERTER");
        }
        assert_eq!(converter(), root.join("unpress"));
        let path = root.join("document.pdf");
        fs::write(&path, "abc").unwrap();
        let mut app = fixture("");
        app.open(&path, true).unwrap();
        assert_eq!(
            app.conversion.as_ref().unwrap().folder,
            root.join(
                ".cache/mdv/conv/ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
        );
        fs::set_permissions(root.join("unpress"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(converter(), PathBuf::from("docconv"));
    }
    #[test]
    fn feed_timestamp_formats_and_bad_records() {
        for text in [
            "2026-10-12T00:00:00Z",
            "2026-10-12T09:00:00+09:00",
            "2026-10-11T19:00:00-0500",
            "2026-10-12T00:00:00.123Z",
        ] {
            assert_eq!(epoch(&serde_json::json!(text)), Some(1791763200));
        }
        assert_eq!(epoch(&serde_json::json!(1791763200.9)), Some(1791763200));
        for text in [
            "bad",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-10-12T25:00:00Z",
            "2026-10-12T00:00:00+2500",
            "2026-10-12T00:00:00.Z",
        ] {
            assert_eq!(epoch(&serde_json::json!(text)), None);
        }
    }
    #[test]
    fn composer_keeps_commands_as_text_and_confirms_cancel() {
        let mut app = fixture("# Target\n\nbody");
        app.state.focus = Some(0);
        app.menu = Some(3);
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        for c in ['q', 'T', '한'] {
            assert!(
                !app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                    .unwrap()
            );
        }
        app.size = (40, 20);
        app.rewrap();
        assert_eq!(app.composer.as_ref().unwrap().1, "qT한");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.composer.is_some());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.composer.is_none());
    }
    #[test]
    fn discussion_rows_never_enter_source_copy() {
        let mut app = fixture(&format!("# Target\n\n{}", "body\n\n".repeat(40)));
        app.threads.push(mdv::threads::Event {
            kind: "answer".into(),
            req_id: "fixture".into(),
            path: String::new(),
            feed_id: None,
            project: None,
            block_id: 0,
            range: [0, 9],
            fingerprint: format!("{:016x}", app.doc.blocks[0].fingerprint),
            text: "reply only".into(),
            source: "terminal".into(),
            time: 0,
            agent: None,
            quote: None,
            before: None,
            after: None,
            doc_hash: None,
            selected: None,
        });
        app.rewrap();
        let row = app.layout.rows.iter().position(|r| r.discussion).unwrap();
        assert!(app.layout.point(row, 0).is_none());
        assert!(!app.layout.texts[0].contains("reply only"));
        assert!(app.layout.rows[row].line.plain.contains("Answer ready"));
        let mut answer = app.threads[0].clone();
        answer.text = "second answer".into();
        app.threads.push(answer.clone());
        app.rewrap();
        app.state.top = app
            .layout
            .rows
            .iter()
            .position(|r| r.line.plain.contains("second answer"))
            .unwrap();
        let top = app.state.top;
        answer.text = "third answer".into();
        app.threads.push(answer);
        app.rewrap();
        assert_eq!(app.state.top, top);
        assert!(app.layout.rows[top].line.plain.contains("second answer"));
    }
    #[test]
    fn search_does_not_fire_commands_and_side_changes_keep_selection() {
        let mut app = fixture("# 한글\n\n한글 문장입니다.");
        let sel = Selection {
            start: Position { block: 1, byte: 0 },
            end: Position { block: 1, byte: 6 },
        };
        app.state.selection = Some(sel);
        for c in ['T', 'o', 'o', 'T', 'T'] {
            app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
            assert_eq!(app.state.selection, Some(sel));
        }
        assert_eq!(app.state.side, Side::Off);
        app.search = Some(String::new());
        for c in ['q', 'ㅅ', 'ㅡ'] {
            assert!(
                !app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                    .unwrap()
            );
        }
        assert_eq!(app.search.as_deref(), Some("qㅅㅡ"));
        assert_eq!(app.state.side, Side::Off);
        assert_eq!(app.state.copy_mode, CopyMode::Text);
        app.search = Some("한글".into());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.matches.len(), 2);
        // Drag endpoints are content positions; resizing cannot alter their bytes.
        app.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 6,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        let selection = app.state.selection.unwrap();
        let text = app.layout.text_copy(selection);
        assert_eq!(text, "한글");
        app.size = (81, 24);
        app.rewrap();
        assert_eq!(app.layout.text_copy(selection), text);
    }
    #[test]
    fn pinned_file_refresh_and_hidden_tree() {
        let folder = env::temp_dir().join(format!("mdv-files-{}", mdv::threads::new_id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("visible.md");
        fs::write(&path, "# Before\n").unwrap();
        fs::write(folder.join(".hidden"), "x").unwrap();
        let mut app = fixture("");
        app.tree = folder;
        app.list_tree().unwrap();
        assert!(
            !app.entries
                .iter()
                .any(|p| p.file_name().unwrap() == ".hidden")
        );
        app.open(&path, true).unwrap();
        app.state.pinned = true;
        fs::write(&path, "# After a change\n").unwrap();
        app.tick();
        assert!(app.state.updated);
        assert_eq!(app.src, "# Before\n");
        app.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.src, "# After a change\n");
        assert!(!app.state.updated);
        app.state.pinned = false;
        fs::write(&path, "# Follow change\n").unwrap();
        app.tick();
        assert_eq!(app.src, "# Follow change\n");
    }
}
