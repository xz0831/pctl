//! The pctl app: what `pctl` alone opens on a terminal: projects and their agents on
//! the left, the project's agents as tabs on top, the selected agent's live terminal in the
//! middle, and the mouse for everything (click to open, `+` to add, right-click for actions).
//! Shortcuts come from `keys` and can be changed in a config file. tmux stays behind the app and
//! keeps the agents running; leaving the app stops nothing. With saved machines (`pctl machine
//! add`) the sidebar shows this computer first and each of them after it, and everything works on
//! their agents too, through the bridge on that computer. The terminal area can be split into cells,
//! each showing one agent (from any computer); see `layout`.
mod docs;
pub(crate) mod keys;
mod layout;
mod machines;
mod quips;
pub(crate) mod term;

use crate::clipboard;
use crate::{
    Core,
    bridge::{self, KINDS, Screen},
    theme::{Palette, ThemeName},
};
use anyhow::Result;
use bridge::CLIP_PIECE;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::{
    cursor::Show,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use docs::DocRow;
use keys::{ACTIONS, Action, Keymap};
use layout::{Dir, Divider, Side, Tree};
use machines::{ACT_WITHIN, Id, Mach};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{self, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use std::{panic, sync::Arc};

/// At most one notification per agent in this long.
const NOTICE_EVERY: Duration = Duration::from_secs(10);
/// Documents listed under one project in the sidebar.
const DOCS_PER_PROJECT: usize = 8;
/// How often the screen is read: fast while it changes, slower the longer it stays the same.
fn capture_every(quiet: u32) -> Duration {
    Duration::from_millis(match quiet {
        0..=4 => 40,
        5..=19 => 120,
        _ => 250,
    })
}
/// The requests that put an image on another computer's clipboard and then press the key that
/// pastes it: the image in pieces, the set, the key.
fn clip_ops(agent: &str, run: &str, png: &[u8]) -> VecDeque<Value> {
    let mut ops: VecDeque<Value> = png
        .chunks(CLIP_PIECE)
        .enumerate()
        .map(|(seq, piece)| {
            json!({"op": "clip_put", "agent": agent, "run": run, "seq": seq, "data": term::base64(piece)})
        })
        .collect();
    ops.push_back(
        json!({"op": "clip_set", "agent": agent, "run": run, "kind": "png", "bytes": png.len()}),
    );
    ops.push_back(json!({"op": "keys", "agent": agent, "run": run, "keys": [{"literal": false, "key": "C-v"}]}));
    ops
}
/// Typed text waits for the end of a burst of events, but never grows past this many bytes.
const TYPED_MAX: usize = 2048;
/// A paste to another computer travels in one bridge line of at most 1 MiB, escapes included.
const REMOTE_PASTE_MAX: usize = 400_000;
type PanicHook = dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static;

pub(crate) struct TerminalGuard {
    previous_hook: Option<Arc<PanicHook>>,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        ) {
            restore_terminal();
            return Err(error.into());
        }
        let previous_hook: Arc<PanicHook> = Arc::from(panic::take_hook());
        let on_panic = Arc::clone(&previous_hook);
        panic::set_hook(Box::new(move |info| {
            restore_terminal();
            on_panic(info);
        }));
        Ok(Self {
            previous_hook: Some(previous_hook),
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        // set_hook panics if called from an unwinding thread.
        if !std::thread::panicking()
            && let Some(previous) = self.previous_hook.take()
        {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

/// While an answer from another computer is on its way, look for it this often.
const SOON: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
struct Agent {
    /// "" is this computer.
    machine: String,
    name: String,
    project: String,
    status: String,
    stale: bool,
    pane: Option<String>,
    run: Option<String>,
    /// Tell messages kept for it while it waited for a decision.
    queued: usize,
    /// The reader of a document (`pctl view`), not an agent: shown in its cell only.
    viewer: bool,
}
impl Agent {
    fn running(&self) -> bool {
        self.pane.is_some() && !matches!(self.status.as_str(), "stopped" | "lost")
    }
    fn id(&self) -> Id {
        Id::new(&self.machine, &self.name)
    }
    fn project_id(&self) -> Id {
        Id::new(&self.machine, &self.project)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Machine(String),
    Project(Id),
    Agent(Id),
    /// A document row: the computer and the document's ID.
    Doc(Id),
    Tab(Id),
    NewAgent(Id),
    NewTab(Id),
    NewProject,
    /// A cell's terminal area.
    Pane(u32),
    /// A cell's title row.
    Title(u32),
    Divider(Divider),
    Theme,
    Help,
    Detach,
    Start(Id, bool),
    SidebarEdge,
    /// The "more" line at the top (true) or bottom of a sidebar that does not fit.
    SideMore(bool),
    Menu(usize),
    Field(usize),
    Choice(usize, usize),
    Submit,
    Cancel,
}

#[derive(Clone, Debug)]
enum Act {
    Restart(Id),
    /// The agent and the run shown when Stop was asked.
    Stop(Id, Option<String>),
    Start(Id, bool),
    Remove(Id),
    /// The project.
    NewAgent(Id),
    NewTab(Id),
    Theme(ThemeName),
    Custom(String),
    Reconnect(String),
    /// A command on the focused cell, from the agent menu.
    Cell(Action),
}

struct Field {
    label: &'static str,
    value: String,
    choices: &'static [&'static str],
    hint: &'static str,
}
struct Form {
    /// None: a new project; Some(project): a new agent in it.
    project: Option<Id>,
    fields: Vec<Field>,
    focus: usize,
    error: String,
    /// Sent to another computer and not answered yet.
    busy: bool,
}
enum Modal {
    Form(Form),
    Menu {
        title: String,
        items: Vec<(String, Act)>,
        x: u16,
        y: u16,
        sel: usize,
    },
    Confirm {
        text: String,
        button: &'static str,
        act: Act,
    },
    Help,
}

#[derive(Default)]
struct View {
    /// The computer and pane the rows below were read from.
    owner: (String, String),
    rows: Vec<term::Row>,
    cursor: Option<(u16, u16)>,
    size: (u16, u16),
    /// The size this app last asked tmux for. Resizing only when this changes keeps two apps that
    /// show the same agent at different sizes from resizing it back and forth.
    asked: (u16, u16),
    scroll: usize,
    history: usize,
    /// The program in the pane is full-screen; `mouse`/`sgr`: it reads the mouse (SGR encoding).
    alt: bool,
    mouse: bool,
    sgr: bool,
    next: Option<Instant>,
    /// The last screen read; an equal one needs no parsing and no redraw.
    last: Option<Screen>,
    /// Captures in a row that found the screen unchanged.
    quiet: u32,
    /// The one screen request to another computer in flight.
    pending: Option<mpsc::Receiver<Result<Value>>>,
}

/// One cell of the terminal area: the agent it shows and everything it keeps about the screen.
#[derive(Default)]
struct Pane {
    agent: Option<Id>,
    view: View,
    /// Where it is drawn, with its title row, and without it.
    outer: Rect,
    inner: Rect,
}

impl View {
    /// How far back the view can go in a cell `height` rows tall: the history, and the rows of a
    /// taller pane that sit above what the cell shows live.
    fn max_scroll(&self, height: u16) -> usize {
        self.history + self.size.1.saturating_sub(height) as usize
    }
}

struct Update {
    machine: String,
    agents: std::result::Result<Vec<Value>, String>,
    projects: Vec<(String, String)>,
    connected: bool,
    /// None: the list failed, so the documents shown stay as they were.
    docs: Option<Vec<DocRow>>,
}
impl Update {
    /// From a `list` answer (`bridge::list`, here or over the bridge).
    fn from_list(machine: &str, answer: Result<Value>) -> Update {
        let list = answer.and_then(|v| {
            if v["ok"] == json!(false) {
                anyhow::bail!("{}", v["error"].as_str().unwrap_or("list failed"));
            }
            Ok(v)
        });
        match list {
            Ok(v) => Update {
                machine: machine.into(),
                agents: match v["error"].as_str() {
                    Some(e) => Err(e.to_string()),
                    None => Ok(v["sessions"].as_array().cloned().unwrap_or_default()),
                },
                projects: v["projects"]
                    .as_array()
                    .map(|ps| {
                        ps.iter()
                            .map(|p| {
                                (
                                    p["name"].as_str().unwrap_or_default().to_string(),
                                    p["root"].as_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                connected: v["connected"].as_bool().unwrap_or(false),
                docs: Some(docs::parse(machine, &v)),
            },
            Err(e) => Update {
                machine: machine.into(),
                agents: Err(format!("{e:#}")),
                projects: vec![],
                connected: false,
                docs: None,
            },
        }
    }
}

/// Rows of the sidebar, for drawing and for keyboard moves.
#[derive(Clone, Debug, PartialEq)]
enum SideRow {
    Machine(String),
    /// A muted line under a machine: no server here, or why it is down.
    Note(String),
    Project(Id),
    Agent(Id),
    /// A document of the project: the computer and the document's ID.
    Doc(Id),
    Add(Id),
}

/// The sidebar's `len` rows in `cap` lines from row `off`: the first row shown (kept in range),
/// how many rows, and how many are hidden above and below. A "more" line takes a line of its own.
fn side_window(len: usize, cap: usize, off: usize) -> (usize, usize, usize, usize) {
    if len <= cap || cap < 3 {
        return (0, len.min(cap), 0, len - len.min(cap));
    }
    let off = off.min(len - cap + 1);
    let mut shown = cap - usize::from(off > 0);
    if off + shown < len {
        shown -= 1;
    }
    (off, shown, off, len - off - shown)
}
/// The first row to show so that row `i` is on screen, scrolling as little as possible.
fn reveal(len: usize, cap: usize, off: usize, i: usize) -> usize {
    let mut off = side_window(len, cap, off).0;
    if i < off {
        return i;
    }
    loop {
        let (o, shown, ..) = side_window(len, cap, off);
        if i < o + shown || o + shown >= len {
            return o;
        }
        off = o + 1;
    }
}

/// The local clipboard's image, read on a thread of its own.
type ClipRead = mpsc::Receiver<Result<Option<Vec<u8>>>>;

/// What an answer from another computer was for.
enum Task {
    /// Typed keys, or a paste, for this agent.
    Keys(Agent),
    Paste(Agent),
    /// An image on its way to the clipboard of the agent's computer: the requests still to send
    /// (pieces, the set, then the key that pastes) and its size in KB.
    Image {
        agent: Agent,
        rest: VecDeque<Value>,
        kb: usize,
    },
    /// An action; on success say `ok` and then do `then`.
    Act {
        machine: String,
        ok: String,
        then: Then,
    },
}
/// What follows a successful action.
enum Then {
    Nothing,
    /// A new agent or tab in this project: show it.
    Opened(Id, String),
    /// The new-agent form's: also close the form.
    Form(Id, String),
    /// A document's reader is running: put it in a cell.
    Doc(DocRow),
}

struct App {
    pal: Palette,
    theme: ThemeName,
    /// A theme file of the person's own (`pctl theme --new`), when one is picked.
    custom: Option<String>,
    customs: Vec<crate::theme::custom::Custom>,
    keys: Keymap,
    /// This computer first, then the enabled saved machines in file order.
    machines: Vec<Mach>,
    agents: Vec<Agent>,
    /// The documents of every computer, newest first within a computer.
    docs: Vec<DocRow>,
    project: Option<Id>,
    tab: HashMap<Id, String>,
    collapsed: HashSet<Id>,
    collapsed_machines: HashSet<String>,
    sidebar: bool,
    sidebar_w: u16,
    in_sidebar: bool,
    side_sel: usize,
    /// The first sidebar row shown, how far it can go, the room it was drawn in, and what the
    /// last draw saw (keyboard row, current agent) so a change brings its row into view.
    side_off: usize,
    side_max: usize,
    side_area: Rect,
    side_seen: (bool, usize, Option<Id>),
    /// Every cell there is, by number; the trees below hold the numbers.
    panes: HashMap<u32, Pane>,
    next_pane: u32,
    /// The cells of the project on screen, and of each other project that was shown.
    tree: Tree<u32>,
    layouts: HashMap<Id, Tree<u32>>,
    /// A divider being dragged, and whether it moved.
    dragging: Option<(Divider, bool)>,
    modal: Option<Modal>,
    prefix: bool,
    notice: String,
    notice_at: Instant,
    hits: Vec<(Rect, Hit)>,
    term: Rect,
    drag: bool,
    quit: bool,
    refresh: Option<mpsc::Sender<()>>,
    /// Agents added here that a refresh started earlier may not list yet.
    fresh: HashMap<Id, (Agent, Instant)>,
    /// Panes whose window this app resized; a window with a client attached stays manual.
    touched: HashSet<String>,
    /// The last left click, to tell a double-click.
    last_click: Option<(Instant, Hit)>,
    /// The selection in a cell: which one, where the drag began and where it is now.
    sel: Option<(u32, term::Cell, term::Cell)>,
    /// When each agent was last announced, for the 10-second limit.
    notified: HashMap<Id, Instant>,
    /// The same for documents, keyed by computer and document.
    doc_notified: HashMap<Id, Instant>,
    /// Document readers whose cell went, to be ended.
    ending: Vec<Id>,
    /// Keys typed in the events just read, for the agent they were typed to: text to send with
    /// `-l` (true) or a key name. They go out as one tmux call, see `flush_typed`.
    typed: Option<(Agent, Vec<(bool, String)>)>,
    /// Requests to other computers whose answer is still to come.
    tasks: Vec<(mpsc::Receiver<Result<Value>>, Task)>,
    /// The local clipboard being read for this agent, on a thread of its own.
    reading: Option<(Agent, ClipRead)>,
}

/// Write a complete escape sequence (a clipboard write, a notification) to the same terminal the
/// drawing goes to. Called between frames, so it cannot corrupt a half-written one.
fn write_out(sequence: &str) {
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

/// What one wheel notch at pane cell (`col`, `row`) types into a full-screen program: an SGR mouse
/// wheel event where it reads the mouse that way, three arrow keys where it does not read the mouse.
/// shortcut: the legacy X10 mouse encoding gets nothing (it needs raw bytes over 127); every agent
/// seen so far asks for SGR.
fn wheel_input(up: bool, col: u16, row: u16, mouse: bool, sgr: bool) -> Vec<(bool, String)> {
    match (mouse, sgr) {
        (true, true) => {
            let button = if up { 64 } else { 65 };
            vec![(true, format!("\x1b[<{button};{};{}M", col + 1, row + 1))]
        }
        (true, false) => vec![],
        (false, _) => vec![(false, if up { "Up" } else { "Down" }.to_string()); 3],
    }
}

/// The view cell a mouse position points at: `skip` is how many of the pane's own rows the view
/// holds above the first drawn one (a taller pane shows its bottom rows, scrolled back the older).
fn view_cell(term: Rect, skip: u16, rows: usize, column: u16, row: u16) -> term::Cell {
    let col = column
        .saturating_sub(term.x)
        .min(term.width.saturating_sub(1));
    let r = row.saturating_sub(term.y) as usize + skip as usize;
    (r.min(rows.saturating_sub(1)) as u16, col)
}

/// The agent name as it goes into an escape sequence: no control characters, no `;`.
fn sanitize(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && *c != ';')
        .collect()
}

/// The notice text for one agent: `done` when it went idle, `needs you` when it is blocked.
fn notice_body(name: &str, text: &str) -> String {
    format!("{}: {text}", sanitize(name))
}

/// The OSC 777 notification for one agent, with a bell when it is blocked.
fn notice_sequence(name: &str, text: &str, bell: bool) -> String {
    let mut s = format!("\x1b]777;notify;pctl;{}\x07", notice_body(name, text));
    if bell {
        s.push('\x07');
    }
    s
}

/// At most one notice per agent per `NOTICE_EVERY`.
fn may_notify(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= NOTICE_EVERY)
}

/// The notifications for states this app watched change: an agent it saw `working` that is now
/// idle or blocked, never one it sees for the first time and never the one on screen.
fn notifications(
    before: &HashMap<Id, String>,
    now: &[Agent],
    shown: Option<&Id>,
) -> Vec<(Id, &'static str, bool)> {
    let mut out = vec![];
    for a in now {
        if before.get(&a.id()).map(String::as_str) != Some("working") || shown == Some(&a.id()) {
            continue;
        }
        match a.status.as_str() {
            "idle" => out.push((a.id(), "done", false)),
            "blocked" => out.push((a.id(), "needs you", true)),
            _ => {}
        }
    }
    out
}

pub fn agent_caller(core: &Core) -> bool {
    let sock = core.state.join("tmux.sock");
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    std::env::var_os("PCTL_AGENT_ID").is_some()
        || std::env::var_os("PCTL_RUN_ID").is_some()
        || std::env::var("TMUX").is_ok_and(|t| {
            t.split(',')
                .next()
                .is_some_and(|s| canon(std::path::Path::new(s)) == canon(&sock))
        })
}

/// Open the app until the person leaves it. Never starts a server by itself, here or on another
/// computer: adding the first agent goes through `session add`, which keeps its own start rules.
pub fn run(core: &Core) -> Result<()> {
    anyhow::ensure!(
        !agent_caller(core),
        "agent_caller: the app is for a person at a terminal. An agent of this server (PCTL_AGENT_ID, PCTL_RUN_ID, or a pane of its tmux) could answer another agent's question with a keystroke; use `pctl session` and `msg`"
    );
    core.retire_old_panel()?;
    let (keys, warnings) = Keymap::load();
    let mut app = App::new(core.theme()?, keys);
    match crate::machine::load() {
        Ok(saved) => app.machines.extend(
            saved
                .iter()
                .filter(|m| m.enabled)
                .map(|m| Mach::remote(&m.label, &m.target)),
        ),
        Err(e) => app.say(format!("Machines: {e:#}")),
    }
    // Come back where the person left: last project and each project's tab (per viewer, not
    // shared state; a missing or bad file just means the first project).
    let place = core.state.join("app-view.json");
    if let Ok(v) =
        std::fs::read(&place).map(|b| serde_json::from_slice::<Value>(&b).unwrap_or_default())
    {
        let machine = v["machine"].as_str().unwrap_or_default();
        for l in v["layouts"].as_array().into_iter().flatten() {
            let project = Id::new(
                l["machine"].as_str().unwrap_or_default(),
                l["project"].as_str().unwrap_or_default(),
            );
            if let Some(tree) = app.tree_from_json(l)
                && tree.len() > 1
            {
                app.layouts.insert(project, tree);
            }
        }
        app.set_project(v["project"].as_str().map(|p| Id::new(machine, p)));
        if let Some(tabs) = v["tabs"].as_object() {
            app.tab = tabs
                .iter()
                .filter_map(|(k, v)| Some((Id::new("", k), v.as_str()?.to_string())))
                .collect();
        }
        for t in v["remote_tabs"].as_array().into_iter().flatten() {
            if let [m, p, a] = &t.as_array().cloned().unwrap_or_default()[..]
                && let (Some(m), Some(p), Some(a)) = (m.as_str(), p.as_str(), a.as_str())
            {
                app.tab.insert(Id::new(m, p), a.to_string());
            }
        }
        if let Some(name) = v["theme"].as_str() {
            app.customs = crate::theme::custom::load_all().0;
            if let Some(c) = app.customs.iter().find(|c| c.name == name) {
                app.pal = c.palette;
                app.custom = Some(name.to_string());
            }
        }
    }
    if !warnings.is_empty() {
        app.say(format!("Shortcut config: {}", warnings.join("; ")));
    }
    let (tx, rx) = mpsc::channel::<Update>();
    let (req_tx, req_rx) = mpsc::channel::<()>();
    app.refresh = Some(req_tx);
    let observer = core.clone();
    thread::spawn(move || {
        loop {
            if tx
                .send(Update::from_list("", bridge::list(&observer)))
                .is_err()
            {
                return;
            }
            match req_rx.recv_timeout(Duration::from_millis(1500)) {
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                _ => while req_rx.try_recv().is_ok() {},
            }
        }
    });
    let shown = (|| -> Result<()> {
        let _guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        // Draw only what changed: an event, a new list, a different screen, or a notice running out.
        let (mut dirty, mut notice) = (true, false);
        let mut viewed: HashMap<String, Vec<String>> = HashMap::new();
        while !app.quit {
            while let Ok(u) = rx.try_recv() {
                app.apply(u);
                dirty = true;
            }
            dirty |= app.tick();
            dirty |= app.end_viewers(core);
            dirty |= app.capture_due(core);
            for machine in &app.machines {
                if machine.unusable().is_some() {
                    viewed.remove(&machine.id);
                    continue;
                }
                let runs: Vec<String> = app
                    .on_screen()
                    .iter()
                    .filter(|id| id.machine == machine.id)
                    .filter_map(|id| app.agents.iter().find(|a| a.id() == *id)?.run.clone())
                    .collect();
                if viewed.get(&machine.id) != Some(&runs) {
                    if machine.is_local() {
                        core.set_app_viewed(&runs)?;
                    } else {
                        let _ = machine.request(json!({"op":"view", "runs":runs}), None);
                    }
                    viewed.insert(machine.id.clone(), runs);
                }
            }
            let live = app.notice_live();
            dirty |= live != std::mem::replace(&mut notice, live);
            if std::mem::take(&mut dirty) {
                terminal.draw(|f| app.render(f))?;
            }
            let wait = app.due_in();
            let wait = if app.waiting() { wait.min(SOON) } else { wait };
            if event::poll(wait)? {
                // Everything already waiting goes in one go, so fast typing is one tmux call.
                loop {
                    app.handle(core, event::read()?);
                    if app.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
                app.place();
                dirty = true;
            }
            app.flush_typed_when_free(core);
        }
        Ok(())
    })();
    let _ = core.set_app_viewed(&[]);
    // Hand the windows back to tmux's own sizing, also after a draw or read error.
    // shortcut: a killed app (SIGHUP) leaves windows that had a client attached at manual; a
    // signal handler would need a new dependency. Reopening the app and leaving clears them.
    for pane in &app.touched {
        let _ = core.tm(&["set-option", "-wu", "-t", pane, "window-size"]);
    }
    // A reader without a cell would run unseen; those on this computer end with the app.
    // shortcut: a reader on another computer stays until that computer's app is opened and its
    // cell closed, or `pctl session remove`.
    for a in app
        .agents
        .iter()
        .filter(|a| a.viewer && a.machine.is_empty())
    {
        let _ = core.doc_close(&a.name);
    }
    shown?;
    println!("{}", quips::goodbye(app.keys.wit));
    let remote_tabs: Vec<Value> = app
        .tab
        .iter()
        .filter(|(p, _)| !p.machine.is_empty())
        .map(|(p, a)| json!([p.machine, p.name, a]))
        .collect();
    let tabs: HashMap<&str, &str> = app
        .tab
        .iter()
        .filter(|(p, _)| p.machine.is_empty())
        .map(|(p, a)| (p.name.as_str(), a.as_str()))
        .collect();
    let layouts = app.layouts_json();
    let _ = std::fs::write(
        &place,
        json!({
            "machine": app.project.as_ref().map(|p| &p.machine), "project": app.project.as_ref().map(|p| &p.name),
            "tabs": tabs, "remote_tabs": remote_tabs, "theme": app.custom, "layouts": layouts,
        })
        .to_string(),
    );
    Ok(())
}

impl App {
    fn new(theme: ThemeName, keys: Keymap) -> Self {
        Self {
            pal: theme.palette(),
            theme,
            custom: None,
            customs: vec![],
            keys,
            machines: vec![Mach::local(bridge::hostname())],
            agents: vec![],
            docs: vec![],
            doc_notified: HashMap::new(),
            ending: vec![],
            project: None,
            tab: HashMap::new(),
            collapsed: HashSet::new(),
            collapsed_machines: HashSet::new(),
            sidebar: true,
            sidebar_w: 26,
            in_sidebar: false,
            side_off: 0,
            side_max: 0,
            side_area: Rect::default(),
            side_seen: (false, 0, None),
            side_sel: 0,
            panes: HashMap::from([(0, Pane::default())]),
            next_pane: 1,
            tree: Tree::new(0),
            layouts: HashMap::new(),
            dragging: None,
            modal: None,
            prefix: false,
            notice: String::new(),
            notice_at: Instant::now(),
            hits: vec![],
            term: Rect::default(),
            drag: false,
            quit: false,
            refresh: None,
            fresh: HashMap::new(),
            touched: HashSet::new(),
            last_click: None,
            sel: None,
            notified: HashMap::new(),
            typed: None,
            tasks: vec![],
            reading: None,
        }
    }
    fn say(&mut self, text: impl Into<String>) {
        self.notice = text.into();
        self.notice_at = Instant::now();
    }
    /// Whether the status line still shows the last notice.
    fn notice_live(&self) -> bool {
        !self.notice.is_empty() && self.notice_at.elapsed() < Duration::from_secs(8)
    }
    /// Ask this computer's observer for a fresh list.
    fn poke(&self) {
        if let Some(r) = &self.refresh {
            let _ = r.send(());
        }
    }
    /// Ask that computer for a fresh list.
    fn poke_machine(&mut self, machine: &str) {
        match self.machines.iter_mut().find(|m| m.id == machine) {
            Some(m) if !m.is_local() => m.poke(),
            _ => self.poke(),
        }
    }

    fn mach(&self, machine: &str) -> Option<&Mach> {
        self.machines.iter().find(|m| m.id == machine)
    }
    /// Whether the sidebar shows machines: only when some are saved.
    fn multi(&self) -> bool {
        self.machines.len() > 1
    }
    fn local_connected(&self) -> bool {
        self.machines[0].connected
    }
    /// The machine's name as the person sees it.
    fn label(&self, machine: &str) -> String {
        self.mach(machine)
            .map_or(machine.to_string(), |m| m.label.clone())
    }
    /// Why that computer cannot be used right now: "<label> is down: …", or None.
    fn refused(&self, machine: &str) -> Option<String> {
        let m = self.mach(machine)?;
        m.unusable().map(|why| {
            if why == "connecting" {
                format!("{} is still connecting", m.label)
            } else {
                format!("{} is down: {why}", m.label)
            }
        })
    }
    /// Send one request to a computer; the refusal is said and None returned when it cannot be.
    fn request(
        &mut self,
        machine: &str,
        op: Value,
        within: Option<Duration>,
    ) -> Option<mpsc::Receiver<Result<Value>>> {
        if let Some(why) = self.refused(machine) {
            self.say(why);
            return None;
        }
        match self.mach(machine)?.request(op, within) {
            Ok(rx) => Some(rx),
            Err(why) => {
                self.say(why);
                None
            }
        }
    }

    /// Keep the other computers going and collect the answers that came in. Whether to redraw.
    fn tick(&mut self) -> bool {
        let mut dirty = false;
        let mut updates = vec![];
        for m in &mut self.machines {
            let (changed, update) = m.tick();
            dirty |= changed;
            updates.extend(update);
        }
        for u in updates {
            self.apply(u);
        }
        let mut done = vec![];
        let mut i = 0;
        while i < self.tasks.len() {
            match self.tasks[i].0.try_recv() {
                Err(mpsc::TryRecvError::Empty) => i += 1,
                answer => {
                    let (_, task) = self.tasks.remove(i);
                    done.push((
                        task,
                        answer.unwrap_or_else(|_| Err(anyhow::anyhow!("no answer"))),
                    ));
                }
            }
        }
        dirty |= !done.is_empty();
        for (task, answer) in done {
            self.finish(task, answer);
        }
        if let Some((a, rx)) = self.reading.take() {
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => self.reading = Some((a, rx)),
                read => {
                    self.image_read(
                        a,
                        read.unwrap_or_else(|_| Err(anyhow::anyhow!("no answer"))),
                    );
                    dirty = true;
                }
            }
        }
        dirty
    }
    /// Something is in flight, so the loop looks again soon.
    fn waiting(&self) -> bool {
        self.panes.values().any(|p| p.view.pending.is_some())
            || !self.tasks.is_empty()
            || self.reading.is_some()
            || self.machines.iter().any(Mach::waiting)
    }
    /// An answer for a task: nothing to say when it went well; why it did not otherwise.
    fn finish(&mut self, task: Task, answer: Result<Value>) {
        let failed = match &answer {
            Ok(v) if v["ok"] == json!(true) => None,
            Ok(v) => Some(v["error"].as_str().unwrap_or("failed").to_string()),
            Err(e) => Some(format!("{e:#}")),
        };
        let paste = matches!(task, Task::Paste(_));
        match task {
            Task::Image { agent, rest, kb } => match failed {
                Some(e) if e.starts_with("changed:") => self.changed(&agent),
                Some(e) => self.say(format!("Image not sent: {e}")),
                None => self.send_next(agent, rest, kb),
            },
            Task::Keys(a) | Task::Paste(a) => match failed {
                Some(e) if e.starts_with("changed:") => self.changed(&a),
                Some(e) if paste => self.say(format!("Could not paste: {e}")),
                Some(e) => self.say(format!("Could not type into {}: {e}", a.name)),
                None => {}
            },
            Task::Act { machine, ok, then } => {
                let form = matches!(then, Then::Form(..));
                match failed {
                    Some(e) => {
                        if form && let Some(Modal::Form(f)) = &mut self.modal {
                            f.busy = false;
                            f.error = e;
                        } else {
                            self.say(e);
                        }
                    }
                    None => {
                        if form {
                            self.modal = None;
                        }
                        self.say(ok);
                        self.after(then);
                    }
                }
                self.recapture();
                self.poke_machine(&machine);
            }
        }
    }

    /// How many of the pane's own rows a cell holds above the first drawn one.
    fn rows_skip(&self, id: u32) -> u16 {
        self.panes.get(&id).map_or(0, |p| {
            if p.view.scroll == 0 {
                p.view.size.1.saturating_sub(p.inner.height)
            } else {
                0
            }
        })
    }
    fn cell_at(&self, id: u32, column: u16, row: u16) -> term::Cell {
        let (inner, rows) = self
            .panes
            .get(&id)
            .map_or((Rect::default(), 0), |p| (p.inner, p.view.rows.len()));
        view_cell(inner, self.rows_skip(id), rows, column, row)
    }
    /// Copy what the drag covers to the person's clipboard: an OSC 52 write on our own stdout
    /// (most terminals turn it into a clipboard, tmux with `set-clipboard on` into a paste buffer)
    /// and, on this computer's own desktop, a clipboard command too, for terminals that ignore
    /// OSC 52. Over ssh that clipboard is not the person's. A plain click, or a drag over nothing,
    /// copies nothing.
    fn copy_selection(&mut self) {
        let Some((id, a, b)) = self.sel else {
            return;
        };
        if a == b {
            self.sel = None;
            return;
        }
        let text = self
            .panes
            .get(&id)
            .map(|p| term::selection_text(&p.view.rows, a, b))
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let count = text.chars().count();
        if text.len() >= clipboard::MAX_COPY {
            self.say(format!("Selection too long to copy ({count} characters)"));
            return;
        }
        write_out(&format!("\x1b]52;c;{}\x07", term::base64(text.as_bytes())));
        let local = std::env::var_os("SSH_CONNECTION").is_none();
        let command_ok = local && clipboard::write_text(&text).is_ok();
        self.say(clipboard::CopyWay::from_env(command_ok).status(count));
    }

    fn apply(&mut self, u: Update) {
        let Some(mi) = self.machines.iter().position(|m| m.id == u.machine) else {
            return;
        };
        // The statuses this app watched before this update, for the notifications below.
        let before: HashMap<Id, String> = self
            .agents
            .iter()
            .filter(|a| a.machine == u.machine)
            .map(|a| (a.id(), a.status.clone()))
            .collect();
        let doc_before: Vec<DocRow> = self
            .docs
            .iter()
            .filter(|d| d.machine == u.machine)
            .cloned()
            .collect();
        if let Some(docs) = u.docs {
            self.docs.retain(|d| d.machine != u.machine);
            self.docs.extend(docs);
        }
        let watched = self.machines[mi].loaded;
        self.machines[mi].loaded = true;
        self.machines[mi].connected = u.connected;
        self.machines[mi].projects = u.projects;
        match u.agents {
            Ok(rows) => {
                self.agents.retain(|a| a.machine != u.machine);
                self.agents.extend(rows.iter().map(|s| Agent {
                    machine: u.machine.clone(),
                    name: s["name"].as_str().unwrap_or_default().into(),
                    project: s["project"].as_str().unwrap_or_default().into(),
                    status: s["status"].as_str().unwrap_or("unknown").into(),
                    stale: s["stale"] == true,
                    pane: s["pane"].as_str().map(String::from),
                    run: s["run_id"].as_str().map(String::from),
                    queued: s["tell_queue"].as_u64().unwrap_or(0) as usize,
                    viewer: s["viewer"].as_bool().unwrap_or(false),
                }));
            }
            Err(e) => {
                let label = self.machines[mi].label.clone();
                self.say(if self.multi() {
                    format!("Could not read sessions on {label}: {e}")
                } else {
                    format!("Could not read sessions: {e}")
                })
            }
        }
        self.fresh.retain(|id, (_, at)| {
            at.elapsed() < Duration::from_secs(10) && !self.agents.iter().any(|a| a.id() == *id)
        });
        self.agents.extend(
            self.fresh
                .values()
                .filter(|(a, _)| a.machine == u.machine)
                .map(|(a, _)| a.clone()),
        );
        let names: Vec<String> = self.machines[mi]
            .projects
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        // The project on screen is gone: from a machine no longer listed, or from its list.
        if self.project.as_ref().is_some_and(|p| {
            self.mach(&p.machine).is_none() || (p.machine == u.machine && !names.contains(&p.name))
        }) {
            self.set_project(None);
        }
        if self.project.is_none() {
            let running = |m: &Mach| {
                m.projects
                    .iter()
                    .find(|(p, _)| {
                        self.agents
                            .iter()
                            .any(|a| a.machine == m.id && &a.project == p && a.running())
                    })
                    .or(m.projects.first())
                    .map(|(p, _)| Id::new(&m.id, p))
            };
            let first = self.machines.iter().find_map(running);
            self.set_project(first);
        }
        for p in &names {
            let pid = Id::new(&u.machine, p);
            let mine = self.project_agents(&pid);
            let keep = self
                .tab
                .get(&pid)
                .is_some_and(|t| mine.iter().any(|a| &a.name == t));
            let pick = mine
                .iter()
                .find(|a| a.running())
                .or(mine.first())
                .map(|a| a.name.clone());
            if !keep {
                match pick {
                    Some(name) => self.tab.insert(pid, name),
                    None => self.tab.remove(&pid),
                };
            }
        }
        self.settle();
        self.notified
            .retain(|id, _| self.agents.iter().any(|a| a.id() == *id));
        if watched {
            let shown = self.current().map(Agent::id);
            // An agent in any cell on screen is being looked at, not only the focused one.
            let seen = self.on_screen();
            let now: Vec<Agent> = self
                .agents
                .iter()
                .filter(|a| {
                    !a.viewer
                        && a.machine == u.machine
                        && (!seen.contains(&a.id()) || shown == Some(a.id()))
                })
                .cloned()
                .collect();
            for (id, text, bell) in notifications(&before, &now, shown.as_ref()) {
                let at = Instant::now();
                if may_notify(self.notified.get(&id).copied(), at) {
                    self.notified.insert(id.clone(), at);
                    let name = if id.machine.is_empty() {
                        id.name.clone()
                    } else {
                        format!("{}/{}", self.label(&id.machine), id.name)
                    };
                    write_out(&notice_sequence(&name, text, bell));
                    // Terminals that ignore OSC 777 (MATE Terminal) get a desktop notice instead.
                    crate::notify::desktop(&notice_body(&name, text));
                }
            }
        }
        // A document that arrived is announced; nothing opens and no focus moves.
        let now: Vec<DocRow> = self
            .docs
            .iter()
            .filter(|d| d.machine == u.machine)
            .cloned()
            .collect();
        for d in docs::arrivals(&doc_before, &now, watched) {
            let at = Instant::now();
            let key = Id::new(&d.machine, &d.id);
            if may_notify(self.doc_notified.get(&key).copied(), at) {
                self.doc_notified.insert(key, at);
                let who = d.by.as_deref().unwrap_or("pctl");
                let who = if d.machine.is_empty() {
                    who.to_string()
                } else {
                    format!("{}/{who}", self.label(&d.machine))
                };
                let text = format!("wants you to read {}", sanitize(&d.title));
                write_out(&notice_sequence(&who, &text, false));
                crate::notify::desktop(&notice_body(&who, &text));
            }
        }
        self.doc_notified.retain(|k, _| {
            self.docs
                .iter()
                .any(|d| d.machine == k.machine && d.id == k.name)
        });
    }
    /// The agent in the focused cell.
    fn current(&self) -> Option<&Agent> {
        self.agent_in(self.focused())
    }
    fn agent_in(&self, id: u32) -> Option<&Agent> {
        let want = self.panes.get(&id)?.agent.as_ref()?;
        self.agents
            .iter()
            .find(|a| a.machine == want.machine && a.name == want.name)
    }
    fn project_agents(&self, project: &Id) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|a| a.machine == project.machine && a.project == project.name && !a.viewer)
            .collect()
    }

    // ---- cells ----

    fn focused(&self) -> u32 {
        *self.tree.focused()
    }
    /// The cells on screen now, in reading order.
    fn visible(&self) -> Vec<u32> {
        let leaves = self.tree.leaves();
        self.tree
            .shown()
            .into_iter()
            .filter_map(|i| leaves.get(i).map(|id| **id))
            .collect()
    }
    /// The agents in the cells on screen.
    fn on_screen(&self) -> Vec<Id> {
        self.visible()
            .iter()
            .filter_map(|id| self.panes.get(id)?.agent.clone())
            .collect()
    }
    /// The cell on screen that shows this agent.
    fn pane_of(&self, agent: &Id) -> Option<u32> {
        self.visible().into_iter().find(|id| {
            self.panes
                .get(id)
                .is_some_and(|p| p.agent.as_ref() == Some(agent))
        })
    }
    fn view(&mut self, id: u32) -> &mut View {
        &mut self.panes.entry(id).or_default().view
    }
    fn new_pane(&mut self, agent: Option<Id>) -> u32 {
        let id = self.next_pane;
        self.next_pane += 1;
        self.panes.insert(
            id,
            Pane {
                agent,
                ..Pane::default()
            },
        );
        id
    }
    /// Show another project: its cells come back as they were, or it starts with one.
    fn set_project(&mut self, project: Option<Id>) {
        if self.project == project {
            return;
        }
        let fresh = match project.as_ref().and_then(|p| self.layouts.remove(p)) {
            Some(tree) => tree,
            None => Tree::new(self.new_pane(None)),
        };
        let old = std::mem::replace(&mut self.tree, fresh);
        if let Some(p) = self.project.take() {
            self.layouts.insert(p, old);
        }
        self.project = project;
        self.sel = None;
        self.dragging = None;
        self.settle();
        self.recapture();
    }
    /// A single cell shows the project's selected tab, like the app did before it had cells: it
    /// is filled when empty and when its agent has left. With several cells nothing is filled in.
    fn settle(&mut self) {
        if self.tree.len() != 1 {
            return;
        }
        let id = self.focused();
        let there = self
            .panes
            .get(&id)
            .and_then(|p| p.agent.as_ref())
            .is_some_and(|a| {
                self.agents
                    .iter()
                    .any(|x| x.machine == a.machine && x.name == a.name)
            });
        if there {
            return;
        }
        let tab = self.project.as_ref().and_then(|p| {
            let name = self.tab.get(p)?;
            Some(Id::new(&p.machine, name))
        });
        if self.panes.get(&id).is_some_and(|p| p.agent != tab) {
            let pane = self.panes.entry(id).or_default();
            pane.agent = tab;
            pane.view.next = None;
        }
    }
    /// Read the screens again at once.
    fn recapture(&mut self) {
        for p in self.panes.values_mut() {
            p.view.next = None;
        }
    }
    /// The areas of the cells on screen, from the terminal area.
    fn place(&mut self) {
        if self.term.width == 0 {
            return;
        }
        let titled = self.tree.titled();
        let leaves: Vec<u32> = self.tree.leaves().into_iter().copied().collect();
        for (i, outer) in self.tree.geometry(self.term).cells {
            let Some(id) = leaves.get(i) else {
                continue;
            };
            let inner = if titled {
                Rect {
                    y: outer.y + 1,
                    height: outer.height.saturating_sub(1),
                    ..outer
                }
            } else {
                outer
            };
            let pane = self.panes.entry(*id).or_default();
            pane.outer = outer;
            pane.inner = inner;
        }
    }
    /// Cells changed: place them and read what their sizes now ask for.
    fn laid_out(&mut self) {
        self.sel = None;
        self.place();
        self.recapture();
    }
    fn focus_index(&mut self, i: usize) {
        if i < self.tree.len() {
            self.tree.focus = i;
            self.sel = None;
            self.in_sidebar = false;
        }
    }
    fn focus_pane(&mut self, id: u32) {
        if let Some(i) = self.tree.leaves().iter().position(|p| **p == id) {
            self.focus_index(i);
        }
    }
    fn split(&mut self, dir: Dir) {
        if self.project.is_none() {
            return;
        }
        let id = self.new_pane(None);
        if self.tree.split(dir, id) {
            self.laid_out();
        } else {
            self.panes.remove(&id);
            self.say("No room for more cells");
        }
    }
    fn close_cell(&mut self) {
        let leaves: Vec<u32> = self.tree.leaves().into_iter().copied().collect();
        match self.tree.close(self.tree.focus) {
            Some(id) => {
                // Only the view goes; the agent keeps running in its tmux window.
                let gone = self.panes.remove(&id).and_then(|p| p.agent);
                self.release(gone);
                self.laid_out();
            }
            None if leaves.len() < 2 => self.say("The last cell cannot be closed"),
            None => {}
        }
    }
    /// A cell stopped showing this: a document's reader is ended (the document stays), an agent
    /// keeps running.
    fn release(&mut self, agent: Option<Id>) {
        if let Some(v) = agent
            && self.agents.iter().any(|a| a.id() == v && a.viewer)
        {
            self.ending.push(v);
        }
    }
    /// End the readers whose cells are gone; nothing is left running unseen. Whether anything went.
    fn end_viewers(&mut self, core: &Core) -> bool {
        let ending = std::mem::take(&mut self.ending);
        for v in &ending {
            self.act_on(
                core,
                &v.machine,
                json!({"action": "doc_close", "agent": v.name}),
                String::new(),
                Then::Nothing,
            );
        }
        !ending.is_empty()
    }
    fn move_focus(&mut self, toward: Side) {
        let cells = self.tree.areas(self.term);
        if let Some(i) = Tree::<u32>::neighbor(&cells, self.tree.focus, toward) {
            self.focus_index(i);
        }
    }
    fn resize_cell(&mut self, toward: Side) {
        if self.tree.resize(toward, layout::STEP) {
            self.laid_out();
        }
    }
    fn swap_cell(&mut self, forward: bool) {
        let n = self.tree.len();
        if n < 2 {
            return;
        }
        let from = self.tree.focus;
        let to = if forward {
            (from + 1) % n
        } else {
            (from + n - 1) % n
        };
        if self.tree.swap(from, to) {
            self.laid_out();
        }
    }
    fn zoom_cell(&mut self) {
        if self.tree.zoom() {
            self.laid_out();
        } else {
            self.say("Only one cell to show");
        }
    }

    /// Put an agent in a cell: the one that already shows it, else the focused one.
    fn show_agent(&mut self, id: &Id) {
        let shown = self.tree.leaves().iter().position(|p| {
            self.panes
                .get(*p)
                .is_some_and(|x| x.agent.as_ref() == Some(id))
        });
        match shown {
            Some(i) => self.focus_index(i),
            None => {
                let pane = self.panes.entry(self.focused()).or_default();
                let old = pane.agent.replace(id.clone());
                pane.view.next = None;
                self.release(old);
            }
        }
    }
    fn open_agent(&mut self, id: &Id) {
        if let Some(a) = self
            .agents
            .iter()
            .find(|a| a.machine == id.machine && a.name == id.name)
            .cloned()
        {
            let project = a.project_id();
            self.collapsed.remove(&project);
            self.tab.insert(project.clone(), a.name.clone());
            // With one cell the project follows the agent, as it always did; with several, the
            // agent comes into the focused cell and the project stays.
            if self.tree.len() == 1 {
                self.set_project(Some(project));
            }
            self.show_agent(&a.id());
            self.sel = None;
        }
    }

    /// The layouts of the projects as the file keeps them: only the ones with several cells.
    fn layouts_json(&self) -> Vec<Value> {
        let mut out = vec![];
        let shown = self.project.iter().map(|p| (p, &self.tree));
        for (p, tree) in self.layouts.iter().chain(shown) {
            if tree.len() < 2 {
                continue;
            }
            let mut v = tree.to_json(&|id| self.agent_json(*id));
            v["machine"] = json!(p.machine);
            v["project"] = json!(p.name);
            out.push(v);
        }
        out
    }
    fn agent_json(&self, id: u32) -> Value {
        match self.panes.get(&id).and_then(|p| p.agent.as_ref()) {
            Some(a) => json!({"machine": a.machine, "agent": a.name}),
            None => json!({}),
        }
    }
    /// A layout from the file, with a cell for each leaf. An agent in two cells is kept in the first.
    fn tree_from_json(&mut self, v: &Value) -> Option<Tree<u32>> {
        let mut seen: HashSet<Id> = HashSet::new();
        let mut made = vec![];
        let tree = Tree::from_json(v, &mut |leaf| {
            let agent = leaf["agent"]
                .as_str()
                .map(|n| Id::new(leaf["machine"].as_str().unwrap_or_default(), n))
                .filter(|a| seen.insert(a.clone()));
            let id = self.next_pane;
            self.next_pane += 1;
            made.push((id, agent));
            Some(id)
        });
        for (id, agent) in made {
            self.panes.insert(
                id,
                Pane {
                    agent,
                    ..Pane::default()
                },
            );
        }
        tree
    }

    /// Show nothing for the agent (its pane is gone, holds another run, or its computer is down).
    /// Whether that changed the screen.
    fn blank(&mut self, id: u32) -> bool {
        let view = self.view(id);
        view.last = None;
        view.pending = None;
        !std::mem::take(&mut view.rows).is_empty()
    }
    /// All the cells that are due.
    fn capture_due(&mut self, core: &Core) -> bool {
        let mut dirty = false;
        for id in self.visible() {
            if self.view(id).next.is_none_or(|t| Instant::now() >= t) {
                dirty |= self.capture(core, id);
            }
        }
        dirty
    }
    /// How long until the first cell is due.
    fn due_in(&self) -> Duration {
        self.visible()
            .iter()
            .map(|id| {
                self.panes
                    .get(id)
                    .and_then(|p| p.view.next)
                    .map_or(Duration::ZERO, |t| {
                        t.saturating_duration_since(Instant::now())
                    })
            })
            .min()
            .unwrap_or(Duration::ZERO)
    }
    /// Read a cell's screen and schedule its next read. Whether the screen changed.
    fn capture(&mut self, core: &Core, id: u32) -> bool {
        let Some(changed) = self.read_screen(core, id) else {
            // The answer from another computer is on its way.
            self.view(id).next = Some(Instant::now() + SOON);
            return false;
        };
        let view = self.view(id);
        view.quiet = if changed { 0 } else { view.quiet + 1 };
        view.next = Some(Instant::now() + capture_every(view.quiet));
        changed
    }
    /// Read a cell's agent's screen, resizing its window to the cell first. Whether the screen
    /// changed; None while the answer of another computer is still to come.
    fn read_screen(&mut self, core: &Core, id: u32) -> Option<bool> {
        let Some(a) = self.agent_in(id).filter(|a| a.running()).cloned() else {
            self.view(id).owner = Default::default();
            return Some(self.blank(id));
        };
        let (Some(pane), Some(run)) = (a.pane.clone(), a.run.clone()) else {
            return Some(self.blank(id));
        };
        let owner = (a.machine.clone(), pane.clone());
        if self.view(id).owner != owner {
            *self.view(id) = View {
                owner,
                ..View::default()
            };
        }
        let inner = self.panes.get(&id).map_or(Rect::default(), |p| p.inner);
        let (w, h) = (inner.width, inner.height);
        if w < 2 || h < 2 {
            return Some(false);
        }
        if !a.machine.is_empty() {
            return self.read_remote(id, &a, &run, (w, h));
        }
        if self.view(id).asked != (w, h) {
            // The pane comes from the last list; check it still holds this run before resizing it.
            // The read below checks it again, in its own call.
            if bridge::verified(core, &pane, &run).is_err() {
                self.poke();
                return Some(self.blank(id));
            }
            self.view(id).asked = (w, h);
            self.touched.insert(pane.clone());
            let _ = bridge::resize(core, &pane, w, h);
        }
        let scroll = self.view(id).scroll;
        Some(match bridge::read_screen(core, &pane, &run, h, scroll) {
            Ok(Some(s)) => self.show(id, s),
            Ok(None) => {
                self.poke();
                self.blank(id)
            }
            Err(_) => false,
        })
    }
    /// The screen of an agent on another computer: one request at a time, the answer taken up on a
    /// later pass. The bridge there does the resizing.
    fn read_remote(&mut self, id: u32, a: &Agent, run: &str, (w, h): (u16, u16)) -> Option<bool> {
        if self.refused(&a.machine).is_some() {
            return Some(self.blank(id));
        }
        if let Some(rx) = self.view(id).pending.take() {
            return match rx.try_recv() {
                Ok(Ok(v)) if v["ok"] == json!(true) => Some(self.show(id, Screen::from_answer(&v))),
                Ok(Ok(v))
                    if v["error"]
                        .as_str()
                        .is_some_and(|e| e.starts_with("changed:")) =>
                {
                    self.poke_machine(&a.machine);
                    Some(self.blank(id))
                }
                Ok(_) | Err(mpsc::TryRecvError::Disconnected) => Some(false),
                Err(mpsc::TryRecvError::Empty) => {
                    self.view(id).pending = Some(rx);
                    None
                }
            };
        }
        let op = json!({
            "op": "screen", "agent": a.name, "run": run,
            "width": w, "height": h, "scroll": self.view(id).scroll,
        });
        let pending = self.request(&a.machine, op, None);
        self.view(id).pending = pending;
        None
    }
    /// Take a screen as a cell's view. Whether it differs from the one shown.
    fn show(&mut self, id: u32, s: Screen) -> bool {
        let inner_h = self.panes.get(&id).map_or(0, |p| p.inner.height);
        let view = self.view(id);
        if view.last.as_ref() == Some(&s) {
            return false;
        }
        view.cursor = s.cursor;
        view.size = s.size;
        view.history = s.history;
        (view.alt, view.mouse, view.sgr) = (s.alt, s.mouse, s.sgr);
        view.scroll = view.scroll.min(view.max_scroll(inner_h));
        view.rows = term::parse_capture(&s.text);
        view.last = Some(s);
        true
    }

    /// Something was typed: the echo is on its way, so look at once and often.
    fn read_soon(&mut self, a: &Agent) {
        let id = self.pane_of(&a.id()).unwrap_or_else(|| self.focused());
        let view = self.view(id);
        view.next = Some(Instant::now());
        view.quiet = 0;
    }
    fn changed(&mut self, a: &Agent) {
        self.say(format!(
            "{} changed (restarted or moved); not typing until the list catches up",
            a.name
        ));
        self.poke_machine(&a.machine);
    }
    fn send_key(&mut self, core: &Core, k: &KeyEvent) {
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            return;
        };
        let Some((lit, key)) = term::key_args(k) else {
            return;
        };
        self.queue(core, a, lit, key);
    }
    /// Queue input for the agent shown; the main loop sends the queue after each batch of events.
    fn queue(&mut self, core: &Core, a: Agent, lit: bool, key: String) {
        if let Some(why) = self.refused(&a.machine) {
            return self.say(format!("{why}; not typing"));
        }
        if self.typed.as_ref().is_some_and(|(t, _)| {
            (&t.machine, &t.name, &t.pane, &t.run) != (&a.machine, &a.name, &a.pane, &a.run)
        }) {
            self.flush_typed(core);
        }
        let shown = self.pane_of(&a.id());
        let (_, parts) = self.typed.get_or_insert_with(|| (a, vec![]));
        match parts.last_mut() {
            Some((true, text)) if lit => text.push_str(&key),
            _ => parts.push((lit, key)),
        }
        if let Some(id) = shown {
            self.view(id).scroll = 0;
        }
        let Some((_, parts)) = &self.typed else {
            return;
        };
        if parts.iter().map(|(_, k)| k.len()).sum::<usize>() > TYPED_MAX {
            self.flush_typed(core);
        }
    }
    /// The flush after each batch of events. Keys for another computer wait while that agent's last
    /// keys are on their way, so a burst (a trackpad's wheel) is one request, not a queue of them.
    fn flush_typed_when_free(&mut self, core: &Core) {
        let busy = self.typed.as_ref().is_some_and(|(a, _)| {
            !a.machine.is_empty()
                && self
                    .tasks
                    .iter()
                    .any(|(_, t)| matches!(t, Task::Keys(b) | Task::Paste(b) if b.id() == a.id()))
        });
        if !busy {
            self.flush_typed(core);
        }
    }
    /// Send the typed keys: one call checks the run and types them all.
    fn flush_typed(&mut self, core: &Core) {
        let Some((a, parts)) = self.typed.take() else {
            return;
        };
        let (Some(pane), Some(run)) = (&a.pane, &a.run) else {
            return self.changed(&a);
        };
        if !a.machine.is_empty() {
            return self.remote_keys(a.clone(), run, &parts);
        }
        match bridge::type_keys(core, pane, run, &parts) {
            Ok(true) => {}
            Ok(false) => self.changed(&a),
            Err(e) => self.say(format!("Could not type into {}: {e:#}", a.name)),
        }
        self.read_soon(&a);
    }
    /// Type the keys into an agent of another computer; the answer comes back as a task.
    fn remote_keys(&mut self, a: Agent, run: &str, parts: &[(bool, String)]) {
        let keys: Vec<Value> = parts
            .iter()
            .map(|(literal, key)| json!({"literal": literal, "key": key}))
            .collect();
        let op = json!({"op": "keys", "agent": a.name, "run": run, "keys": keys});
        if let Some(rx) = self.request(&a.machine, op, None) {
            self.tasks.push((rx, Task::Keys(a.clone())));
        }
        self.read_soon(&a);
    }
    /// The image-paste key. An agent here sees this clipboard itself, so it only gets the key. An
    /// agent on another computer gets the clipboard's image first, when there is one.
    fn image_paste(&mut self, core: &Core) {
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            return;
        };
        if a.machine.is_empty() {
            return self.queue(core, a, false, "C-v".into());
        }
        if self.reading.is_some()
            || self
                .tasks
                .iter()
                .any(|(_, t)| matches!(t, Task::Image { .. }))
        {
            return self.say("Still sending the last image");
        }
        if let Some(why) = self.refused(&a.machine) {
            return self.say(format!("{why}; not pasting"));
        }
        let (tx, rx) = mpsc::channel();
        // Its own thread: the clipboard command may be slow, and the drawing must not wait for it.
        thread::spawn(move || {
            let _ = tx.send(clipboard::read_image());
        });
        self.reading = Some((a, rx));
    }
    /// The local clipboard was read for an agent of another computer.
    fn image_read(&mut self, a: Agent, read: Result<Option<Vec<u8>>>) {
        let Some(run) = a.run.clone() else {
            return self.changed(&a);
        };
        match read {
            Err(e) => self.say(format!("Image not sent: {e:#}")),
            Ok(None) => self.remote_keys(a, &run, &[(false, "C-v".into())]),
            Ok(Some(png)) => {
                let kb = png.len().div_ceil(1024);
                self.send_next(a.clone(), clip_ops(&a.name, &run, &png), kb);
            }
        }
    }
    /// Send the next request of an image on its way. The key goes last, only after the clipboard
    /// there was set, and ends the chain.
    fn send_next(&mut self, a: Agent, mut rest: VecDeque<Value>, kb: usize) {
        let Some(op) = rest.pop_front() else {
            return;
        };
        let key = op["op"] == "keys";
        if key {
            self.say(format!(
                "Sent image ({kb} KB) to {}",
                self.label(&a.machine)
            ));
        }
        if let Some(rx) = self.request(&a.machine, op, Some(ACT_WITHIN)) {
            let task = if key {
                Task::Keys(a.clone())
            } else {
                Task::Image {
                    agent: a.clone(),
                    rest,
                    kb,
                }
            };
            self.tasks.push((rx, task));
        }
        self.read_soon(&a);
    }
    fn paste(&mut self, core: &Core, text: &str) {
        self.flush_typed(core);
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            return;
        };
        let (Some(pane), Some(run)) = (&a.pane, &a.run) else {
            return self.changed(&a);
        };
        if !a.machine.is_empty() {
            if text.len() > REMOTE_PASTE_MAX {
                return self.say(format!(
                    "Could not paste: {} characters is too much to send to another computer",
                    text.chars().count()
                ));
            }
            let op = json!({"op": "paste", "agent": a.name, "run": run, "text": text});
            if let Some(rx) = self.request(&a.machine, op, None) {
                self.tasks.push((rx, Task::Paste(a.clone())));
            }
            return self.read_soon(&a);
        }
        match bridge::paste_text(core, pane, run, text) {
            Ok(true) => {}
            Ok(false) => self.changed(&a),
            Err(e) => self.say(format!("Could not paste: {e:#}")),
        }
        self.read_soon(&a);
    }

    fn handle(&mut self, core: &Core, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(core, k),
            Event::Mouse(m) => self.mouse(core, m),
            Event::Paste(text) if self.modal.is_none() => self.paste(core, &text),
            Event::Paste(text) => {
                if let Some(Modal::Form(f)) = &mut self.modal {
                    let i = f.focus;
                    if f.fields[i].choices.is_empty() {
                        f.fields[i].value.push_str(text.trim_end_matches('\n'));
                    }
                }
            }
            Event::Resize(..) => {
                for p in self.panes.values_mut() {
                    p.view.asked = (0, 0);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, core: &Core, k: KeyEvent) {
        self.sel = None;
        if self.modal.is_some() {
            return self.modal_key(core, k);
        }
        if self.prefix {
            self.prefix = false;
            if self.keys.prefix.matches(&k) {
                return self.send_key(core, &k);
            }
            if let KeyCode::Char(d @ '1'..='9') = k.code {
                let n = d as usize - '1' as usize;
                let tabs: Vec<Id> = self
                    .project
                    .as_ref()
                    .map(|p| self.project_agents(p).iter().map(|a| a.id()).collect())
                    .unwrap_or_default();
                if let Some(id) = tabs.get(n) {
                    self.open_agent(&id.clone());
                }
                return;
            }
            if k.code == KeyCode::Esc {
                return;
            }
            match self.keys.action(true, &k) {
                Some(a) => self.action(core, a),
                None => self.say(format!(
                    "No shortcut on {} — {} shows them",
                    keys::Key {
                        code: k.code,
                        mods: k.modifiers
                    }
                    .label(),
                    self.keys.label(Action::Help)
                )),
            }
            return;
        }
        if self.keys.prefix.matches(&k) {
            self.prefix = true;
            return;
        }
        if let Some(a) = self.keys.action(false, &k) {
            return self.action(core, a);
        }
        if self.in_sidebar {
            return self.sidebar_key(core, k);
        }
        self.send_key(core, &k);
    }

    fn action(&mut self, core: &Core, a: Action) {
        self.flush_typed(core);
        let current = self.current().map(Agent::id);
        match a {
            Action::NewTab => match self.project.clone() {
                Some(p) => self.new_tab(core, &p),
                None => self.project_form(),
            },
            Action::NewAgent => match self.project.clone() {
                Some(p) => self.agent_form(&p),
                None => self.project_form(),
            },
            Action::NewProject => self.project_form(),
            Action::NextTab | Action::PrevTab => {
                let Some(p) = self.project.clone() else {
                    return;
                };
                let tabs: Vec<Id> = self.project_agents(&p).iter().map(|a| a.id()).collect();
                if tabs.is_empty() {
                    return;
                }
                let i = current
                    .and_then(|c| tabs.iter().position(|t| *t == c))
                    .unwrap_or(0);
                let n = tabs.len();
                let j = if a == Action::NextTab {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                self.open_agent(&tabs[j]);
            }
            Action::AgentMenu => {
                if let Some(c) = current {
                    let x = if self.sidebar { self.sidebar_w + 1 } else { 0 };
                    self.agent_menu(&c, x, 1);
                }
            }
            Action::StopAgent => {
                if let Some(c) = current {
                    self.confirm_stop(&c);
                }
            }
            Action::RestartAgent => {
                if let Some(c) = current {
                    self.run_act(core, Act::Restart(c));
                }
            }
            Action::ToggleSidebar => self.sidebar = !self.sidebar,
            Action::FocusSidebar => {
                self.sidebar = true;
                self.in_sidebar = true;
                let rows = self.side_rows();
                self.side_sel = current
                    .and_then(|c| {
                        rows.iter()
                            .position(|r| matches!(r, SideRow::Agent(n) if *n == c))
                    })
                    .unwrap_or(0);
            }
            Action::Theme => {
                let (w, h) = (
                    self.term.x + self.term.width,
                    self.term.y + self.term.height,
                );
                self.theme_menu(w.saturating_sub(70), h.saturating_sub(10));
            }
            Action::Help => self.modal = Some(Modal::Help),
            Action::Detach => self.quit = true,
            Action::ImagePaste if !self.in_sidebar => self.image_paste(core),
            Action::ImagePaste => {}
            Action::SplitRight => self.split(Dir::Right),
            Action::SplitDown => self.split(Dir::Down),
            Action::CloseCell => self.close_cell(),
            Action::FocusLeft => self.move_focus(Side::Left),
            Action::FocusRight => self.move_focus(Side::Right),
            Action::FocusUp => self.move_focus(Side::Up),
            Action::FocusDown => self.move_focus(Side::Down),
            Action::ResizeLeft => self.resize_cell(Side::Left),
            Action::ResizeRight => self.resize_cell(Side::Right),
            Action::ResizeUp => self.resize_cell(Side::Up),
            Action::ResizeDown => self.resize_cell(Side::Down),
            Action::SwapPrev => self.swap_cell(false),
            Action::SwapNext => self.swap_cell(true),
            Action::Zoom => self.zoom_cell(),
        }
    }

    fn side_rows(&self) -> Vec<SideRow> {
        let mut rows = vec![];
        for m in &self.machines {
            if self.multi() {
                rows.push(SideRow::Machine(m.id.clone()));
                if self.collapsed_machines.contains(&m.id) {
                    continue;
                }
            }
            if let Some(why) = m.unusable().filter(|_| !m.is_local()) {
                rows.push(SideRow::Note(if why == "connecting" {
                    "connecting…".into()
                } else {
                    format!("down: {why}")
                }));
            } else if self.multi() && m.is_local() && m.loaded && !m.connected {
                rows.push(SideRow::Note("no pctl server here".into()));
            }
            for (p, _) in &m.projects {
                let pid = Id::new(&m.id, p);
                rows.push(SideRow::Project(pid.clone()));
                if !self.collapsed.contains(&pid) {
                    for a in self.project_agents(&pid) {
                        rows.push(SideRow::Agent(a.id()));
                    }
                    // shortcut: only the newest few; older ones wait for a document list of their own.
                    for d in self
                        .docs
                        .iter()
                        .filter(|d| d.machine == m.id && &d.project == p)
                        .take(DOCS_PER_PROJECT)
                    {
                        rows.push(SideRow::Doc(Id::new(&d.machine, &d.id)));
                    }
                    rows.push(SideRow::Add(pid));
                }
            }
        }
        rows
    }
    fn sidebar_key(&mut self, core: &Core, k: KeyEvent) {
        let rows = self.side_rows();
        match k.code {
            KeyCode::Up => self.side_sel = self.side_sel.saturating_sub(1),
            KeyCode::Down => self.side_sel = (self.side_sel + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Esc | KeyCode::Right => self.in_sidebar = false,
            KeyCode::Left => match rows.get(self.side_sel) {
                Some(SideRow::Project(p)) => {
                    self.collapsed.insert(p.clone());
                }
                Some(SideRow::Machine(m)) => {
                    self.collapsed_machines.insert(m.clone());
                }
                _ => {}
            },
            KeyCode::Enter => match rows.get(self.side_sel).cloned() {
                Some(SideRow::Machine(m)) => self.click_machine(&m),
                Some(SideRow::Project(p)) => self.click_project(&p),
                Some(SideRow::Agent(a)) => {
                    self.open_agent(&a);
                    self.in_sidebar = false;
                }
                Some(SideRow::Doc(d)) => {
                    self.open_doc(core, &d);
                    self.in_sidebar = false;
                }
                Some(SideRow::Add(p)) => self.agent_form(&p),
                Some(SideRow::Note(_)) | None => {}
            },
            _ => {}
        }
    }
    fn click_project(&mut self, p: &Id) {
        if self.project.as_ref() == Some(p) && !self.collapsed.contains(p) {
            self.collapsed.insert(p.clone());
        } else {
            self.collapsed.remove(p);
            self.set_project(Some(p.clone()));
            self.recapture();
        }
    }
    fn click_machine(&mut self, m: &str) {
        if !self.collapsed_machines.remove(m) {
            self.collapsed_machines.insert(m.to_string());
        }
    }

    fn mouse(&mut self, core: &Core, m: MouseEvent) {
        // A wheel tick joins the keys before it (it goes to the same agent or flushes them in
        // `queue`); anything else sends them first.
        if !matches!(
            m.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            self.flush_typed(core);
        }
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| {
                m.column >= r.x
                    && m.column < r.x + r.width
                    && m.row >= r.y
                    && m.row < r.y + r.height
            })
            .map(|(_, h)| h.clone());
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.sel = None; // a click anywhere else clears the last selection
                let in_modal = matches!(
                    hit,
                    Some(
                        Hit::Menu(_) | Hit::Field(_) | Hit::Choice(..) | Hit::Submit | Hit::Cancel
                    )
                );
                match self.modal {
                    // An open box takes every click: nothing behind it reacts.
                    Some(Modal::Menu { .. } | Modal::Help) if !in_modal => self.modal = None,
                    Some(_) if !in_modal => {}
                    _ => {
                        if let Some(h) = hit {
                            // A double-click on an agent or its tab opens its menu, like a
                            // right-click: some terminals (MATE Terminal) keep the right button.
                            let double = matches!(h, Hit::Agent(_) | Hit::Tab(_))
                                && self.last_click.as_ref().is_some_and(|(t, last)| {
                                    *last == h && t.elapsed() < Duration::from_millis(400)
                                });
                            self.last_click = (!double).then(|| (Instant::now(), h.clone()));
                            match h {
                                Hit::Agent(a) | Hit::Tab(a) if double => {
                                    self.agent_menu(&a, m.column, m.row)
                                }
                                // A press in a cell focuses it and starts a selection there; the
                                // drag below extends it and the release copies it.
                                Hit::Pane(id) => {
                                    self.focus_pane(id);
                                    let at = self.cell_at(id, m.column, m.row);
                                    self.sel = Some((id, at, at));
                                }
                                Hit::Title(id) => self.focus_pane(id),
                                Hit::Divider(d) => self.dragging = Some((d, false)),
                                h => self.click(core, h),
                            }
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.modal.is_none() => match hit {
                Some(Hit::Agent(a) | Hit::Tab(a)) => self.agent_menu(&a, m.column, m.row),
                Some(Hit::Project(p) | Hit::NewAgent(p) | Hit::NewTab(p)) => {
                    self.modal = Some(Modal::Menu {
                        title: p.name.clone(),
                        items: vec![
                            ("New bash tab".into(), Act::NewTab(p.clone())),
                            ("New agent…".into(), Act::NewAgent(p)),
                        ],
                        x: m.column,
                        y: m.row,
                        sel: 0,
                    })
                }
                Some(Hit::Machine(id)) if self.mach(&id).is_some_and(|m| !m.is_local()) => {
                    self.modal = Some(Modal::Menu {
                        title: self.label(&id),
                        items: vec![("Reconnect".into(), Act::Reconnect(id))],
                        x: m.column,
                        y: m.row,
                        sel: 0,
                    })
                }
                _ => {}
            },
            MouseEventKind::Drag(MouseButton::Left) if self.drag => {
                self.sidebar_w = m.column.clamp(16, 60);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging.is_some() => {
                if let Some((d, moved)) = &mut self.dragging {
                    *moved = true;
                    let (path, ratio) = (d.path.clone(), d.ratio_at(m.column, m.row));
                    self.tree.set_ratio(&path, ratio);
                    self.laid_out();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((id, a, _)) = self.sel {
                    let b = self.cell_at(id, m.column, m.row);
                    self.sel = Some((id, a, b));
                }
            }
            MouseEventKind::Up(_) => {
                self.drag = false;
                // A press on a divider that never moved is a click on the cell under it.
                if let Some((_, false)) = self.dragging.take()
                    && let Some(i) = self.tree.geometry(self.term).cell_at(m.column, m.row)
                {
                    self.focus_index(i);
                }
                self.copy_selection();
            }
            // A full-screen program (most agents) has no tmux history to scroll: the wheel is its
            // own, as a mouse event where it reads the mouse, else as arrow keys like a terminal.
            // It goes to the cell under the pointer, whichever cell has the focus.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.modal.is_none()
                    && m.column >= self.side_area.x
                    && m.column < self.side_area.x + self.side_area.width
                    && m.row >= self.side_area.y
                    && m.row < self.side_area.y + self.side_area.height =>
            {
                self.side_off = if m.kind == MouseEventKind::ScrollUp {
                    self.side_off.saturating_sub(2)
                } else {
                    (self.side_off + 2).min(self.side_max)
                };
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if matches!(hit, Some(Hit::Pane(id)) if self.panes.get(&id).is_some_and(|p| p.view.alt && p.view.scroll == 0)) =>
            {
                self.sel = None;
                if let Some(Hit::Pane(id)) = hit
                    && let Some(a) = self.agent_in(id).filter(|a| a.running()).cloned()
                {
                    let (row, col) = self.cell_at(id, m.column, m.row);
                    let up = m.kind == MouseEventKind::ScrollUp;
                    let (mouse, sgr) = self
                        .panes
                        .get(&id)
                        .map_or((false, false), |p| (p.view.mouse, p.view.sgr));
                    for (lit, key) in wheel_input(up, col, row, mouse, sgr) {
                        self.queue(core, a.clone(), lit, key);
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(Hit::Pane(id)) = hit {
                    self.sel = None;
                    let inner_h = self.panes.get(&id).map_or(0, |p| p.inner.height);
                    let view = self.view(id);
                    view.scroll = (view.scroll + 3).min(view.max_scroll(inner_h));
                    view.next = None;
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(Hit::Pane(id)) = hit {
                    self.sel = None;
                    let view = self.view(id);
                    view.scroll = view.scroll.saturating_sub(3);
                    view.next = None;
                }
            }
            _ => {}
        }
    }
    fn click(&mut self, core: &Core, h: Hit) {
        match h {
            Hit::Machine(m) => self.click_machine(&m),
            Hit::Project(p) => self.click_project(&p),
            Hit::Agent(a) | Hit::Tab(a) => {
                self.open_agent(&a);
                self.in_sidebar = false;
            }
            Hit::Doc(d) => {
                self.open_doc(core, &d);
                self.in_sidebar = false;
            }
            Hit::NewAgent(p) => self.agent_form(&p),
            Hit::NewTab(p) => self.new_tab(core, &p),
            Hit::NewProject => self.project_form(),
            Hit::Pane(id) | Hit::Title(id) => self.focus_pane(id),
            Hit::Divider(_) => {}
            Hit::Help => self.modal = Some(Modal::Help),
            Hit::Theme => {
                let (w, h) = (
                    self.term.x + self.term.width,
                    self.term.y + self.term.height,
                );
                self.theme_menu(w.saturating_sub(70), h.saturating_sub(9));
            }
            Hit::Detach => self.quit = true,
            Hit::Start(a, true) => self.confirm_new(&a),
            Hit::Start(a, new) => self.run_act(core, Act::Start(a, new)),
            Hit::SidebarEdge => self.drag = true,
            Hit::SideMore(up) => {
                let page = (self.side_area.height as usize).saturating_sub(3).max(1);
                self.side_off = if up {
                    self.side_off.saturating_sub(page)
                } else {
                    (self.side_off + page).min(self.side_max)
                };
            }
            Hit::Menu(i) => self.menu_pick(core, i),
            Hit::Field(i) => {
                if let Some(Modal::Form(f)) = &mut self.modal {
                    f.focus = i;
                }
            }
            Hit::Choice(i, j) => {
                if let Some(Modal::Form(f)) = &mut self.modal {
                    f.focus = i;
                    f.fields[i].value = f.fields[i].choices[j].to_string();
                }
            }
            Hit::Submit => match self.modal {
                Some(Modal::Confirm { .. }) => {
                    if let Some(Modal::Confirm { act, .. }) = self.modal.take() {
                        self.run_act(core, act);
                    }
                }
                _ => self.submit(core),
            },
            Hit::Cancel => self.modal = None,
        }
    }

    fn agent_menu(&mut self, id: &Id, x: u16, y: u16) {
        let Some(a) = self
            .agents
            .iter()
            .find(|a| a.machine == id.machine && a.name == id.name)
        else {
            return;
        };
        let mut items = vec![];
        if a.running() {
            items.push((
                "Restart (same conversation)".to_string(),
                Act::Restart(a.id()),
            ));
            items.push(("Stop…".to_string(), Act::Stop(a.id(), None)));
        } else {
            items.push(("Start".to_string(), Act::Start(a.id(), false)));
            items.push((
                "Start a new conversation…".to_string(),
                Act::Start(a.id(), true),
            ));
            items.push(("Remove from the list…".to_string(), Act::Remove(a.id())));
        }
        items.push((
            "New agent in this project".to_string(),
            Act::NewAgent(a.project_id()),
        ));
        // The cell commands act on the focused cell, so only the agent in it offers them.
        if self.current().is_some_and(|c| c.id() == *id) {
            let mut cell = |text: &str, action: Action| {
                items.push((
                    format!("{text} ({})", self.keys.label(action)),
                    Act::Cell(action),
                ));
            };
            cell("Split right", Action::SplitRight);
            cell("Split down", Action::SplitDown);
            cell("Zoom, and back", Action::Zoom);
            if self.tree.len() > 1 {
                cell("Close this cell", Action::CloseCell);
            }
        }
        self.modal = Some(Modal::Menu {
            title: a.name.clone(),
            items,
            x,
            y,
            sel: 0,
        });
    }
    fn menu_pick(&mut self, core: &Core, i: usize) {
        let Some(Modal::Menu { items, .. }) = self.modal.take() else {
            return;
        };
        let Some((_, act)) = items.get(i).cloned() else {
            return;
        };
        match act {
            Act::Stop(a, _) => self.confirm_stop(&a),
            Act::Start(a, true) => self.confirm_new(&a),
            Act::Remove(a) => {
                self.modal = Some(Modal::Confirm {
                    text: format!(
                        "Remove {} from the list? Its conversation is not deleted.",
                        a.name
                    ),
                    button: "Remove",
                    act: Act::Remove(a),
                })
            }
            other => self.run_act(core, other),
        }
    }
    fn theme_menu(&mut self, x: u16, y: u16) {
        // Read the theme folder again, so a file made a moment ago shows up.
        let (customs, warnings) = crate::theme::custom::load_all();
        self.customs = customs;
        if !warnings.is_empty() {
            self.say(format!("Theme files: {}", warnings.join("; ")));
        }
        let builtin = !self.custom.is_some();
        let mut items: Vec<(String, Act)> = ThemeName::ALL
            .iter()
            .map(|t| {
                let on = if builtin && *t == self.theme {
                    "●"
                } else {
                    "○"
                };
                (
                    format!("{on} {:<8} {}", t.label().to_lowercase(), t.about()),
                    Act::Theme(*t),
                )
            })
            .collect();
        for c in &self.customs {
            let on = if self.custom.as_deref() == Some(c.name.as_str()) {
                "●"
            } else {
                "○"
            };
            items.push((
                format!("{on} {:<8} {} (yours)", c.name, c.about),
                Act::Custom(c.name.clone()),
            ));
        }
        let sel = items
            .iter()
            .position(|(t, _)| t.starts_with('●'))
            .unwrap_or(0);
        self.modal = Some(Modal::Menu {
            title: "Theme".into(),
            items,
            x,
            y,
            sel,
        });
    }
    fn confirm_stop(&mut self, a: &Id) {
        // Stop the run the person saw when asked, not whatever runs by the time they confirm.
        let run = self
            .agents
            .iter()
            .find(|x| x.machine == a.machine && x.name == a.name)
            .and_then(|x| x.run.clone());
        self.modal = Some(Modal::Confirm {
            text: quips::stop_question(self.keys.wit, &a.name),
            button: "Stop",
            act: Act::Stop(a.clone(), run),
        });
    }
    fn confirm_new(&mut self, a: &Id) {
        self.modal = Some(Modal::Confirm {
            text: format!(
                "Start {} in a new conversation? pctl forgets the saved one, so Start and Restart no longer resume it.",
                a.name
            ),
            button: "Start new",
            act: Act::Start(a.clone(), true),
        });
    }
    /// Do one action on an agent or project: here at once, on another computer as a request whose
    /// answer is taken up later (`finish`), so a slow or down computer never holds up the app.
    fn act_on(&mut self, core: &Core, machine: &str, req: Value, ok: String, then: Then) {
        if machine.is_empty() {
            match bridge::act(core, &req) {
                Ok(_) => {
                    self.say(ok);
                    self.after(then);
                }
                Err(e) => self.say(format!("{e:#}")),
            }
            self.recapture();
            self.poke();
            return;
        }
        let mut op = req;
        op["op"] = json!("act");
        if let Some(rx) = self.request(machine, op, Some(ACT_WITHIN)) {
            self.tasks.push((
                rx,
                Task::Act {
                    machine: machine.into(),
                    ok,
                    then,
                },
            ));
        } else if let Some(Modal::Form(f)) = &mut self.modal {
            f.busy = false;
        }
    }
    fn run_act(&mut self, core: &Core, act: Act) {
        let (machine, req, ok) = match &act {
            Act::Restart(a) => (
                &a.machine,
                json!({"action": "restart", "agent": a.name}),
                format!("Restarted {}", a.name),
            ),
            Act::Start(a, new) => (
                &a.machine,
                json!({"action": "start", "agent": a.name, "new": new}),
                format!("Started {}", a.name),
            ),
            Act::Remove(a) => (
                &a.machine,
                json!({"action": "remove", "agent": a.name}),
                format!("Removed {}", a.name),
            ),
            Act::Stop(a, seen) => (
                &a.machine,
                json!({"action": "stop", "agent": a.name, "run": seen}),
                format!("Stopped {}", a.name),
            ),
            Act::NewAgent(p) => {
                self.agent_form(p);
                return;
            }
            Act::NewTab(p) => {
                self.new_tab(core, p);
                return;
            }
            Act::Cell(action) => {
                self.action(core, *action);
                return;
            }
            Act::Reconnect(m) => {
                let label = self.label(m);
                if let Some(m) = self.machines.iter_mut().find(|x| &x.id == m) {
                    m.reconnect();
                }
                self.say(format!("Reconnecting {label}…"));
                return;
            }
            Act::Theme(t) => {
                let r = core.set_theme(*t).map(|_| {
                    self.theme = *t;
                    self.custom = None;
                    self.pal = t.palette();
                    format!("Theme: {} — {}", t.label().to_lowercase(), t.about())
                });
                return self.themed(r);
            }
            Act::Custom(name) => {
                let r = match self.customs.iter().find(|c| &c.name == name) {
                    Some(c) => {
                        self.pal = c.palette;
                        self.custom = Some(name.clone());
                        Ok(format!("Theme: {name} — {}", c.about))
                    }
                    None => Err(anyhow::anyhow!(
                        "theme {name} is gone from the theme folder"
                    )),
                };
                return self.themed(r);
            }
        };
        let machine = machine.clone();
        self.act_on(core, &machine, req, ok, Then::Nothing);
    }
    fn themed(&mut self, r: Result<String>) {
        match r {
            Ok(msg) => self.say(msg),
            Err(e) => self.say(format!("{e:#}")),
        }
        self.recapture();
        self.poke();
    }

    /// A new bash tab right away, like a new tab in a terminal app: named
    /// `<project>-bash<n>`, in the project's folder.
    fn new_tab(&mut self, core: &Core, project: &Id) {
        let name = (1..)
            .map(|n| format!("{}-bash{n}", project.name))
            .find(|c| {
                !self
                    .agents
                    .iter()
                    .any(|a| a.machine == project.machine && &a.name == c)
            })
            .unwrap_or_default();
        let req = json!({"action": "new_tab", "project": project.name, "name": name});
        let ok = format!("Opened {name}");
        self.act_on(
            core,
            &project.machine,
            req,
            ok,
            Then::Opened(project.clone(), name),
        );
    }
    fn after(&mut self, then: Then) {
        match then {
            Then::Opened(p, name) | Then::Form(p, name) => self.opened(&p, &name),
            Then::Doc(d) => self.doc_opened(&d),
            Then::Nothing => {}
        }
    }
    /// The person chose a document: its reader starts where the document is, and shows in a cell
    /// when it is up. Opening marks it read. Nothing here runs unless the person clicked.
    fn open_doc(&mut self, core: &Core, id: &Id) {
        let Some(d) = self
            .docs
            .iter()
            .find(|d| d.machine == id.machine && d.id == id.name)
            .cloned()
        else {
            return;
        };
        let viewer = Id::new(&d.machine, &crate::docs::viewer_name(&d.id));
        let project = Id::new(&d.machine, &d.project);
        if self.project.as_ref() == Some(&project)
            && let Some(cell) = self.pane_of(&viewer)
        {
            return self.focus_pane(cell);
        }
        let req = json!({"action": "doc_open", "doc": d.id});
        let ok = format!("Opened {}", d.title);
        self.act_on(core, &d.machine.clone(), req, ok, Then::Doc(d));
    }
    /// The reader runs: in a new cell right of the one that shows the agent which showed the
    /// document, else of the focused one.
    fn doc_opened(&mut self, d: &DocRow) {
        let project = Id::new(&d.machine, &d.project);
        self.collapsed.remove(&project);
        self.set_project(Some(project.clone()));
        let viewer = Id::new(&d.machine, &crate::docs::viewer_name(&d.id));
        let beside =
            d.by.as_ref()
                .and_then(|by| self.pane_of(&Id::new(&d.machine, by)));
        if let Some(cell) = beside {
            self.focus_pane(cell);
        }
        if self.pane_of(&viewer).is_none() {
            // Without room the reader takes the focused cell rather than not showing at all.
            self.split(Dir::Right);
            let a = Agent {
                machine: viewer.machine.clone(),
                name: viewer.name.clone(),
                project: project.name.clone(),
                status: "starting".into(),
                pane: None,
                run: None,
                queued: 0,
                viewer: true,
                stale: false,
            };
            self.fresh.insert(a.id(), (a.clone(), Instant::now()));
            self.agents.push(a);
            let pane = self.panes.entry(self.focused()).or_default();
            pane.agent = Some(viewer.clone());
            pane.view.next = None;
        } else if let Some(cell) = self.pane_of(&viewer) {
            self.focus_pane(cell);
        }
        for doc in self
            .docs
            .iter_mut()
            .filter(|x| x.machine == d.machine && x.id == d.id)
        {
            doc.new = false;
        }
        self.sel = None;
        self.recapture();
        self.poke_machine(&d.machine);
    }
    /// An agent just added: listed at once, even if the next refresh does not have it yet, and
    /// shown in its project.
    fn opened(&mut self, project: &Id, name: &str) {
        let a = Agent {
            machine: project.machine.clone(),
            name: name.into(),
            project: project.name.clone(),
            status: "starting".into(),
            stale: false,
            pane: None,
            run: None,
            queued: 0,
            viewer: false,
        };
        self.agents.push(a.clone());
        self.fresh.insert(a.id(), (a.clone(), Instant::now()));
        self.open_agent(&a.id());
        self.recapture();
        self.poke_machine(&project.machine);
    }
    fn project_form(&mut self) {
        // Projects are made on this computer; with others shown, it needs a server of its own.
        if self.multi() && !self.local_connected() {
            return self.say("No pctl server runs on this computer, so it cannot hold a project");
        }
        self.modal = Some(Modal::Form(Form {
            project: None,
            fields: vec![
                Field {
                    label: "Name",
                    value: String::new(),
                    choices: &[],
                    hint: "for example 54_Agent_Panel",
                },
                Field {
                    label: "Folder",
                    value: String::new(),
                    choices: &[],
                    hint: "the project's folder (absolute path)",
                },
            ],
            focus: 0,
            error: String::new(),
            busy: false,
        }));
    }
    fn agent_form(&mut self, project: &Id) {
        if let Some(why) = self.refused(&project.machine) {
            return self.say(why);
        }
        // On another computer the folder is a path there; the form only holds the text typed.
        let root = self
            .mach(&project.machine)
            .and_then(|m| m.projects.iter().find(|(n, _)| *n == project.name))
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        self.modal = Some(Modal::Form(Form {
            project: Some(project.clone()),
            fields: vec![
                Field {
                    label: "Name",
                    value: String::new(),
                    choices: &[],
                    hint: "short and unique, for example panel-review",
                },
                Field {
                    label: "Folder",
                    value: root,
                    choices: &[],
                    hint: "where it works",
                },
                Field {
                    label: "Kind",
                    value: KINDS[0].into(),
                    choices: &KINDS,
                    hint: "",
                },
                Field {
                    label: "Model",
                    value: String::new(),
                    choices: &[],
                    hint: "optional, for example claude-sonnet-5-5",
                },
                Field {
                    label: "Command",
                    value: String::new(),
                    choices: &[],
                    hint: "only for Command, for example bash -l",
                },
                Field {
                    label: "Worktree",
                    value: String::new(),
                    choices: &[],
                    hint: "optional new branch, for example panel/topic",
                },
            ],
            focus: 0,
            error: String::new(),
            busy: false,
        }));
    }
    fn modal_key(&mut self, core: &Core, k: KeyEvent) {
        self.flush_typed(core);
        match &mut self.modal {
            Some(Modal::Help) => self.modal = None,
            Some(Modal::Confirm { .. }) => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(Modal::Confirm { act, .. }) = self.modal.take() {
                        self.run_act(core, act);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => self.modal = None,
                _ => {}
            },
            Some(Modal::Menu { items, sel, .. }) => match k.code {
                KeyCode::Up => *sel = sel.saturating_sub(1),
                KeyCode::Down => *sel = (*sel + 1).min(items.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let i = *sel;
                    self.menu_pick(core, i);
                }
                KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Some(Modal::Form(f)) => {
                let n = f.fields.len();
                let field = &mut f.fields[f.focus];
                match k.code {
                    KeyCode::Esc => self.modal = None,
                    KeyCode::Enter if f.busy => {}
                    KeyCode::Enter => self.submit(core),
                    KeyCode::Tab | KeyCode::Down => f.focus = (f.focus + 1) % n,
                    KeyCode::BackTab | KeyCode::Up => f.focus = (f.focus + n - 1) % n,
                    KeyCode::Left | KeyCode::Right if !field.choices.is_empty() => {
                        let c = field.choices;
                        let i = c.iter().position(|v| *v == field.value).unwrap_or(0);
                        let j = if k.code == KeyCode::Right {
                            (i + 1) % c.len()
                        } else {
                            (i + c.len() - 1) % c.len()
                        };
                        field.value = c[j].to_string();
                    }
                    KeyCode::Backspace if field.choices.is_empty() => {
                        field.value.pop();
                    }
                    KeyCode::Char(c)
                        if field.choices.is_empty()
                            && !k.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        field.value.push(c)
                    }
                    _ => {}
                }
            }
            None => {}
        }
    }
    fn submit(&mut self, core: &Core) {
        let Some(Modal::Form(f)) = &mut self.modal else {
            return;
        };
        if f.busy {
            return;
        }
        let v = |i: usize| f.fields[i].value.trim().to_string();
        let mut new_root = String::new();
        let result: Result<String> = match f.project.clone() {
            None => {
                let (name, folder) = (v(0), v(1));
                if name.is_empty() || folder.is_empty() {
                    f.error = "Name and folder are needed.".into();
                    return;
                }
                if !plain_name(&name) {
                    f.error = "Use letters, digits, '-', '_' or '.' in the name (it is also a msg address).".into();
                    return;
                }
                core.register_project(&name, std::path::Path::new(&folder))
                    .map(|pr| {
                        new_root = pr.root;
                    })
                    .map(|_| name)
            }
            Some(project) => {
                let (name, folder, kind, model, command, branch) =
                    (v(0), v(1), v(2), v(3), v(4), v(5));
                if name.is_empty() || folder.is_empty() {
                    f.error = "Name and folder are needed.".into();
                    return;
                }
                if !plain_name(&name) {
                    f.error = "Use letters, digits, '-', '_' or '.' in the name (it is also a msg address).".into();
                    return;
                }
                let req = json!({
                    "action": "new_agent", "project": project.name, "name": name, "folder": folder,
                    "kind": kind, "model": model, "command": command, "branch": branch,
                });
                if !project.machine.is_empty() {
                    f.busy = true;
                    f.error = String::new();
                    let ok = format!("Started {name}");
                    let machine = project.machine.clone();
                    return self.act_on(core, &machine, req, ok, Then::Form(project, name));
                }
                bridge::act(core, &req).map(|_| name)
            }
        };
        match result {
            Ok(name) => {
                let new_project = f.project.is_none();
                let project = f.project.clone();
                self.modal = None;
                if new_project {
                    let id = Id::new("", &name);
                    self.set_project(Some(id.clone()));
                    self.machines[0].projects.push((name.clone(), new_root));
                    self.say(format!("Added project {name}. Now add its first agent."));
                    self.poke();
                    self.agent_form(&id);
                } else {
                    self.say(format!("Started {name}"));
                    self.poke();
                    let project = project.unwrap_or_default();
                    self.opened(&project, &name);
                }
            }
            Err(e) => {
                if let Some(Modal::Form(f)) = &mut self.modal {
                    f.error = format!("{e:#}");
                }
            }
        }
    }

    // ---- drawing ----

    fn glyph(&self, status: &str) -> (&'static str, Style) {
        let p = self.pal;
        match status {
            "blocked" => (
                "!",
                Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            ),
            "working" => ("●", Style::default().fg(p.cyan)),
            "idle" => ("●", Style::default().fg(p.accent)),
            "exited" => ("×", Style::default().fg(p.muted)),
            "stopped" => ("○", Style::default().fg(p.muted)),
            _ => ("?", Style::default().fg(p.warn)),
        }
    }
    fn hit(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }

    fn render(&mut self, f: &mut Frame) {
        self.hits.clear();
        let p = self.pal;
        let area = f.area();
        f.render_widget(Block::default().style(p.base()), area);
        if area.height < 4 || area.width < 30 {
            f.render_widget(Paragraph::new("Window too small for pctl"), area);
            return;
        }
        let main = Rect {
            height: area.height - 1,
            ..area
        };
        let side_w = if self.sidebar {
            self.sidebar_w.min(main.width / 2)
        } else {
            0
        };
        let right_x = if self.sidebar {
            main.x + side_w + 1
        } else {
            main.x
        };
        let right = Rect {
            x: right_x,
            width: main.width - (right_x - main.x),
            ..main
        };
        if self.sidebar {
            self.render_sidebar(
                f,
                Rect {
                    width: side_w,
                    ..main
                },
            );
            let edge = Rect {
                x: main.x + side_w,
                width: 1,
                ..main
            };
            let line: Vec<Line> = (0..edge.height).map(|_| Line::from("│")).collect();
            f.render_widget(
                Paragraph::new(line).style(Style::default().fg(p.border).bg(p.bg)),
                edge,
            );
            self.hit(edge, Hit::SidebarEdge);
        }
        self.render_tabs(f, Rect { height: 1, ..right });
        let term = Rect {
            y: right.y + 1,
            height: right.height.saturating_sub(1),
            ..right
        };
        self.term = term;
        self.place();
        self.render_term(f, term);
        self.render_status(
            f,
            Rect {
                y: area.y + area.height - 1,
                height: 1,
                ..area
            },
        );
        self.render_modal(f, area);
    }

    fn render_sidebar(&mut self, f: &mut Frame, r: Rect) {
        let p = self.pal;
        f.render_widget(Block::default().style(p.body()), r);
        self.side_area = r;
        let mut y = r.y;
        let line = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>, style: Style| {
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(style),
                Rect {
                    x: r.x,
                    y,
                    width: r.width,
                    height: 1,
                },
            );
        };
        line(
            f,
            y,
            vec![Span::styled(
                " PROJECTS",
                Style::default().fg(p.muted).add_modifier(Modifier::BOLD),
            )],
            p.body(),
        );
        // Projects are made on this computer: with others shown, only where a server runs.
        if !self.multi() || self.local_connected() {
            let plus = Rect {
                x: r.x + r.width.saturating_sub(4),
                y,
                width: 3,
                height: 1,
            };
            f.render_widget(
                Paragraph::new("[+]").style(Style::default().fg(p.accent)),
                plus,
            );
            self.hit(plus, Hit::NewProject);
        }
        y += 1;
        let rows = self.side_rows();
        if rows.is_empty() {
            if self.machines[0].loaded {
                line(
                    f,
                    y + 1,
                    vec![Span::raw(" No projects yet.")],
                    Style::default().fg(p.muted),
                );
                let b = Rect {
                    x: r.x + 1,
                    y: y + 2,
                    width: 13.min(r.width),
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new("[+ Project]").style(Style::default().fg(p.accent)),
                    b,
                );
                self.hit(b, Hit::NewProject);
            }
            return;
        }
        // With machines shown, their projects sit one step in.
        let step = u16::from(self.multi());
        let pad = |n: u16| " ".repeat((n + step) as usize);
        // A row that came into focus (keyboard, or another agent on screen) is brought into view.
        let cap = (r.height as usize).saturating_sub(1);
        let current = self.current().map(Agent::id);
        let seen = (self.in_sidebar, self.side_sel, current.clone());
        let want = if self.in_sidebar && (seen.0, seen.1) != (self.side_seen.0, self.side_seen.1) {
            Some(self.side_sel)
        } else if seen.2 != self.side_seen.2 {
            current.and_then(|c| {
                rows.iter()
                    .position(|r| matches!(r, SideRow::Agent(a) if *a == c))
            })
        } else {
            None
        };
        self.side_seen = seen;
        if let Some(i) = want {
            self.side_off = reveal(rows.len(), cap, self.side_off, i);
        }
        let (off, shown, above, below) = side_window(rows.len(), cap, self.side_off);
        self.side_off = off;
        self.side_max = if rows.len() > cap && cap >= 3 {
            rows.len() - cap + 1
        } else {
            0
        };
        let more = |f: &mut Frame, this: &mut Self, y: u16, up: bool, n: usize| {
            let text = format!(" {} {n} more", if up { "▲" } else { "▼" });
            line(f, y, vec![Span::raw(text)], Style::default().fg(p.muted));
            this.hit(
                Rect {
                    x: r.x,
                    y,
                    width: r.width,
                    height: 1,
                },
                Hit::SideMore(up),
            );
        };
        if above > 0 {
            more(f, self, y, true, above);
            y += 1;
        }
        for (i, row) in rows.iter().enumerate().skip(off).take(shown) {
            let selected_row = self.in_sidebar && i == self.side_sel;
            let base = if selected_row {
                Style::default().fg(p.text).bg(p.raised)
            } else {
                p.body()
            };
            let rect = Rect {
                x: r.x,
                y,
                width: r.width,
                height: 1,
            };
            let muted = |machine: &str| self.refused(machine).is_some();
            match row {
                SideRow::Machine(id) => {
                    let open = !self.collapsed_machines.contains(id);
                    let blocked = self
                        .agents
                        .iter()
                        .filter(|a| &a.machine == id && a.status == "blocked" && !a.viewer)
                        .count();
                    let st = if muted(id) {
                        Style::default().fg(p.muted).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                    };
                    let mut spans = vec![
                        Span::raw(if open { " ▾ " } else { " ▸ " }),
                        Span::styled(fit(&self.label(id), r.width.saturating_sub(7) as usize), st),
                    ];
                    if blocked > 0 {
                        spans.push(Span::styled(
                            format!(" !{blocked}"),
                            Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
                        ));
                    }
                    line(f, y, spans, base);
                    self.hit(rect, Hit::Machine(id.clone()));
                }
                SideRow::Note(text) => {
                    let text = fit(text, r.width.saturating_sub(4 + step) as usize);
                    line(
                        f,
                        y,
                        vec![Span::raw(format!("{}{text}", pad(2)))],
                        Style::default().fg(p.muted),
                    );
                }
                SideRow::Project(id) => {
                    let open = !self.collapsed.contains(id);
                    let mine = self.project_agents(id);
                    let blocked = mine.iter().filter(|a| a.status == "blocked").count();
                    let mut spans = vec![Span::raw(format!(
                        "{}{}",
                        pad(0),
                        if open { " ▾ " } else { " ▸ " }
                    ))];
                    let current = self.project.as_ref() == Some(id);
                    let st = if muted(&id.machine) {
                        Style::default().fg(p.muted)
                    } else if current {
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(p.text)
                    };
                    spans.push(Span::styled(
                        fit(&id.name, r.width.saturating_sub(7 + step) as usize),
                        st,
                    ));
                    if blocked > 0 {
                        spans.push(Span::styled(
                            format!(" !{blocked}"),
                            Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
                        ));
                    }
                    line(f, y, spans, base);
                    self.hit(rect, Hit::Project(id.clone()));
                }
                SideRow::Agent(id) => {
                    let a = self
                        .agents
                        .iter()
                        .find(|a| a.machine == id.machine && a.name == id.name)
                        .cloned();
                    let (g, gs) = self.glyph(a.as_ref().map_or("", |a| a.status.as_str()));
                    let current = self.current().is_some_and(|c| c.id() == *id);
                    let st = if current && !selected_row {
                        Style::default().fg(p.ink).bg(p.accent)
                    } else if muted(&id.machine) {
                        Style::default().fg(p.muted).bg(base.bg.unwrap_or(p.bg))
                    } else {
                        base
                    };
                    let kept = a.as_ref().map_or(0, |a| a.queued);
                    let stale = a.as_ref().is_some_and(|a| a.stale);
                    let tag = if kept > 0 {
                        format!(" ✉{kept}")
                    } else {
                        String::new()
                    };
                    let mark = if stale { " ↑" } else { "" };
                    let room = r.width.saturating_sub(6 + step) as usize;
                    let mut spans = vec![
                        Span::raw(pad(3)),
                        Span::styled(g, gs),
                        Span::raw(" "),
                        Span::raw(fit(
                            &id.name,
                            room.saturating_sub(tag.chars().count() + mark.chars().count()),
                        )),
                        Span::raw(mark),
                    ];
                    if kept > 0 {
                        spans.push(Span::styled(tag, Style::default().fg(p.accent)));
                    }
                    line(f, y, spans, st);
                    self.hit(rect, Hit::Agent(id.clone()));
                }
                SideRow::Doc(id) => {
                    let d = self
                        .docs
                        .iter()
                        .find(|d| d.machine == id.machine && d.id == id.name);
                    let (title, new) =
                        d.map_or((String::new(), false), |d| (d.title.clone(), d.new));
                    let st = if muted(&id.machine) {
                        Style::default().fg(p.muted).bg(base.bg.unwrap_or(p.bg))
                    } else {
                        base
                    };
                    let room = r.width.saturating_sub(10 + step) as usize;
                    let mut spans = vec![
                        Span::raw(pad(3)),
                        Span::raw("📄 "),
                        Span::raw(docs::fit_cells(&title, room)),
                    ];
                    if new {
                        spans.push(Span::styled(
                            " new",
                            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                        ));
                    }
                    line(f, y, spans, st);
                    self.hit(rect, Hit::Doc(id.clone()));
                }
                SideRow::Add(project) => {
                    line(
                        f,
                        y,
                        vec![Span::styled(
                            format!("{}+ agent", pad(3)),
                            Style::default().fg(p.muted),
                        )],
                        base,
                    );
                    self.hit(rect, Hit::NewAgent(project.clone()));
                }
            }
            y += 1;
        }
        if below > 0 {
            more(f, self, y, false, below);
        }
    }

    fn render_tabs(&mut self, f: &mut Frame, r: Rect) {
        let p = self.pal;
        f.render_widget(Block::default().style(Style::default().bg(p.panel)), r);
        let Some(project) = self.project.clone() else {
            return;
        };
        let current = self.current().map(Agent::id);
        let tabs: Vec<Agent> = self.project_agents(&project).into_iter().cloned().collect();
        let mut x = r.x;
        for a in &tabs {
            let (g, gs) = self.glyph(&a.status);
            let label = format!(" {} ", fit(&a.name, 24));
            let w = (label.chars().count() + 2) as u16;
            if x + w + 3 > r.x + r.width {
                break;
            }
            let on = current.as_ref() == Some(&a.id());
            let st = if on {
                Style::default()
                    .fg(p.text)
                    .bg(p.raised)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.muted).bg(p.panel)
            };
            let rect = Rect {
                x,
                y: r.y,
                width: w,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" {g}"), gs.bg(if on { p.raised } else { p.panel })),
                    Span::styled(label, st),
                ])),
                rect,
            );
            self.hit(rect, Hit::Tab(a.id()));
            x += w + 1;
        }
        let plus = Rect {
            x,
            y: r.y,
            width: 3,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.accent).bg(p.panel)),
            plus,
        );
        self.hit(plus, Hit::NewTab(project));
    }

    /// Text in the middle of an area, a third of the way down. The row below it.
    fn center(&self, f: &mut Frame, r: Rect, lines: Vec<Line<'static>>) -> u16 {
        let h = lines.len() as u16;
        let y = r.y + r.height.saturating_sub(h) / 3;
        f.render_widget(
            Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center)
                .style(self.pal.base()),
            Rect {
                y,
                height: h.min(r.height),
                ..r
            },
        );
        y + h
    }
    fn button(&mut self, f: &mut Frame, r: Rect, y: u16, text: &str, hit: Hit) {
        let p = self.pal;
        let w = text.chars().count() as u16;
        let b = Rect {
            x: r.x + r.width.saturating_sub(w) / 2,
            y,
            width: w.min(r.width),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(text.to_string()).style(Style::default().fg(p.ink).bg(p.accent)),
            b,
        );
        self.hit(b, hit);
    }

    fn render_term(&mut self, f: &mut Frame, r: Rect) {
        if !self.machines[0].loaded {
            self.center(f, r, vec![Line::from(quips::loading(self.keys.wit))]);
            return;
        }
        let Some(project) = self.project.clone() else {
            if self.multi() {
                // Machines are saved, so this is not a first run: say where each stands.
                let mut lines = vec![Line::from("Nothing to show yet."), Line::from("")];
                for m in &self.machines {
                    lines.push(Line::from(match m.unusable() {
                        Some(why) if !m.is_local() => format!("{}: {why}", m.label),
                        _ if !m.connected => format!("{}: no pctl server", m.label),
                        _ => format!("{}: no projects", m.label),
                    }));
                }
                lines.push(Line::from(""));
                let y = self.center(f, r, lines);
                if self.local_connected() {
                    self.button(f, r, y, " + Project ", Hit::NewProject);
                }
                return;
            }
            let [hello, line] = quips::welcome(self.keys.wit);
            let mut lines = vec![
                Line::from(Span::styled(
                    hello,
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(line),
                Line::from(""),
            ];
            if !self.local_connected() {
                lines.push(Line::from("No PCTL server is running yet."));
                if crate::server_start_allowed().is_ok() {
                    lines.push(Line::from("Add a project with + Project; that starts one."));
                } else {
                    lines.push(Line::from(
                        "Start pctl with PCTL_STANDALONE=1 (or where [server] start_env allows it),",
                    ));
                    lines.push(Line::from("then add a project."));
                }
            }
            lines.push(Line::from(""));
            let y = self.center(f, r, lines);
            self.button(f, r, y, " + Project ", Hit::NewProject);
            return;
        };
        let titled = self.tree.titled();
        let leaves: Vec<u32> = self.tree.leaves().into_iter().copied().collect();
        let focus = self.focused();
        for i in self.tree.shown() {
            let Some(&id) = leaves.get(i) else {
                continue;
            };
            let Some((outer, inner)) = self.panes.get(&id).map(|p| (p.outer, p.inner)) else {
                continue;
            };
            if inner.width == 0 || inner.height == 0 {
                continue;
            }
            self.hit(inner, Hit::Pane(id));
            if titled {
                let title = Rect { height: 1, ..outer };
                self.hit(title, Hit::Title(id));
                self.render_title(f, id, title, id == focus);
            }
            self.render_pane(f, id, inner, &project, !titled);
        }
        if titled {
            let p = self.pal;
            for d in self.tree.geometry(self.term).dividers {
                if d.dir == Dir::Right {
                    let line: Vec<Line> = (0..d.rect.height).map(|_| Line::from("│")).collect();
                    f.render_widget(
                        Paragraph::new(line).style(Style::default().fg(p.border).bg(p.bg)),
                        d.rect,
                    );
                }
                self.hit(d.rect, Hit::Divider(d));
            }
        }
    }

    /// A cell's title row: the computer (when machines are saved), the agent and its state.
    fn render_title(&self, f: &mut Frame, id: u32, r: Rect, focused: bool) {
        let p = self.pal;
        let a = self.agent_in(id);
        let (text, glyph) = match a {
            Some(a) if a.viewer => {
                let title = self
                    .docs
                    .iter()
                    .find(|d| d.machine == a.machine && crate::docs::viewer_name(&d.id) == a.name)
                    .map_or(a.name.as_str(), |d| d.title.as_str());
                let done = if a.status == "exited" {
                    " · closed"
                } else {
                    ""
                };
                (format!(" 📄 {title}{done} "), Some(self.glyph(&a.status)))
            }
            Some(a) => {
                let name = if self.multi() {
                    format!(" {} · {} ", self.label(&a.machine), a.name)
                } else {
                    format!(" {} ", a.name)
                };
                (name, Some(self.glyph(&a.status)))
            }
            None => (" no agent ".to_string(), None),
        };
        let muted = a.is_some_and(|a| self.refused(&a.machine).is_some());
        let base = if focused {
            Style::default()
                .fg(p.ink)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else if muted {
            Style::default()
                .fg(p.muted)
                .bg(p.panel)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.muted).bg(p.panel)
        };
        let mut spans = vec![Span::styled(
            fit(&text, r.width.saturating_sub(2) as usize),
            base,
        )];
        if let Some((g, gs)) = glyph {
            spans.push(Span::styled(g, if focused { base } else { gs.bg(p.panel) }));
        }
        f.render_widget(Block::default().style(base), r);
        f.render_widget(Paragraph::new(Line::from(spans)), r);
    }

    /// What one cell holds. `lone`: it is the only thing on screen, so it also says what the
    /// project lacks (no agents yet, its computer down), as the app did before it had cells.
    fn render_pane(&mut self, f: &mut Frame, id: u32, r: Rect, project: &Id, lone: bool) {
        let p = self.pal;
        let agent = self.agent_in(id).cloned();
        let machine = agent.as_ref().map_or(&project.machine, |a| &a.machine);
        if let Some(why) = self.refused(machine).filter(|_| agent.is_some() || lone) {
            // The last list stays in the sidebar; the screen says why there is none.
            self.center(
                f,
                r,
                vec![
                    Line::from(why),
                    Line::from(""),
                    Line::from("It is tried again on its own. Right-click the machine to try now."),
                ],
            );
            return;
        }
        let Some(a) = agent else {
            if !lone {
                self.center(f, r, vec![Line::from("Pick an agent in the sidebar")]);
                return;
            }
            let project = project.clone();
            let y = self.center(
                f,
                r,
                vec![
                    Line::from(format!("No agents in {} yet.", project.name)),
                    Line::from(quips::empty_project(self.keys.wit)),
                    Line::from(""),
                ],
            );
            self.button(f, r, y, " + bash ", Hit::NewTab(project.clone()));
            self.button(
                f,
                r,
                y + 2,
                " + Agent (Claude Code, Codex, …) ",
                Hit::NewAgent(project),
            );
            return;
        };
        if a.status == "starting" {
            self.center(f, r, vec![Line::from(format!("Starting {}…", a.name))]);
            return;
        }
        if !a.running() {
            let y = self.center(
                f,
                r,
                vec![
                    Line::from(format!("{} is {}.", a.name, a.status)),
                    Line::from(""),
                ],
            );
            self.button(f, r, y, " Start ", Hit::Start(a.id(), false));
            self.button(
                f,
                r,
                y + 2,
                " Start a new conversation… ",
                Hit::Start(a.id(), true),
            );
            return;
        }
        let Some(view) = self.panes.get(&id).map(|p| &p.view) else {
            return;
        };
        if view.owner != (a.machine.clone(), a.pane.clone().unwrap_or_default()) {
            return;
        }
        // The theme colours the terminal too: default background and text, and the 16 ANSI
        // colours; 256-colour and RGB output from the agent is left as it is.
        let (tbg, tfg, ansi) = (p.term_bg, p.term_fg, p.ansi);
        let tone = |c: Option<Color>, default: Color| match c {
            None | Some(Color::Reset) => default,
            Some(Color::Indexed(n)) if n < 16 => ansi.map_or(Color::Indexed(n), |a| a[n as usize]),
            Some(c) => c,
        };
        let buf = f.buffer_mut();
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                buf[(x, y)].reset();
                buf[(x, y)].set_style(Style::default().fg(tfg).bg(tbg));
            }
        }
        // A pane taller than this area (another viewer sized it) shows its bottom rows: the input
        // line and any dialog live there, and nothing must be typed into a row out of sight.
        let skip = self.rows_skip(id);
        for (i, row) in view
            .rows
            .iter()
            .skip(skip as usize)
            .take(r.height as usize)
            .enumerate()
        {
            let y = r.y + i as u16;
            let mut x = r.x;
            for (text, style) in row {
                if x >= r.x + r.width {
                    break;
                }
                let style = Style {
                    fg: Some(tone(style.fg, tfg)),
                    bg: Some(tone(style.bg, tbg)),
                    ..*style
                };
                let (nx, _) = buf.set_stringn(x, y, text, (r.x + r.width - x) as usize, style);
                x = nx;
            }
        }
        // The selection sits on top of the text, reversed, the way a terminal shows one. A wide
        // character is marked once, on the cell it starts at.
        if let Some((sel, a, b)) = self.sel
            && sel == id
        {
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            let drawn = view.rows.iter().skip(skip as usize).take(r.height as usize);
            for (i, row) in drawn.enumerate() {
                let vrow = skip as usize + i;
                if vrow < a.0 as usize || vrow > b.0 as usize {
                    continue;
                }
                let (start, end) = if vrow == a.0 as usize && vrow == b.0 as usize {
                    (a.1 as usize, b.1 as usize)
                } else if vrow == a.0 as usize {
                    (a.1 as usize, usize::MAX)
                } else if vrow == b.0 as usize {
                    (0, b.1 as usize)
                } else {
                    (0, usize::MAX)
                };
                for (_, x, w) in term::row_cells(row) {
                    if x + w > start && x <= end && x < r.width as usize {
                        let cell = &mut buf[(r.x + x as u16, r.y + i as u16)];
                        cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                    }
                }
            }
        }
        if view.scroll > 0 {
            let note = format!(
                " ↑ {} lines back · scroll down or type to return ",
                view.scroll
            );
            let w = (note.chars().count() as u16).min(r.width);
            f.render_widget(
                Paragraph::new(note).style(Style::default().fg(p.ink).bg(p.warn)),
                Rect {
                    x: r.x + r.width - w,
                    y: r.y,
                    width: w,
                    height: 1,
                },
            );
        } else if let (Some((cx, cy)), true, false, None) = (
            view.cursor,
            id == self.focused(),
            self.in_sidebar,
            &self.modal,
        ) && let Some(cy) = cy.checked_sub(skip)
            && cx < r.width
            && cy < r.height
        {
            f.set_cursor_position((r.x + cx, r.y + cy));
        }
    }

    fn render_status(&mut self, f: &mut Frame, r: Rect) {
        let p = self.pal;
        let mut left = vec![Span::styled(
            " ◎ pctl ",
            Style::default()
                .fg(p.ink)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )];
        if let Some(a) = self.current() {
            let (g, gs) = self.glyph(&a.status);
            let on = if self.multi() {
                format!(" {} · {} / ", self.label(&a.machine), a.project)
            } else {
                format!(" {} / ", a.project)
            };
            left.push(Span::raw(on));
            left.push(Span::styled(g, gs));
            left.push(Span::raw(format!(" {} {}", a.name, a.status)));
            if a.queued > 0 {
                left.push(Span::styled(
                    format!(" · {} kept for it", a.queued),
                    Style::default().fg(p.accent),
                ));
            }
        }
        if self.tree.zoomed {
            left.push(Span::styled("   zoomed", Style::default().fg(p.accent)));
        }
        // A fresh notice or the prefix hint goes first: it lasts seconds and may matter more.
        if self.prefix {
            left.push(Span::styled(
                format!("   {} …", self.keys.prefix.label()),
                Style::default().fg(p.warn),
            ));
        } else if self.notice_live() {
            left.push(Span::styled(
                format!("   {}", self.notice),
                Style::default().fg(p.warn),
            ));
        }
        let blocked = self
            .agents
            .iter()
            .filter(|a| a.status == "blocked" && !a.viewer)
            .count();
        if blocked > 0 {
            left.push(Span::styled(
                format!("   {}", quips::waiting(self.keys.wit, blocked)),
                Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            ));
        }
        let buttons = [
            (" ◐ Theme ", Hit::Theme),
            (" ? Help ", Hit::Help),
            (" ⏏ Leave ", Hit::Detach),
        ];
        let bw: u16 = buttons
            .iter()
            .map(|(t, _)| t.chars().count() as u16 + 1)
            .sum();
        let show = r.width > bw + 20;
        // The text stops before the buttons and ends with "…" instead of running under them.
        let room = if show { r.width - bw } else { r.width } as usize;
        let total: usize = left.iter().map(|s| s.content.chars().count()).sum();
        if total > room {
            let mut kept = Vec::new();
            let mut used = 0;
            for span in left {
                let n = span.content.chars().count();
                if used + n < room {
                    used += n;
                    kept.push(span);
                } else {
                    let rest: String = span
                        .content
                        .chars()
                        .take(room.saturating_sub(used + 1))
                        .collect();
                    kept.push(Span::styled(format!("{rest}…"), span.style));
                    break;
                }
            }
            left = kept;
        }
        f.render_widget(
            Paragraph::new(Line::from(left)).style(Style::default().fg(p.text).bg(p.panel)),
            Rect {
                width: room as u16,
                ..r
            },
        );
        f.render_widget(
            Block::default().style(Style::default().bg(p.panel)),
            Rect {
                x: r.x + room as u16,
                width: r.width - room as u16,
                ..r
            },
        );
        if show {
            let mut x = r.x + r.width;
            for (text, hit) in buttons.into_iter().rev() {
                let w = text.chars().count() as u16;
                x -= w;
                let b = Rect { x, width: w, ..r };
                f.render_widget(
                    Paragraph::new(text).style(Style::default().fg(p.text).bg(p.raised)),
                    b,
                );
                self.hit(b, hit);
                x -= 1;
            }
        }
    }

    fn render_modal(&mut self, f: &mut Frame, area: Rect) {
        let p = self.pal;
        let Some(modal) = self.modal.take() else {
            return;
        };
        let boxed = |f: &mut Frame, r: Rect, title: &str| {
            f.render_widget(Clear, r);
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} "))
                    .style(Style::default().fg(p.text).bg(p.raised))
                    .border_style(Style::default().fg(p.accent)),
                r,
            );
        };
        match &modal {
            Modal::Help => {
                let mut lines: Vec<Line> = vec![
                    Line::from(
                        "Mouse: click a project, agent or tab to open it; + adds; right-click or double-click an agent for",
                    ),
                    Line::from(
                        "restart, stop, start and remove; drag the sidebar edge; wheel scrolls the terminal.",
                    ),
                    Line::from(
                        "Some terminals (MATE Terminal) keep the right button: double-click or press ctrl+b m.",
                    ),
                    Line::from(
                        "States (● idle or working, ! waiting for you, × exited, ○ stopped) are read from",
                    ),
                    Line::from("the screen: a best guess that can lag a moment."),
                    Line::from(""),
                ];
                let mut entries: Vec<(String, &str)> = ACTIONS
                    .iter()
                    .map(|(_, _, action, text)| (self.keys.label(*action), *text))
                    .filter(|(label, _)| !label.is_empty())
                    .collect();
                entries.push((format!("{} 1…9", self.keys.prefix.label()), "Tab 1 to 9"));
                entries.push((
                    format!("{} {}", self.keys.prefix.label(), self.keys.prefix.label()),
                    "Send the prefix key itself to the agent",
                ));
                // One column when it fits, else two, so every shortcut stays on a short screen.
                let two = lines.len() + entries.len() + 6 > area.height as usize;
                let width = if two { area.width.min(120) } else { 90 };
                let column = (width.saturating_sub(4) / 2) as usize;
                let entry = |(key, text): &(String, &str), room: usize| {
                    vec![
                        Span::styled(format!("{key:>18}  "), Style::default().fg(p.accent)),
                        Span::raw(fit(text, room.saturating_sub(20))),
                    ]
                };
                if two {
                    lines.clear();
                    let half = entries.len().div_ceil(2);
                    for i in 0..half {
                        let mut spans = entry(&entries[i], column);
                        if let Some(right) = entries.get(i + half) {
                            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                            spans.push(Span::raw(" ".repeat(column.saturating_sub(used))));
                            spans.extend(entry(right, column));
                        }
                        lines.push(Line::from(spans));
                    }
                } else {
                    for e in &entries {
                        lines.push(Line::from(entry(e, usize::MAX)));
                    }
                }
                lines.push(Line::from(""));
                let src = if self.keys.source.is_empty() {
                    "~/.config/pctl/config.toml (not created yet)".to_string()
                } else {
                    self.keys.source.clone()
                };
                lines.push(Line::from(Span::styled(
                    format!("Change shortcuts in {src}, under [keys]."),
                    Style::default().fg(p.muted),
                )));
                // A short screen has no room for the intro lines; keep the right-button hint there.
                let last = if two {
                    "Some terminals (MATE Terminal) keep the right button: double-click or press ctrl+b m."
                        .to_string()
                } else {
                    quips::tip(self.keys.wit).to_string()
                };
                lines.push(Line::from(Span::styled(last, Style::default().fg(p.muted))));
                let h = lines.len() as u16 + 2;
                let r = centered(area, width, h);
                boxed(f, r, "Shortcuts");
                f.render_widget(Paragraph::new(lines), inner(r));
            }
            Modal::Confirm { text, button, .. } => {
                let w = (text.chars().count() as u16 + 6).clamp(40, 72);
                let lines = (text.chars().count() as u16).div_ceil(w - 4).max(1);
                let r = centered(area, w, lines + 5);
                boxed(f, r, "Confirm");
                let i = inner(r);
                f.render_widget(
                    Paragraph::new(text.clone()).wrap(ratatui::widgets::Wrap { trim: true }),
                    Rect {
                        height: lines + 1,
                        ..i
                    },
                );
                self.buttons(
                    f,
                    Rect {
                        y: i.y + lines + 1,
                        height: 1,
                        ..i
                    },
                    button,
                );
            }
            Modal::Menu {
                title,
                items,
                x,
                y,
                sel,
            } => {
                let w = items
                    .iter()
                    .map(|(t, _)| t.chars().count() as u16)
                    .max()
                    .unwrap_or(10)
                    .max(title.chars().count() as u16)
                    + 4;
                let h = items.len() as u16 + 2;
                let r = Rect {
                    x: (*x).min(area.width.saturating_sub(w)),
                    y: (*y).min(area.height.saturating_sub(h)),
                    width: w.min(area.width),
                    height: h.min(area.height),
                };
                boxed(f, r, title);
                for (i, (t, _)) in items.iter().enumerate() {
                    let row = Rect {
                        x: r.x + 1,
                        y: r.y + 1 + i as u16,
                        width: r.width - 2,
                        height: 1,
                    };
                    let st = if i == *sel {
                        Style::default().fg(p.ink).bg(p.accent)
                    } else {
                        Style::default()
                    };
                    f.render_widget(Paragraph::new(format!(" {t}")).style(st), row);
                    self.hit(row, Hit::Menu(i));
                }
            }
            Modal::Form(form) => {
                let h = form.fields.len() as u16 * 2 + 6;
                let r = centered(area, 76, h);
                boxed(
                    f,
                    r,
                    if form.project.is_none() {
                        "New project".into()
                    } else {
                        let p = form.project.clone().unwrap_or_default();
                        if p.machine.is_empty() {
                            format!("New agent in {}", p.name)
                        } else {
                            format!("New agent in {} on {}", p.name, self.label(&p.machine))
                        }
                    }
                    .as_str(),
                );
                let i = inner(r);
                let mut y = i.y;
                for (n, field) in form.fields.iter().enumerate() {
                    let focused = n == form.focus;
                    let label = Span::styled(
                        format!("{:>9}  ", field.label),
                        Style::default().fg(if focused { p.accent } else { p.muted }),
                    );
                    if field.choices.is_empty() {
                        let value = if field.value.is_empty() && !focused {
                            Span::styled(field.hint, Style::default().fg(p.muted))
                        } else {
                            Span::raw(field.value.clone())
                        };
                        let row = Rect {
                            x: i.x,
                            y,
                            width: i.width,
                            height: 1,
                        };
                        f.render_widget(
                            Paragraph::new(Line::from(vec![label, value])).style(
                                Style::default().bg(if focused { p.panel } else { p.raised }),
                            ),
                            row,
                        );
                        self.hit(row, Hit::Field(n));
                        if focused {
                            let cx = i.x + 11 + field.value.chars().count() as u16;
                            f.set_cursor_position((cx.min(i.x + i.width - 1), y));
                        }
                    } else {
                        f.render_widget(
                            Paragraph::new(Line::from(label)),
                            Rect {
                                x: i.x,
                                y,
                                width: 11,
                                height: 1,
                            },
                        );
                        let mut x = i.x + 11;
                        for (j, c) in field.choices.iter().enumerate() {
                            let on = *c == field.value;
                            let text = format!(" {} {c} ", if on { "●" } else { "○" });
                            let w = text.chars().count() as u16;
                            let b = Rect {
                                x,
                                y,
                                width: w.min(i.x + i.width - x),
                                height: 1,
                            };
                            f.render_widget(
                                Paragraph::new(text).style(if on {
                                    Style::default().fg(p.ink).bg(p.accent)
                                } else {
                                    Style::default().fg(p.text)
                                }),
                                b,
                            );
                            self.hit(b, Hit::Choice(n, j));
                            x += w + 1;
                        }
                    }
                    y += 2;
                }
                if !form.error.is_empty() {
                    f.render_widget(
                        Paragraph::new(form.error.clone()).style(Style::default().fg(p.danger)),
                        Rect {
                            x: i.x,
                            y,
                            width: i.width,
                            height: 1,
                        },
                    );
                }
                self.buttons(
                    f,
                    Rect {
                        x: i.x,
                        y: y + 1,
                        width: i.width,
                        height: 1,
                    },
                    if form.project.is_none() {
                        "Add project"
                    } else {
                        "Start agent"
                    },
                );
            }
        }
        self.modal = Some(modal);
    }
    fn buttons(&mut self, f: &mut Frame, r: Rect, ok: &str) {
        let p = self.pal;
        let ok = format!(" {ok} ");
        let w = ok.chars().count() as u16;
        let a = Rect {
            width: w.min(r.width),
            ..r
        };
        let c = Rect {
            x: r.x + w + 2,
            width: 8.min(r.width.saturating_sub(w + 2)),
            ..r
        };
        f.render_widget(
            Paragraph::new(ok).style(
                Style::default()
                    .fg(p.ink)
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            a,
        );
        f.render_widget(
            Paragraph::new(" Cancel ").style(Style::default().fg(p.text).bg(p.panel)),
            c,
        );
        self.hit(a, Hit::Submit);
        self.hit(c, Hit::Cancel);
    }
}

/// Names typed in the app: they become msg addresses and folder names, so keep them plain.
fn plain_name(n: &str) -> bool {
    !n.starts_with(['.', '-'])
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}
fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 3,
        width: w,
        height: h,
    }
}
fn inner(r: Rect) -> Rect {
    Rect {
        x: r.x + 2,
        y: r.y + 1,
        width: r.width.saturating_sub(4),
        height: r.height.saturating_sub(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn an_image_goes_in_pieces_then_the_set_then_the_key() {
        // Two whole pieces and a short one, so a piece boundary and a remainder both show.
        let png: Vec<u8> = (0..CLIP_PIECE * 2 + 5).map(|i| (i % 251) as u8).collect();
        let ops = clip_ops("a", "run-1", &png);
        assert_eq!(ops.len(), 3 + 2);
        let mut joined = vec![];
        for (i, op) in ops.iter().take(3).enumerate() {
            assert_eq!(
                (op["op"].as_str(), op["seq"].as_u64()),
                (Some("clip_put"), Some(i as u64))
            );
            let piece = term::unbase64(op["data"].as_str().expect("data")).expect("base64");
            assert!(piece.len() <= CLIP_PIECE);
            joined.extend(piece);
        }
        assert_eq!(joined, png);
        assert_eq!(ops[3]["op"], "clip_set");
        assert_eq!(ops[3]["bytes"], png.len());
        assert_eq!(ops[4]["keys"], json!([{"literal": false, "key": "C-v"}]));
        assert!(ops.iter().all(|o| o["agent"] == "a" && o["run"] == "run-1"));
        // Each request line stays under the bridge's 1 MiB line limit.
        assert!(ops.iter().all(|o| o.to_string().len() < 1 << 20));
        assert_eq!(clip_ops("a", "r", &[1]).len(), 3, "one byte is one piece");
    }

    fn agent_on(machine: &str, name: &str, status: &str) -> Agent {
        Agent {
            machine: machine.into(),
            name: name.into(),
            project: "p".into(),
            status: status.into(),
            stale: false,
            pane: Some("%1".into()),
            run: Some("r".into()),
            queued: 0,
            viewer: false,
        }
    }
    fn agent(name: &str, status: &str) -> Agent {
        agent_on("", name, status)
    }
    fn statuses(pairs: &[(&str, &str)]) -> HashMap<Id, String> {
        pairs
            .iter()
            .map(|(n, s)| (Id::new("", n), s.to_string()))
            .collect()
    }
    fn a_id(name: &str) -> Id {
        Id::new("", name)
    }
    #[test]
    fn only_an_agent_this_app_saw_working_is_announced() {
        // The first update after start announces nothing, and neither does an agent first seen now.
        assert!(notifications(&statuses(&[]), &[agent("a", "idle")], None).is_empty());
        assert!(
            notifications(
                &statuses(&[("other", "working")]),
                &[agent("a", "idle")],
                None
            )
            .is_empty()
        );
        assert_eq!(
            notifications(&statuses(&[("a", "working")]), &[agent("a", "idle")], None),
            vec![(a_id("a"), "done", false)]
        );
        assert_eq!(
            notifications(
                &statuses(&[("a", "working")]),
                &[agent("a", "blocked")],
                None
            ),
            vec![(a_id("a"), "needs you", true)]
        );
        // Idle to idle, and anything that was not working, says nothing.
        assert!(notifications(&statuses(&[("a", "idle")]), &[agent("a", "idle")], None).is_empty());
        assert!(
            notifications(&statuses(&[("a", "blocked")]), &[agent("a", "idle")], None).is_empty()
        );
        // The agent on screen is the one being looked at already.
        assert!(
            notifications(
                &statuses(&[("a", "working")]),
                &[agent("a", "idle")],
                Some(&a_id("a"))
            )
            .is_empty()
        );
    }
    #[test]
    fn the_same_name_on_two_computers_is_two_agents() {
        // "a" was working here and on morty; only morty's finished.
        let mut before = statuses(&[("a", "working")]);
        before.insert(Id::new("morty", "a"), "working".into());
        let now = [agent_on("", "a", "working"), agent_on("morty", "a", "idle")];
        assert_eq!(
            notifications(&before, &now, None),
            vec![(Id::new("morty", "a"), "done", false)]
        );
        // Looking at this computer's "a" does not hide morty's.
        assert_eq!(notifications(&before, &now, Some(&a_id("a"))).len(), 1);
    }
    #[test]
    fn notices_are_rate_limited_per_agent() {
        let t = Instant::now();
        assert!(may_notify(None, t));
        assert!(!may_notify(Some(t), t + Duration::from_secs(9)));
        assert!(may_notify(Some(t), t + Duration::from_secs(10)));
    }
    #[test]
    fn a_notice_carries_a_plain_name_and_a_bell_only_when_blocked() {
        assert_eq!(
            notice_sequence("panel-main", "done", false),
            "\x1b]777;notify;pctl;panel-main: done\x07"
        );
        assert_eq!(
            notice_sequence("a;b\x1b\x07c", "needs you", true),
            "\x1b]777;notify;pctl;abc: needs you\x07\x07"
        );
        assert_eq!(sanitize("x;y\x0bz\tc"), "xyzc");
    }
    #[test]
    fn the_wheel_goes_to_a_full_screen_program_as_its_own_input() {
        assert_eq!(
            wheel_input(true, 4, 9, true, true),
            vec![(true, "\x1b[<64;5;10M".to_string())]
        );
        assert_eq!(
            wheel_input(false, 0, 0, true, true),
            vec![(true, "\x1b[<65;1;1M".to_string())]
        );
        assert_eq!(
            wheel_input(true, 4, 9, false, false),
            vec![(false, "Up".to_string()); 3]
        );
        assert_eq!(
            wheel_input(false, 4, 9, false, true),
            vec![(false, "Down".to_string()); 3]
        );
        assert!(wheel_input(true, 4, 9, true, false).is_empty());
    }
    #[test]
    fn a_mouse_cell_maps_through_the_pane_offset_and_the_scroll() {
        let t = Rect::new(27, 1, 10, 5);
        assert_eq!(view_cell(t, 0, 20, 28, 2), (1, 1));
        assert_eq!(
            view_cell(t, 10, 20, 28, 2),
            (11, 1),
            "scrolled back, a screen row holds an older line"
        );
        assert_eq!(view_cell(t, 0, 20, 5, 2), (1, 0), "left of the view clamps");
        assert_eq!(view_cell(t, 0, 20, 99, 2), (1, 9), "right of it clamps");
        assert_eq!(view_cell(t, 0, 0, 99, 99), (0, 9), "with no rows yet");
    }
    #[test]
    fn names_typed_in_the_app_are_plain() {
        for ok in ["panel-main", "54_Agent_Panel", "a.b"] {
            assert!(super::plain_name(ok), "{ok}");
        }
        for bad in ["PS1='$ '; clear", "a b", "-x", ".hidden", "a/b", "한글"] {
            assert!(!super::plain_name(bad), "{bad}");
        }
    }
    fn app_with(names: &[&str]) -> App {
        let mut app = App::new(ThemeName::ALL[0], Keymap::defaults());
        app.agents = names.iter().map(|n| agent(n, "idle")).collect();
        app.machines[0].loaded = true;
        app.set_project(Some(Id::new("", "p")));
        app.term = Rect::new(0, 1, 80, 24);
        app
    }
    fn in_cells(app: &App) -> Vec<Option<String>> {
        app.tree
            .leaves()
            .iter()
            .map(|id| app.panes[*id].agent.as_ref().map(|a| a.name.clone()))
            .collect()
    }
    fn name(n: &str) -> Option<String> {
        Some(n.to_string())
    }
    #[test]
    fn a_new_cell_is_empty_and_an_agent_is_never_shown_twice() {
        let mut app = app_with(&["a", "b", "c"]);
        app.tab.insert(Id::new("", "p"), "a".into());
        app.settle();
        assert_eq!(
            in_cells(&app),
            [name("a")],
            "one cell shows the project's tab"
        );
        app.split(Dir::Right);
        assert_eq!(in_cells(&app), [name("a"), None]);
        assert_eq!(app.tree.focus, 1, "the new cell has the focus");
        app.open_agent(&a_id("b"));
        assert_eq!(in_cells(&app), [name("a"), name("b")]);
        // Asking for one that is shown focuses its cell instead of showing it again.
        app.open_agent(&a_id("a"));
        assert_eq!(
            (in_cells(&app), app.tree.focus),
            ([name("a"), name("b")].to_vec(), 0)
        );
        // Another agent takes the place of the one in the focused cell.
        app.open_agent(&a_id("c"));
        assert_eq!(in_cells(&app), [name("c"), name("b")]);
        // An agent that is not listed is not opened.
        app.open_agent(&a_id("nobody"));
        assert_eq!(in_cells(&app), [name("c"), name("b")]);
    }
    #[test]
    fn closing_a_cell_keeps_the_agent_and_the_last_cell() {
        let mut app = app_with(&["a", "b"]);
        app.open_agent(&a_id("a"));
        app.split(Dir::Down);
        app.open_agent(&a_id("b"));
        app.close_cell();
        assert_eq!(in_cells(&app), [name("a")]);
        assert_eq!(app.agents.len(), 2, "nothing was stopped or removed");
        app.close_cell();
        assert_eq!(in_cells(&app), [name("a")]);
        assert!(app.notice.contains("last cell"), "{}", app.notice);
    }
    #[test]
    fn a_single_cell_follows_the_project_and_its_tab() {
        let mut app = app_with(&["a", "b"]);
        app.agents[1].project = "q".into();
        app.tab.insert(Id::new("", "p"), "a".into());
        app.tab.insert(Id::new("", "q"), "b".into());
        app.settle();
        assert_eq!(app.current().map(|a| a.name.as_str()), Some("a"));
        // With one cell, opening an agent of another project moves the project, as before cells.
        app.open_agent(&a_id("b"));
        assert_eq!(app.project, Some(Id::new("", "q")));
        assert_eq!(app.current().map(|a| a.name.as_str()), Some("b"));
        // Each project keeps its own cells.
        app.set_project(Some(Id::new("", "p")));
        app.split(Dir::Right);
        app.set_project(Some(Id::new("", "q")));
        assert_eq!(app.tree.len(), 1);
        app.set_project(Some(Id::new("", "p")));
        assert_eq!(app.tree.len(), 2, "p's two cells came back");
        // With several, an agent of another project goes into the focused cell and the project stays.
        app.open_agent(&a_id("b"));
        assert_eq!(app.project, Some(Id::new("", "p")));
        assert_eq!(in_cells(&app), [name("a"), name("b")]);
        // A single cell whose agent has left the list shows the project's tab again.
        let mut app = app_with(&["a", "b"]);
        app.tab.insert(Id::new("", "p"), "a".into());
        app.settle();
        app.agents.retain(|x| x.name != "a");
        app.tab.insert(Id::new("", "p"), "b".into());
        app.settle();
        assert_eq!(in_cells(&app), [name("b")]);
    }
    #[test]
    fn the_layout_of_a_project_round_trips_through_the_view_file() {
        let mut app = app_with(&["a", "b", "c"]);
        app.open_agent(&a_id("a"));
        app.split(Dir::Right);
        app.open_agent(&a_id("b"));
        app.split(Dir::Down);
        app.open_agent(&Id::new("", "c"));
        app.resize_cell(Side::Up);
        app.focus_index(1);
        let saved = app.layouts_json();
        assert_eq!(saved.len(), 1);
        assert_eq!(
            (saved[0]["project"].as_str(), saved[0]["machine"].as_str()),
            (Some("p"), Some(""))
        );
        // Through text, as the file holds it, into a fresh app.
        let text = serde_json::to_string(&saved).expect("json");
        let again: Vec<Value> = serde_json::from_str(&text).expect("parse");
        let mut fresh = app_with(&[]);
        let tree = fresh.tree_from_json(&again[0]).expect("a tree");
        fresh.layouts.insert(Id::new("", "q"), tree);
        fresh.set_project(Some(Id::new("", "q")));
        assert_eq!(in_cells(&fresh), [name("a"), name("b"), name("c")]);
        assert_eq!(fresh.tree.focus, 1);
        assert_eq!(fresh.tree.geometry(app.term), app.tree.geometry(app.term));
        // A cell that held an agent of another computer keeps its computer.
        let remote = json!({"focus": 0, "tree": {"split": "right", "ratio": 500,
            "a": {"machine": "rm", "agent": "x"}, "b": {"machine": "", "agent": "x"}}});
        let mut app = app_with(&[]);
        let tree = app.tree_from_json(&remote).expect("a tree");
        let ids: Vec<Option<Id>> = tree
            .leaves()
            .iter()
            .map(|id| app.panes[*id].agent.clone())
            .collect();
        assert_eq!(
            ids,
            [Some(Id::new("rm", "x")), Some(Id::new("", "x"))],
            "same name, two computers, two agents"
        );
        // The same agent twice is kept in the first cell only.
        let twice = json!({"focus": 0, "tree": {"split": "down", "ratio": 500,
            "a": {"machine": "", "agent": "x"}, "b": {"machine": "", "agent": "x"}}});
        let tree = app.tree_from_json(&twice).expect("a tree");
        let ids: Vec<Option<Id>> = tree
            .leaves()
            .iter()
            .map(|id| app.panes[*id].agent.clone())
            .collect();
        assert_eq!(ids, [Some(Id::new("", "x")), None]);
        // An empty cell and a damaged tree.
        assert!(
            app.tree_from_json(&json!({"tree": {"split": "up"}}))
                .is_none()
        );
        let empty = json!({"tree": {"split": "right", "a": {}, "b": {}}});
        let tree = app.tree_from_json(&empty).expect("a tree");
        assert!(
            tree.leaves()
                .iter()
                .all(|id| app.panes[*id].agent.is_none())
        );
    }
    fn doc_row(id: &str, by: &str, new: bool) -> DocRow {
        DocRow {
            machine: String::new(),
            id: id.into(),
            project: "p".into(),
            title: format!("Doc {id}"),
            by: Some(by.into()),
            new,
            shown: 1,
        }
    }
    fn viewer_of(id: &str) -> Agent {
        Agent {
            name: crate::docs::viewer_name(id),
            viewer: true,
            ..agent("x", "idle")
        }
    }
    #[test]
    fn documents_list_after_the_agents_and_readers_are_not_agents() {
        let mut app = app_with(&["a", "b"]);
        app.machines[0].projects = vec![("p".into(), "/".into())];
        app.agents.push(viewer_of("d1"));
        app.docs = vec![doc_row("d1", "a", true), doc_row("d2", "a", false)];
        let rows = app.side_rows();
        let at = |r: &SideRow| rows.iter().position(|x| x == r).unwrap();
        let (b, d1, d2, add) = (
            at(&SideRow::Agent(a_id("b"))),
            at(&SideRow::Doc(Id::new("", "d1"))),
            at(&SideRow::Doc(Id::new("", "d2"))),
            at(&SideRow::Add(Id::new("", "p"))),
        );
        assert!(
            b < d1 && d1 < d2 && d2 < add,
            "agents, documents, then +agent"
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, SideRow::Agent(i) if i.name.starts_with("doc-")))
        );
        assert_eq!(
            app.project_agents(&Id::new("", "p")).len(),
            2,
            "no tab for a reader"
        );
        app.docs = (0..20)
            .map(|i| doc_row(&format!("x{i}"), "a", false))
            .collect();
        assert_eq!(
            app.side_rows()
                .iter()
                .filter(|r| matches!(r, SideRow::Doc(_)))
                .count(),
            DOCS_PER_PROJECT
        );
    }
    #[test]
    fn an_arriving_document_moves_nothing_and_opening_it_splits_right_of_its_agent() {
        let mut app = app_with(&["a", "b"]);
        app.open_agent(&a_id("a"));
        app.split(Dir::Right);
        app.open_agent(&a_id("b"));
        app.focus_index(0);
        let before = (in_cells(&app), app.tree.focus, app.panes.len());
        // The list answer brings a new document (this computer was already watched).
        let answer = json!({"sessions": [], "projects": [{"name": "p", "root": "/"}], "connected": true,
            "docs": [{"id": "d1", "project": "p", "title": "Plan", "by": "a", "new": true, "shown": 1}]});
        app.apply(Update::from_list("", Ok(answer)));
        assert_eq!(app.docs.len(), 1);
        assert_eq!(
            (in_cells(&app), app.tree.focus, app.panes.len()),
            before,
            "nothing opened, no focus moved"
        );
        // Only the person's click opens it: right of the cell of the agent that showed it.
        app.focus_index(1);
        let doc = app.docs[0].clone();
        app.doc_opened(&doc);
        let viewer = crate::docs::viewer_name("d1");
        assert_eq!(in_cells(&app), [name("a"), name(&viewer), name("b")]);
        assert_eq!(app.tree.focus, 1, "the reader's cell has the focus");
        assert!(!app.docs[0].new, "opening marks it read");
        // A second click finds the open cell instead of starting another.
        app.focus_index(0);
        app.doc_opened(&doc);
        assert_eq!((in_cells(&app).len(), app.tree.focus), (3, 1));
        // Closing the cell ends the reader, and only the reader.
        app.close_cell();
        assert_eq!(in_cells(&app), [name("a"), name("b")]);
        assert_eq!(app.ending, [Id::new("", &viewer)]);
    }
    #[test]
    fn a_failed_list_keeps_the_documents_shown() {
        let mut app = app_with(&["a"]);
        app.docs = vec![doc_row("d1", "a", true)];
        app.apply(Update::from_list("", Err(anyhow::anyhow!("down"))));
        assert_eq!(app.docs.len(), 1);
    }
    #[test]
    fn the_agent_typed_to_is_the_one_in_the_focused_cell() {
        let mut app = app_with(&["a"]);
        app.agents.push(agent_on("rm", "r", "idle"));
        app.open_agent(&a_id("a"));
        app.split(Dir::Right);
        app.open_agent(&Id::new("rm", "r"));
        assert_eq!(app.current().map(|a| a.id()), Some(Id::new("rm", "r")));
        app.focus_index(0);
        assert_eq!(app.current().map(|a| a.id()), Some(a_id("a")));
        // A cell with no agent has nobody to type to.
        app.split(Dir::Down);
        assert!(app.current().is_none());
    }
    #[test]
    fn the_sidebar_window_keeps_its_more_lines_and_stays_in_range() {
        // Fits: everything shows, nothing is hidden, whatever the offset says.
        assert_eq!(side_window(5, 8, 3), (0, 5, 0, 0));
        // 20 rows in 10 lines: a "more" line below at the top, both in the middle, above at the end.
        assert_eq!(side_window(20, 10, 0), (0, 9, 0, 11));
        assert_eq!(side_window(20, 10, 4), (4, 8, 4, 8));
        assert_eq!(side_window(20, 10, 99), (11, 9, 11, 0));
        // Too little room for the lines: rows are cut, not hidden behind markers.
        assert_eq!(side_window(20, 2, 5), (0, 2, 0, 18));
    }

    #[test]
    fn a_row_is_revealed_with_the_least_scroll() {
        let (len, cap) = (30, 10);
        for i in 0..len {
            for off in [0, 3, 12, 21, 99] {
                let o = reveal(len, cap, off, i);
                let (first, shown, ..) = side_window(len, cap, o);
                assert!(first <= i && i < first + shown, "row {i} from {off}: {o}");
                if i >= side_window(len, cap, off).0
                    && i < side_window(len, cap, off).0 + side_window(len, cap, off).1
                {
                    assert_eq!(o, side_window(len, cap, off).0, "already shown: no scroll");
                }
            }
        }
    }

    #[test]
    fn a_taller_pane_can_be_scrolled_back_over_its_own_hidden_top() {
        let view = View {
            history: 100,
            size: (80, 40),
            ..View::default()
        };
        assert_eq!(
            view.max_scroll(24),
            116,
            "history plus the 16 rows above the cell"
        );
        assert_eq!(view.max_scroll(40), 100, "a pane as tall as its cell");
        assert_eq!(view.max_scroll(60), 100, "a cell taller than the pane");
        let flat = View {
            size: (80, 40),
            ..View::default()
        };
        assert_eq!(
            flat.max_scroll(24),
            16,
            "no history: the rows the cell hides"
        );
    }
    #[test]
    fn a_list_answer_becomes_an_update() {
        let ok = json!({"ok": true, "sessions": [{"name": "a"}], "projects": [{"name": "p", "root": "/r"}], "connected": true});
        let u = Update::from_list("rm", Ok(ok));
        assert_eq!(u.machine, "rm");
        assert_eq!(u.agents.expect("agents").len(), 1);
        assert_eq!(u.projects, [("p".to_string(), "/r".to_string())]);
        assert!(u.connected);
        // Sessions that could not be read still list the projects and the server.
        let part = json!({"sessions": [], "error": "no registry", "projects": [{"name": "p", "root": "/r"}], "connected": true});
        let u = Update::from_list("", Ok(part));
        assert_eq!(u.agents.expect_err("error"), "no registry");
        assert_eq!(u.projects.len(), 1);
        // A bridge that answered "not ok", and one that did not answer at all.
        let bad = Update::from_list("rm", Ok(json!({"ok": false, "error": "boom"})));
        assert_eq!(bad.agents.expect_err("error"), "boom");
        let gone = Update::from_list("rm", Err(anyhow::anyhow!("down: eof")));
        assert!(gone.agents.is_err() && !gone.connected);
    }
}
