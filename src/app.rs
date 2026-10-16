//! The pctl app: what `pctl` alone opens on a terminal. Like Herdr: projects and their agents on
//! the left, the project's agents as tabs on top, the selected agent's live terminal in the
//! middle, and the mouse for everything (click to open, `+` to add, right-click for actions).
//! Shortcuts come from `keys` and can be changed in a config file. tmux stays behind the app and
//! keeps the agents running; leaving the app stops nothing.
mod keys;
mod quips;
mod term;

use crate::{
    Core,
    human::literal,
    theme::{Palette, ThemeName},
    ui::TerminalGuard,
};
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use keys::{ACTIONS, Action, Keymap};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const KINDS: [&str; 5] = ["bash", "Claude Code", "Codex", "OpenCode", "Command"];
const CAPTURE_EVERY: Duration = Duration::from_millis(120);

#[derive(Clone, Debug)]
struct Agent {
    name: String,
    project: String,
    status: String,
    pane: Option<String>,
    run: Option<String>,
}
impl Agent {
    fn running(&self) -> bool {
        self.pane.is_some() && !matches!(self.status.as_str(), "stopped" | "lost")
    }
}

#[derive(Clone, Debug)]
enum Hit {
    Project(String),
    Agent(String),
    Tab(String),
    NewAgent(String),
    NewTab(String),
    NewProject,
    Terminal,
    Theme,
    Help,
    Detach,
    Start(String, bool),
    SidebarEdge,
    Menu(usize),
    Field(usize),
    Choice(usize, usize),
    Submit,
    Cancel,
}

#[derive(Clone, Debug)]
enum Act {
    Restart(String),
    Stop(String),
    Start(String, bool),
    Remove(String),
    NewAgent(String),
    NewTab(String),
    Theme(ThemeName),
    Custom(String),
}

struct Field {
    label: &'static str,
    value: String,
    choices: &'static [&'static str],
    hint: &'static str,
}
struct Form {
    /// None: a new project; Some(project): a new agent in it.
    project: Option<String>,
    fields: Vec<Field>,
    focus: usize,
    error: String,
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
    pane: String,
    rows: Vec<term::Row>,
    cursor: Option<(u16, u16)>,
    size: (u16, u16),
    scroll: usize,
    history: usize,
    next: Option<Instant>,
}

struct Update {
    agents: std::result::Result<Vec<Value>, String>,
    projects: Vec<(String, String)>,
    connected: bool,
}

/// Rows of the sidebar, for drawing and for keyboard moves.
#[derive(Clone, Debug, PartialEq)]
enum SideRow {
    Project(String),
    Agent(String),
    Add(String),
}

struct App {
    pal: Palette,
    theme: ThemeName,
    /// A theme file of the person's own (`pctl theme --new`), when one is picked.
    custom: Option<String>,
    customs: Vec<crate::theme::custom::Custom>,
    keys: Keymap,
    projects: Vec<(String, String)>,
    agents: Vec<Agent>,
    loaded: bool,
    connected: bool,
    project: Option<String>,
    tab: HashMap<String, String>,
    collapsed: HashSet<String>,
    sidebar: bool,
    sidebar_w: u16,
    in_sidebar: bool,
    side_sel: usize,
    view: View,
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
    fresh: HashMap<String, (Agent, Instant)>,
}

/// Open the app until the person leaves it. Never starts a server by itself: adding the first
/// agent goes through `session add`, which keeps its own start rules.
pub fn run(core: &Core) -> Result<()> {
    let (keys, warnings) = Keymap::load();
    let mut app = App::new(core.theme()?, keys);
    // Come back where the person left: last project and each project's tab (per viewer, not
    // shared state; a missing or bad file just means the first project).
    let place = core.state.join("app-view.json");
    if let Ok(v) =
        std::fs::read(&place).map(|b| serde_json::from_slice::<Value>(&b).unwrap_or_default())
    {
        app.project = v["project"].as_str().map(String::from);
        if let Some(tabs) = v["tabs"].as_object() {
            app.tab = tabs
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect();
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
            let update = Update {
                agents: observer.session_list().map_err(|e| format!("{e:#}")),
                projects: observer
                    .registry()
                    .map(|r| {
                        r.projects
                            .iter()
                            .map(|p| (p.name.clone(), p.root.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                connected: observer.server().is_ok_and(|m| !m.is_empty()),
            };
            if tx.send(update).is_err() {
                return;
            }
            match req_rx.recv_timeout(Duration::from_millis(1500)) {
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                _ => while req_rx.try_recv().is_ok() {},
            }
        }
    });
    {
        let _guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        while !app.quit {
            while let Ok(u) = rx.try_recv() {
                app.apply(u);
            }
            if app.view.next.is_none_or(|t| Instant::now() >= t) {
                app.capture(core);
            }
            terminal.draw(|f| app.render(f))?;
            if event::poll(Duration::from_millis(40))? {
                let ev = event::read()?;
                app.handle(core, ev);
            }
        }
    }
    println!("{}", quips::goodbye(app.keys.wit));
    let _ = std::fs::write(
        &place,
        serde_json::json!({"project": app.project, "tabs": app.tab, "theme": app.custom})
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
            projects: vec![],
            agents: vec![],
            loaded: false,
            connected: false,
            project: None,
            tab: HashMap::new(),
            collapsed: HashSet::new(),
            sidebar: true,
            sidebar_w: 26,
            in_sidebar: false,
            side_sel: 0,
            view: View::default(),
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
        }
    }
    fn say(&mut self, text: impl Into<String>) {
        self.notice = text.into();
        self.notice_at = Instant::now();
    }
    fn poke(&self) {
        if let Some(r) = &self.refresh {
            let _ = r.send(());
        }
    }

    fn apply(&mut self, u: Update) {
        self.loaded = true;
        self.connected = u.connected;
        self.projects = u.projects;
        match u.agents {
            Ok(rows) => {
                self.agents = rows
                    .iter()
                    .map(|s| Agent {
                        name: s["name"].as_str().unwrap_or_default().into(),
                        project: s["project"].as_str().unwrap_or_default().into(),
                        status: s["status"].as_str().unwrap_or("unknown").into(),
                        pane: s["pane"].as_str().map(String::from),
                        run: s["run_id"].as_str().map(String::from),
                    })
                    .collect();
            }
            Err(e) => self.say(format!("Could not read sessions: {e}")),
        }
        self.fresh.retain(|name, (_, at)| {
            at.elapsed() < Duration::from_secs(10) && !self.agents.iter().any(|a| &a.name == name)
        });
        self.agents
            .extend(self.fresh.values().map(|(a, _)| a.clone()));
        let names: Vec<String> = self.projects.iter().map(|(n, _)| n.clone()).collect();
        if self.project.as_ref().is_none_or(|p| !names.contains(p)) {
            self.project = names
                .iter()
                .find(|p| self.agents.iter().any(|a| &a.project == *p && a.running()))
                .or(names.first())
                .cloned();
        }
        for p in &names {
            let mine: Vec<&Agent> = self.agents.iter().filter(|a| &a.project == p).collect();
            let keep = self
                .tab
                .get(p)
                .is_some_and(|t| mine.iter().any(|a| &a.name == t));
            if !keep {
                match mine.iter().find(|a| a.running()).or(mine.first()) {
                    Some(a) => self.tab.insert(p.clone(), a.name.clone()),
                    None => self.tab.remove(p),
                };
            }
        }
    }
    fn current(&self) -> Option<&Agent> {
        let p = self.project.as_ref()?;
        let name = self.tab.get(p)?;
        self.agents.iter().find(|a| &a.name == name)
    }
    fn project_agents(&self, project: &str) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|a| a.project == project)
            .collect()
    }
    fn open_agent(&mut self, name: &str) {
        if let Some(a) = self.agents.iter().find(|a| a.name == name).cloned() {
            self.project = Some(a.project.clone());
            self.collapsed.remove(&a.project);
            self.tab.insert(a.project, a.name);
            self.view.scroll = 0;
            self.view.next = None;
        }
    }

    /// Read the current agent's screen from tmux, resizing its window to the view first.
    fn capture(&mut self, core: &Core) {
        self.view.next = Some(Instant::now() + CAPTURE_EVERY);
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            self.view.rows.clear();
            self.view.pane.clear();
            return;
        };
        // The pane comes from the last list; check it still holds this run before touching it.
        let Some(pane) = self.verified(core, &a, false) else {
            self.view.rows.clear();
            return;
        };
        if self.view.pane != pane {
            self.view = View {
                pane: pane.clone(),
                next: self.view.next,
                ..View::default()
            };
        }
        let (w, h) = (self.term.width, self.term.height);
        if w < 2 || h < 2 {
            return;
        }
        if self.view.size != (w, h) {
            let _ = core.tm(&[
                "resize-window",
                "-t",
                &pane,
                "-x",
                &w.to_string(),
                "-y",
                &h.to_string(),
            ]);
        }
        let (start, end) = (
            format!("-{}", self.view.scroll),
            format!("{}", h as i64 - 1 - self.view.scroll as i64),
        );
        let mut args = vec!["capture-pane", "-p", "-e", "-t", &pane];
        if self.view.scroll > 0 {
            args.extend(["-S", &start, "-E", &end]);
        }
        args.extend([
            ";",
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{cursor_x} #{cursor_y} #{cursor_flag} #{pane_width} #{pane_height} #{history_size}",
        ]);
        let Ok(out) = core.tm(&args) else {
            return;
        };
        let (body, last) = out.rsplit_once('\n').unwrap_or(("", out.as_str()));
        let m: Vec<u64> = last.split(' ').filter_map(|v| v.parse().ok()).collect();
        if m.len() == 6 {
            self.view.cursor = (m[2] == 1).then_some((m[0] as u16, m[1] as u16));
            self.view.size = (m[3] as u16, m[4] as u16);
            self.view.history = m[5] as usize;
            self.view.scroll = self.view.scroll.min(self.view.history);
        }
        self.view.rows = term::parse_capture(body);
    }

    /// The agent's pane, if it still holds the agent's current run. Panes are cached from the
    /// last list (every 1.5 s); after a restart or a new tmux server the same pane id can belong
    /// to another process, and nothing must be typed into it.
    /// shortcut: check and send are two tmux calls; a restart in the milliseconds between them
    /// could slip through. Use one `if-shell -F` command if that ever matters.
    fn verified(&mut self, core: &Core, a: &Agent, loud: bool) -> Option<String> {
        let (pane, run) = (a.pane.clone()?, a.run.clone()?);
        match core.tm(&["display-message", "-p", "-t", &pane, "#{@pctl_run}"]) {
            Ok(r) if r.trim() == run => Some(pane),
            _ => {
                if loud {
                    self.say(format!(
                        "{} changed (restarted or moved); not typing until the list catches up",
                        a.name
                    ));
                }
                self.poke();
                None
            }
        }
    }
    fn send_key(&mut self, core: &Core, k: &KeyEvent) {
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            return;
        };
        let Some((lit, key)) = term::key_args(k) else {
            return;
        };
        let Some(pane) = self.verified(core, &a, true) else {
            return;
        };
        let key = literal(&key);
        let r = if lit {
            core.tm(&["send-keys", "-t", &pane, "-l", "--", &key])
        } else {
            core.tm(&["send-keys", "-t", &pane, "--", &key])
        };
        if let Err(e) = r {
            self.say(format!("Could not type into {}: {e:#}", a.name));
        }
        self.view.scroll = 0;
        self.view.next = Some(Instant::now() + Duration::from_millis(25));
    }
    fn paste(&mut self, core: &Core, text: &str) {
        let Some(a) = self.current().filter(|a| a.running()).cloned() else {
            return;
        };
        let Some(pane) = self.verified(core, &a, true) else {
            return;
        };
        let r = core
            .tm(&["set-buffer", "-b", "pctl-app", "--", &literal(text)])
            .and_then(|_| core.tm(&["paste-buffer", "-p", "-d", "-b", "pctl-app", "-t", &pane]));
        if let Err(e) = r {
            self.say(format!("Could not paste: {e:#}"));
        }
        self.view.next = Some(Instant::now() + Duration::from_millis(25));
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
            Event::Resize(..) => self.view.size = (0, 0),
            _ => {}
        }
    }

    fn key(&mut self, core: &Core, k: KeyEvent) {
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
                let tabs: Vec<String> = self
                    .project
                    .as_deref()
                    .map(|p| {
                        self.project_agents(p)
                            .iter()
                            .map(|a| a.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(name) = tabs.get(n) {
                    self.open_agent(&name.clone());
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
            return self.sidebar_key(k);
        }
        self.send_key(core, &k);
    }

    fn action(&mut self, core: &Core, a: Action) {
        let current = self.current().map(|a| a.name.clone());
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
                let tabs: Vec<String> = self
                    .project_agents(&p)
                    .iter()
                    .map(|a| a.name.clone())
                    .collect();
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
        }
    }

    fn side_rows(&self) -> Vec<SideRow> {
        let mut rows = vec![];
        for (p, _) in &self.projects {
            rows.push(SideRow::Project(p.clone()));
            if !self.collapsed.contains(p) {
                for a in self.project_agents(p) {
                    rows.push(SideRow::Agent(a.name.clone()));
                }
                rows.push(SideRow::Add(p.clone()));
            }
        }
        rows
    }
    fn sidebar_key(&mut self, k: KeyEvent) {
        let rows = self.side_rows();
        match k.code {
            KeyCode::Up => self.side_sel = self.side_sel.saturating_sub(1),
            KeyCode::Down => self.side_sel = (self.side_sel + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Esc | KeyCode::Right => self.in_sidebar = false,
            KeyCode::Left => {
                if let Some(SideRow::Project(p)) = rows.get(self.side_sel) {
                    self.collapsed.insert(p.clone());
                }
            }
            KeyCode::Enter => match rows.get(self.side_sel).cloned() {
                Some(SideRow::Project(p)) => self.click_project(&p),
                Some(SideRow::Agent(a)) => {
                    self.open_agent(&a);
                    self.in_sidebar = false;
                }
                Some(SideRow::Add(p)) => self.agent_form(&p),
                None => {}
            },
            _ => {}
        }
    }
    fn click_project(&mut self, p: &str) {
        if self.project.as_deref() == Some(p) && !self.collapsed.contains(p) {
            self.collapsed.insert(p.to_string());
        } else {
            self.collapsed.remove(p);
            self.project = Some(p.to_string());
            self.view.next = None;
        }
    }

    fn mouse(&mut self, core: &Core, m: MouseEvent) {
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
                            self.click(core, h);
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.modal.is_none() => match hit {
                Some(Hit::Agent(a) | Hit::Tab(a)) => self.agent_menu(&a, m.column, m.row),
                Some(Hit::Project(p) | Hit::NewAgent(p) | Hit::NewTab(p)) => {
                    self.modal = Some(Modal::Menu {
                        title: p.clone(),
                        items: vec![
                            ("New bash tab".into(), Act::NewTab(p.clone())),
                            ("New agent…".into(), Act::NewAgent(p)),
                        ],
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
            MouseEventKind::Up(_) => self.drag = false,
            MouseEventKind::ScrollUp if matches!(hit, Some(Hit::Terminal)) => {
                self.view.scroll = (self.view.scroll + 3).min(self.view.history);
                self.view.next = None;
            }
            MouseEventKind::ScrollDown if matches!(hit, Some(Hit::Terminal)) => {
                self.view.scroll = self.view.scroll.saturating_sub(3);
                self.view.next = None;
            }
            _ => {}
        }
    }
    fn click(&mut self, core: &Core, h: Hit) {
        match h {
            Hit::Project(p) => self.click_project(&p),
            Hit::Agent(a) | Hit::Tab(a) => {
                self.open_agent(&a);
                self.in_sidebar = false;
            }
            Hit::NewAgent(p) => self.agent_form(&p),
            Hit::NewTab(p) => self.new_tab(core, &p),
            Hit::NewProject => self.project_form(),
            Hit::Terminal => self.in_sidebar = false,
            Hit::Help => self.modal = Some(Modal::Help),
            Hit::Theme => {
                let (w, h) = (
                    self.term.x + self.term.width,
                    self.term.y + self.term.height,
                );
                self.theme_menu(w.saturating_sub(70), h.saturating_sub(9));
            }
            Hit::Detach => self.quit = true,
            Hit::Start(a, new) => self.run_act(core, Act::Start(a, new)),
            Hit::SidebarEdge => self.drag = true,
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

    fn agent_menu(&mut self, name: &str, x: u16, y: u16) {
        let Some(a) = self.agents.iter().find(|a| a.name == name) else {
            return;
        };
        let mut items = vec![];
        if a.running() {
            items.push((
                "Restart (same conversation)".to_string(),
                Act::Restart(a.name.clone()),
            ));
            items.push(("Stop…".to_string(), Act::Stop(a.name.clone())));
        } else {
            items.push(("Start".to_string(), Act::Start(a.name.clone(), false)));
            items.push((
                "Start a new conversation".to_string(),
                Act::Start(a.name.clone(), true),
            ));
            items.push((
                "Remove from the list…".to_string(),
                Act::Remove(a.name.clone()),
            ));
        }
        items.push((
            "New agent in this project".to_string(),
            Act::NewAgent(a.project.clone()),
        ));
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
            Act::Stop(a) => self.confirm_stop(&a),
            Act::Remove(a) => {
                self.modal = Some(Modal::Confirm {
                    text: format!("Remove {a} from the list? Its conversation is not deleted."),
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
    fn confirm_stop(&mut self, a: &str) {
        self.modal = Some(Modal::Confirm {
            text: quips::stop_question(self.keys.wit, a),
            button: "Stop",
            act: Act::Stop(a.to_string()),
        });
    }
    fn run_act(&mut self, core: &Core, act: Act) {
        let r: Result<String> = match &act {
            Act::Restart(a) => core
                .session_restart(a, false, false)
                .map(|_| format!("Restarted {a}")),
            Act::Start(a, new) => core.session_start(a, *new).map(|_| format!("Started {a}")),
            Act::Remove(a) => core.session_remove(a).map(|_| format!("Removed {a}")),
            Act::Stop(a) => core.session_get(a).and_then(|s| {
                let (id, run) = (
                    s["agent_id"].as_str().unwrap_or_default(),
                    s["run_id"].as_str().unwrap_or_default(),
                );
                core.terminate(id, run).map(|_| format!("Stopped {a}"))
            }),
            Act::NewAgent(p) => {
                self.agent_form(p);
                return;
            }
            Act::NewTab(p) => {
                self.new_tab(core, p);
                return;
            }
            Act::Theme(t) => core.set_theme(*t).map(|_| {
                self.theme = *t;
                self.custom = None;
                self.pal = t.palette();
                format!("Theme: {} — {}", t.label().to_lowercase(), t.about())
            }),
            Act::Custom(name) => match self.customs.iter().find(|c| &c.name == name) {
                Some(c) => {
                    self.pal = c.palette;
                    self.custom = Some(name.clone());
                    Ok(format!("Theme: {name} — {}", c.about))
                }
                None => Err(anyhow::anyhow!(
                    "theme {name} is gone from the theme folder"
                )),
            },
        };
        match r {
            Ok(msg) => self.say(msg),
            Err(e) => self.say(format!("{e:#}")),
        }
        self.view.next = None;
        self.poke();
    }

    /// A new bash tab right away, like a new terminal tab in Herdr or Ghostty: named
    /// `<project>-bash<n>`, in the project's folder.
    fn new_tab(&mut self, core: &Core, project: &str) {
        let root = self
            .projects
            .iter()
            .find(|(n, _)| n == project)
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        let name = (1..)
            .map(|n| format!("{project}-bash{n}"))
            .find(|c| !self.agents.iter().any(|a| &a.name == c))
            .unwrap_or_default();
        match core.session_add(
            &name,
            Path::new(&root),
            Some(project),
            vec![bash(), "-l".into()],
            None,
            None,
            None,
        ) {
            Ok(_) => {
                self.added(&name, project);
                self.project = Some(project.into());
                self.tab.insert(project.into(), name.clone());
                self.say(format!("Opened {name}"));
            }
            Err(e) => self.say(format!("{e:#}")),
        }
        self.view.next = None;
        self.poke();
    }
    fn added(&mut self, name: &str, project: &str) {
        let a = Agent {
            name: name.into(),
            project: project.into(),
            status: "starting".into(),
            pane: None,
            run: None,
        };
        self.agents.push(a.clone());
        self.fresh.insert(name.into(), (a, Instant::now()));
    }
    fn project_form(&mut self) {
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
        }));
    }
    fn agent_form(&mut self, project: &str) {
        let root = self
            .projects
            .iter()
            .find(|(n, _)| n == project)
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        self.modal = Some(Modal::Form(Form {
            project: Some(project.to_string()),
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
        }));
    }
    fn modal_key(&mut self, core: &Core, k: KeyEvent) {
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
                core.register_project(&name, Path::new(&folder))
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
                let model = (!model.is_empty()).then_some(model);
                let argv: Result<Vec<String>> = match kind.as_str() {
                    "Claude Code" => Ok(["claude".to_string()]
                        .into_iter()
                        .chain(
                            model
                                .clone()
                                .map(|m| ["--model".into(), m])
                                .into_iter()
                                .flatten(),
                        )
                        .collect()),
                    "Codex" => Ok(["codex".to_string()]
                        .into_iter()
                        .chain(
                            model
                                .clone()
                                .map(|m| ["-m".into(), m])
                                .into_iter()
                                .flatten(),
                        )
                        .collect()),
                    "OpenCode" => Ok(vec!["opencode".into()]),
                    "bash" => Ok(vec![bash(), "-l".into()]),
                    _ => crate::parse_command(&command),
                };
                let opencode_model = if kind == "OpenCode" {
                    model.as_deref()
                } else {
                    None
                };
                argv.and_then(|argv| {
                    core.session_add(
                        &name,
                        Path::new(&folder),
                        Some(&project),
                        argv,
                        None,
                        (!branch.is_empty()).then_some((branch.as_str(), None)),
                        opencode_model,
                    )
                })
                .map(|_| name)
            }
        };
        match result {
            Ok(name) => {
                let new_project = f.project.is_none();
                self.modal = None;
                if new_project {
                    self.project = Some(name.clone());
                    self.projects.push((name.clone(), new_root));
                    self.say(format!("Added project {name}. Now add its first agent."));
                    self.poke();
                    self.agent_form(&name);
                } else {
                    self.say(format!("Started {name}"));
                    self.poke();
                    let project = self.project.clone().unwrap_or_default();
                    self.added(&name, &project);
                    self.tab.insert(project, name);
                    self.view.next = None;
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
        y += 1;
        if self.projects.is_empty() {
            if self.loaded {
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
        let rows = self.side_rows();
        for (i, row) in rows.iter().enumerate() {
            if y >= r.y + r.height {
                break;
            }
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
            match row {
                SideRow::Project(name) => {
                    let open = !self.collapsed.contains(name);
                    let mine = self.project_agents(name);
                    let blocked = mine.iter().filter(|a| a.status == "blocked").count();
                    let mut spans = vec![Span::raw(if open { " ▾ " } else { " ▸ " })];
                    let current = self.project.as_deref() == Some(name);
                    let st = if current {
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(p.text)
                    };
                    spans.push(Span::styled(
                        fit(name, r.width.saturating_sub(7) as usize),
                        st,
                    ));
                    if blocked > 0 {
                        spans.push(Span::styled(
                            format!(" !{blocked}"),
                            Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
                        ));
                    }
                    line(f, y, spans, base);
                    self.hit(rect, Hit::Project(name.clone()));
                }
                SideRow::Agent(name) => {
                    let a = self.agents.iter().find(|a| &a.name == name).cloned();
                    let (g, gs) = self.glyph(a.as_ref().map_or("", |a| a.status.as_str()));
                    let current = self.current().is_some_and(|c| &c.name == name);
                    let st = if current && !selected_row {
                        Style::default().fg(p.ink).bg(p.accent)
                    } else {
                        base
                    };
                    let spans = vec![
                        Span::raw("   "),
                        Span::styled(g, gs),
                        Span::raw(" "),
                        Span::raw(fit(name, r.width.saturating_sub(6) as usize)),
                    ];
                    line(f, y, spans, st);
                    self.hit(rect, Hit::Agent(name.clone()));
                }
                SideRow::Add(project) => {
                    line(
                        f,
                        y,
                        vec![Span::styled("   + agent", Style::default().fg(p.muted))],
                        base,
                    );
                    self.hit(rect, Hit::NewAgent(project.clone()));
                }
            }
            y += 1;
        }
    }

    fn render_tabs(&mut self, f: &mut Frame, r: Rect) {
        let p = self.pal;
        f.render_widget(Block::default().style(Style::default().bg(p.panel)), r);
        let Some(project) = self.project.clone() else {
            return;
        };
        let current = self.current().map(|a| a.name.clone());
        let tabs: Vec<Agent> = self.project_agents(&project).into_iter().cloned().collect();
        let mut x = r.x;
        for a in &tabs {
            let (g, gs) = self.glyph(&a.status);
            let label = format!(" {} ", fit(&a.name, 24));
            let w = (label.chars().count() + 2) as u16;
            if x + w + 3 > r.x + r.width {
                break;
            }
            let on = current.as_deref() == Some(a.name.as_str());
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
            self.hit(rect, Hit::Tab(a.name.clone()));
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

    fn render_term(&mut self, f: &mut Frame, r: Rect) {
        let p = self.pal;
        self.hit(r, Hit::Terminal);
        let center = |f: &mut Frame, lines: Vec<Line<'static>>| {
            let h = lines.len() as u16;
            let y = r.y + r.height.saturating_sub(h) / 3;
            f.render_widget(
                Paragraph::new(lines)
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(p.base()),
                Rect {
                    y,
                    height: h.min(r.height),
                    ..r
                },
            );
            y + h
        };
        let button = |app: &mut App, f: &mut Frame, y: u16, text: &str, hit: Hit| {
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
            app.hit(b, hit);
        };
        if !self.loaded {
            center(f, vec![Line::from(quips::loading(self.keys.wit))]);
            return;
        }
        let Some(project) = self.project.clone() else {
            let [hello, line] = quips::welcome(self.keys.wit);
            let mut lines = vec![
                Line::from(Span::styled(
                    hello,
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(line),
                Line::from(""),
            ];
            if !self.connected {
                lines.push(Line::from("No PCTL server is running yet."));
                lines.push(Line::from(
                    "Start it from a Herdr pane with pctl-server-herdr, or on a computer",
                ));
                lines.push(Line::from(
                    "without Herdr start pctl with PCTL_STANDALONE=1, then add a project.",
                ));
            }
            lines.push(Line::from(""));
            let y = center(f, lines);
            button(self, f, y, " + Project ", Hit::NewProject);
            return;
        };
        let Some(a) = self.current().cloned() else {
            let y = center(
                f,
                vec![
                    Line::from(format!("No agents in {project} yet.")),
                    Line::from(quips::empty_project(self.keys.wit)),
                    Line::from(""),
                ],
            );
            button(self, f, y, " + bash ", Hit::NewTab(project.clone()));
            button(
                self,
                f,
                y + 2,
                " + Agent (Claude Code, Codex, …) ",
                Hit::NewAgent(project),
            );
            return;
        };
        if a.status == "starting" {
            center(f, vec![Line::from(format!("Starting {}…", a.name))]);
            return;
        }
        if !a.running() {
            let y = center(
                f,
                vec![
                    Line::from(format!("{} is {}.", a.name, a.status)),
                    Line::from(""),
                ],
            );
            button(self, f, y, " Start ", Hit::Start(a.name.clone(), false));
            button(
                self,
                f,
                y + 2,
                " Start a new conversation ",
                Hit::Start(a.name.clone(), true),
            );
            return;
        }
        if self.view.pane != a.pane.clone().unwrap_or_default() {
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
        for (i, row) in self.view.rows.iter().take(r.height as usize).enumerate() {
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
        if self.view.scroll > 0 {
            let note = format!(
                " ↑ {} lines back · scroll down or type to return ",
                self.view.scroll
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
        } else if let (Some((cx, cy)), false, None) =
            (self.view.cursor, self.in_sidebar, &self.modal)
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
            left.push(Span::raw(format!(" {} / ", a.project)));
            left.push(Span::styled(g, gs));
            left.push(Span::raw(format!(" {} {}", a.name, a.status)));
        }
        // A fresh notice or the prefix hint goes first: it lasts seconds and may matter more.
        if self.prefix {
            left.push(Span::styled(
                format!("   {} …", self.keys.prefix.label()),
                Style::default().fg(p.warn),
            ));
        } else if !self.notice.is_empty() && self.notice_at.elapsed() < Duration::from_secs(8) {
            left.push(Span::styled(
                format!("   {}", self.notice),
                Style::default().fg(p.warn),
            ));
        }
        let blocked = self.agents.iter().filter(|a| a.status == "blocked").count();
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
                        "Mouse: click a project, agent or tab to open it; + adds; right-click an agent for",
                    ),
                    Line::from(
                        "restart, stop, start and remove; drag the sidebar edge; wheel scrolls the terminal.",
                    ),
                    Line::from(
                        "States (● idle or working, ! waiting for you, × exited, ○ stopped) are read from",
                    ),
                    Line::from("the screen: a best guess that can lag a moment."),
                    Line::from(""),
                ];
                for (_, _, action, text) in ACTIONS {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:>22}  ", self.keys.label(action)),
                            Style::default().fg(p.accent),
                        ),
                        Span::raw(text),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>22}  ", format!("{} 1…9", self.keys.prefix.label())),
                        Style::default().fg(p.accent),
                    ),
                    Span::raw("Tab 1 to 9"),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "{:>22}  ",
                            format!("{} {}", self.keys.prefix.label(), self.keys.prefix.label())
                        ),
                        Style::default().fg(p.accent),
                    ),
                    Span::raw("Send the prefix key itself to the agent"),
                ]));
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
                lines.push(Line::from(Span::styled(
                    quips::tip(self.keys.wit),
                    Style::default().fg(p.muted),
                )));
                let h = lines.len() as u16 + 2;
                let r = centered(area, 90, h);
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
                        format!("New agent in {}", form.project.clone().unwrap_or_default())
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
/// bash, as Ghostty starts it here: `$SHELL` when that is bash, else a Homebrew or system bash.
fn bash() -> String {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if Path::new(&shell).file_name().is_some_and(|n| n == "bash") {
        return shell;
    }
    ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .unwrap_or("bash")
        .to_string()
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
    #[test]
    fn names_typed_in_the_app_are_plain() {
        for ok in ["panel-main", "54_Agent_Panel", "a.b"] {
            assert!(super::plain_name(ok), "{ok}");
        }
        for bad in ["PS1='$ '; clear", "a b", "-x", ".hidden", "a/b", "한글"] {
            assert!(!super::plain_name(bad), "{bad}");
        }
    }
}
