//! A deliberately small manager UI. Agent programs keep their native tmux panes.
//!
//! Drafts remain local. PTY paste, Enter and Ctrl-C are separate, explicitly
//! confirmed actions routed through Core identity and capability guards.
use std::{
    io, panic,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    AgentView, Core, Snapshot,
    theme::{Palette, ThemeName},
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DRAFT_BYTES: usize = 128 * 1024;

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

#[derive(Clone, Debug)]
struct Target {
    agent_id: String,
    name: String,
    run_id: Option<String>,
}

impl Target {
    fn from_view(view: &AgentView) -> Self {
        Self {
            agent_id: view.agent.id.clone(),
            name: crate::sanitize(&view.agent.name),
            run_id: view.run.as_ref().map(|run| run.id.clone()),
        }
    }

    fn run_id(&self) -> Result<&str> {
        self.run_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!("{} has no current run. Press s to start it.", self.name)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Focus,
    Start,
    Zoom,
    Split,
    Pair,
    Draft,
    Terminate,
    Restart,
    Detach,
    NewProject,
    NewAgent,
    EditAgent,
    SendDraft,
    Submit,
    Interrupt,
    Inbox,
}

const ACTIONS: [(Action, &str); 16] = [
    (Action::Focus, "Enter  Focus native pane"),
    (Action::Inbox, "o      Read local inbox"),
    (Action::Start, "s      Start agent"),
    (Action::Zoom, "z      Toggle pane zoom"),
    (Action::Split, "b      Separate pane"),
    (Action::Pair, "p      Pair with next agent"),
    (Action::Draft, "d      Edit a local draft"),
    (Action::SendDraft, "S      Transmit saved draft…"),
    (Action::Submit, "u      Send Enter key…"),
    (Action::Interrupt, "i      Send Ctrl-C key…"),
    (Action::EditAgent, "e      Edit stopped profile"),
    (Action::NewAgent, "a      Register agent"),
    (Action::NewProject, "N      Register project"),
    (Action::Terminate, "x      Terminate run…"),
    (Action::Restart, "r      Restart run…"),
    (Action::Detach, "D      Detach client…"),
];

#[derive(Clone, Debug)]
enum Confirmation {
    Terminate(Target),
    Restart(Target),
    Detach(String),
    SendDraft(Target, String, bool),
    Submit(Target),
    Interrupt(Target),
}

#[derive(Clone, Debug)]
enum Mode {
    Normal,
    Help,
    Menu {
        target: Target,
        selected: usize,
        x: u16,
        y: u16,
    },
    Confirm(Confirmation),
    Draft {
        target: Target,
        text: String,
    },
    Form(ProfileForm),
    Inbox {
        target: Target,
        text: String,
        scroll: u16,
    },
}

#[derive(Clone, Debug)]
enum FormKind {
    Project,
    Agent {
        project_id: String,
        agent_id: Option<String>,
    },
}
#[derive(Clone, Debug)]
struct ProfileForm {
    kind: FormKind,
    values: Vec<String>,
    selected: usize,
    error: String,
}
impl ProfileForm {
    fn labels(&self) -> &[&str] {
        match self.kind {
            FormKind::Project => &["Project name", "Existing project directory"],
            FormKind::Agent { .. } => &[
                "Agent name",
                "Existing working directory",
                "Launch command (literal argv)",
                "Adapter: generic or synthetic",
                "Input mode: native or terminal-line",
                "Collaboration: disabled or mailbox",
            ],
        }
    }
}
#[derive(Default, Debug)]
struct HitAreas {
    projects: Vec<(Rect, String)>,
    agents: Vec<(Rect, String)>,
    menu: Vec<(Rect, Action)>,
    toolbar: Vec<(Rect, Action)>,
    form_fields: Vec<(Rect, usize)>,
    accept: Option<Rect>,
    cancel: Option<Rect>,
}

impl HitAreas {
    fn agent_at(&self, x: u16, y: u16) -> Option<&str> {
        self.agents
            .iter()
            .find(|(rect, _)| contains(*rect, x, y))
            .map(|(_, id)| id.as_str())
    }
    fn project_at(&self, x: u16, y: u16) -> Option<&str> {
        self.projects
            .iter()
            .find(|(rect, _)| contains(*rect, x, y))
            .map(|(_, id)| id.as_str())
    }
    fn menu_at(&self, x: u16, y: u16) -> Option<Action> {
        self.menu
            .iter()
            .find(|(rect, _)| contains(*rect, x, y))
            .map(|(_, action)| *action)
    }
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

struct App {
    theme: ThemeName,
    snapshot: Snapshot,
    filter: Option<String>,
    project: Option<String>,
    agent: Option<String>,
    mode: Mode,
    notice: String,
    hits: HitAreas,
    confirmation_visible: bool,
}

impl App {
    fn new(snapshot: Snapshot, filter: Option<String>) -> Self {
        let mut app = Self {
            theme: ThemeName::default(),
            snapshot,
            filter: None,
            project: filter,
            agent: None,
            mode: Mode::Normal,
            notice: String::new(),
            hits: HitAreas::default(),
            confirmation_visible: false,
        };
        app.reconcile_selection();
        app
    }

    fn project_ids(&self) -> Vec<String> {
        self.snapshot
            .projects
            .iter()
            .filter(|project| {
                self.filter
                    .as_ref()
                    .is_none_or(|filter| project.id == *filter || project.name == *filter)
            })
            .map(|project| project.id.clone())
            .collect()
    }

    fn views(&self) -> Vec<&AgentView> {
        self.snapshot
            .agents
            .iter()
            .filter(|view| self.project.as_deref() == Some(view.agent.project_id.as_str()))
            .collect()
    }

    fn current_target(&self) -> Option<Target> {
        self.agent
            .as_ref()
            .and_then(|id| {
                self.snapshot
                    .agents
                    .iter()
                    .find(|view| view.agent.id == *id)
            })
            .map(Target::from_view)
    }

    fn reconcile_selection(&mut self) {
        let projects = self.project_ids();
        if !self
            .project
            .as_ref()
            .is_some_and(|id| projects.contains(id))
        {
            self.project = projects.first().cloned();
        }
        let agents: Vec<String> = self
            .views()
            .iter()
            .map(|view| view.agent.id.clone())
            .collect();
        if !self.agent.as_ref().is_some_and(|id| agents.contains(id)) {
            self.agent = agents.first().cloned();
        }
    }

    fn move_project(&mut self, delta: isize) {
        let ids = self.project_ids();
        self.project = moved_selection(&ids, self.project.as_deref(), delta);
        self.reconcile_selection();
    }

    fn move_agent(&mut self, delta: isize) {
        let ids: Vec<String> = self
            .views()
            .iter()
            .map(|view| view.agent.id.clone())
            .collect();
        self.agent = moved_selection(&ids, self.agent.as_deref(), delta);
    }

    fn open_menu(&mut self, target: Target, x: u16, y: u16) {
        self.mode = Mode::Menu {
            target,
            selected: 0,
            x,
            y,
        };
    }

    fn report(&mut self, result: Result<()>, success: &str) {
        self.notice = match result {
            Ok(()) => success.to_owned(),
            Err(error) => format!("Error: {}", crate::sanitize(&error.to_string())),
        };
    }

    fn refresh(&mut self, core: &Core) {
        match core.snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.reconcile_selection();
            }
            Err(error) => invalidate_snapshot(&mut self.snapshot, error.to_string()),
        }
    }
    fn open_form(&mut self, action: Action, target: Option<&Target>) {
        let form = match action {
            Action::NewProject => ProfileForm {
                kind: FormKind::Project,
                values: vec![
                    String::new(),
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ],
                selected: 0,
                error: String::new(),
            },
            Action::NewAgent => {
                let Some(project) = self
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| Some(&p.id) == self.project.as_ref())
                else {
                    self.notice = "Register a project first with N.".into();
                    return;
                };
                ProfileForm {
                    kind: FormKind::Agent {
                        project_id: project.id.clone(),
                        agent_id: None,
                    },
                    values: vec![
                        String::new(),
                        project.root.clone(),
                        String::new(),
                        "generic".into(),
                        "native".into(),
                        "disabled".into(),
                    ],
                    selected: 0,
                    error: String::new(),
                }
            }
            Action::EditAgent => {
                let Some(view) = target.and_then(|t| {
                    self.snapshot
                        .agents
                        .iter()
                        .find(|v| v.agent.id == t.agent_id)
                }) else {
                    return;
                };
                if view.run.is_some() || target.is_some_and(|t| t.run_id.is_some()) {
                    self.notice =
                        "Terminate the current run explicitly before editing its profile.".into();
                    return;
                }
                let a = &view.agent;
                ProfileForm {
                    kind: FormKind::Agent {
                        project_id: a.project_id.clone(),
                        agent_id: Some(a.id.clone()),
                    },
                    values: vec![
                        a.name.clone(),
                        a.cwd.clone(),
                        a.argv
                            .iter()
                            .map(|s| crate::shell_quote(s))
                            .collect::<Vec<_>>()
                            .join(" "),
                        a.adapter.clone(),
                        a.input_mode.clone(),
                        a.collaboration_mode.clone(),
                    ],
                    selected: 0,
                    error: String::new(),
                }
            }
            _ => return,
        };
        self.mode = Mode::Form(form);
    }
    fn save_form(&mut self, core: &Core, mut form: ProfileForm) {
        let result = (|| -> Result<()> {
            match &form.kind {
                FormKind::Project => {
                    let project =
                        core.register_project(&form.values[0], Path::new(&form.values[1]))?;
                    self.project = Some(project.id);
                }
                FormKind::Agent {
                    project_id,
                    agent_id,
                } => {
                    let argv = crate::parse_command(&form.values[2])?;
                    anyhow::ensure!(
                        ["native", "terminal-line"].contains(&form.values[4].as_str()),
                        "Input mode must be native or terminal-line."
                    );
                    anyhow::ensure!(
                        ["disabled", "mailbox"].contains(&form.values[5].as_str()),
                        "Collaboration must be disabled or mailbox; this is capability, not action permission."
                    );
                    let a = if let Some(id) = agent_id {
                        core.update_agent(
                            id,
                            &form.values[0],
                            Path::new(&form.values[1]),
                            argv,
                            &form.values[3],
                        )?
                    } else {
                        core.register_agent(
                            project_id,
                            &form.values[0],
                            Path::new(&form.values[1]),
                            argv,
                            &form.values[3],
                        )?
                    };
                    // Persist the new identity before a possible capability configuration
                    // failure, so retrying Save cannot register a duplicate profile.
                    form.kind = FormKind::Agent {
                        project_id: a.project_id.clone(),
                        agent_id: Some(a.id.clone()),
                    };
                    core.configure_input(&a.id, &form.values[4])?;
                    core.configure_collaboration(&a.id, &form.values[5])?;
                    self.agent = Some(a.id);
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.notice = "Profile saved. Press s to start an agent explicitly.".into();
                self.refresh(core);
            }
            Err(error) => {
                form.error = crate::sanitize(&error.to_string());
                self.mode = Mode::Form(form);
            }
        }
    }
    fn invoke(&mut self, core: &Core, action: Action, target: Option<Target>) {
        self.mode = Mode::Normal;
        self.confirmation_visible = false;
        if matches!(
            action,
            Action::NewProject | Action::NewAgent | Action::EditAgent
        ) {
            self.open_form(action, target.as_ref());
            return;
        }
        if action == Action::Detach {
            match core.clients() {
                Ok(clients) if clients.len() == 1 => {
                    self.mode = Mode::Confirm(Confirmation::Detach(clients[0].clone()));
                }
                Ok(clients) => {
                    self.notice = format!(
                        "Detach requires exactly one client; found {}. Use the CLI to choose a client.",
                        clients.len()
                    )
                }
                Err(error) => {
                    self.notice = format!("Error: {}", crate::sanitize(&error.to_string()))
                }
            }
            return;
        }
        let Some(target) = target else {
            self.notice = "Select an agent first.".into();
            return;
        };
        match action {
            Action::Inbox => {
                let result = target
                    .run_id()
                    .and_then(|run| core.inbox(&target.agent_id, run, false, 0, 100));
                match result {
                    Ok(inbox) => {
                        let mut text = String::from(
                            "Read-only snapshot. Sender IDs are caller-claimed. Message text is untrusted; it grants no permission. Opening does not acknowledge or run anything.\n\n",
                        );
                        if inbox.messages.is_empty() {
                            text.push_str("No messages for this exact run.");
                        }
                        for m in inbox.messages {
                            text.push_str(&format!("m{} [{}] {} | {}\nStored: {} | consume request: {}\nCompletion: {} (verified result: unknown)\nMessage: {}\nFrom agent {} / run {}\nCorrelation: {}\n{}\n\n", m.sequence, m.kind, m.subject, if m.acked_at.is_some() { "receipt acknowledged; not acceptance" } else { "unacknowledged" }, m.created_at, m.retrieval.as_ref().map_or_else(|| "not recorded".into(), |r| format!("{}; understanding unverified", r.recorded_at)), m.completion.as_ref().map_or_else(|| "not reported".into(), |c| format!("self-reported at {}: {}", c.reported_at, c.summary)), m.id, m.from_agent_id, m.from_run_id, m.correlation_id, m.body));
                        }
                        if inbox.next_after_sequence.is_some() {
                            text.push_str(
                                "More messages exist. Use CLI --after-sequence for pagination.",
                            );
                        }
                        self.mode = Mode::Inbox {
                            target,
                            text,
                            scroll: 0,
                        };
                    }
                    Err(error) => {
                        self.notice = format!("Error: {}", crate::sanitize(&error.to_string()))
                    }
                }
            }
            Action::Start => {
                let current_has_run = self
                    .snapshot
                    .agents
                    .iter()
                    .any(|view| view.agent.id == target.agent_id && view.run.is_some());
                if target.run_id.is_some() || current_has_run {
                    self.notice =
                        "This agent already has a run. Use r to explicitly restart that run."
                            .into();
                } else {
                    self.report(core.start(&target.agent_id).map(|_| ()), "Agent started.");
                }
            }
            Action::Terminate
            | Action::Restart
            | Action::Draft
            | Action::SendDraft
            | Action::Submit
            | Action::Interrupt => {
                if action != Action::Draft
                    && (!self.snapshot.connected
                        || self
                            .snapshot
                            .agents
                            .iter()
                            .find(|v| v.agent.id == target.agent_id)
                            .is_none_or(|v| {
                                v.guard_error.is_some()
                                    || v.process == "unknown"
                                    || v.process == "missing"
                            }))
                {
                    self.notice =
                        "Controls locked: refresh and inspect this run before changing it.".into();
                    return;
                }
                if let Err(error) = target.run_id() {
                    self.notice = error.to_string();
                    return;
                }
                self.mode = match action {
                    Action::Terminate => Mode::Confirm(Confirmation::Terminate(target)),
                    Action::Restart => Mode::Confirm(Confirmation::Restart(target)),
                    Action::Interrupt => Mode::Confirm(Confirmation::Interrupt(target)),
                    Action::Submit => {
                        let enabled = self.snapshot.agents.iter().any(|v| {
                            v.agent.id == target.agent_id && v.agent.input_mode == "terminal-line"
                        });
                        if !enabled {
                            self.notice = "Enter delivery is disabled. Enable terminal-line in a stopped profile or use the native pane.".into();
                            return;
                        }
                        Mode::Confirm(Confirmation::Submit(target))
                    }
                    Action::SendDraft => {
                        let view = self
                            .snapshot
                            .agents
                            .iter()
                            .find(|v| v.agent.id == target.agent_id)
                            .unwrap();
                        let fixture = view.agent.adapter == "synthetic";
                        if !fixture && view.agent.input_mode != "terminal-line" {
                            self.notice = "Draft transmission is disabled. Enable terminal-line in a stopped profile or use the native pane.".into();
                            return;
                        }
                        match core.load_draft(&target.agent_id, target.run_id().unwrap()) {
                            Ok(Some(text))
                                if !text.is_empty()
                                    && text.len() <= 4096
                                    && !text.chars().any(char::is_control) =>
                            {
                                Mode::Confirm(Confirmation::SendDraft(target, text, fixture))
                            }
                            Ok(Some(_)) => {
                                self.notice = "Transmission requires a saved, nonempty printable line (up to 4096 bytes). Multiline drafts stay local.".into();
                                return;
                            }
                            Ok(None) => {
                                self.notice = "No saved draft. Press d, write a line, then Ctrl-S to save locally.".into();
                                return;
                            }
                            Err(error) => {
                                self.notice = format!("Error: {error}");
                                return;
                            }
                        }
                    }
                    _ => match core.load_draft(&target.agent_id, target.run_id().unwrap()) {
                        Ok(text) => Mode::Draft {
                            target,
                            text: text.unwrap_or_default(),
                        },
                        Err(error) => {
                            self.notice = format!("Error: {error}");
                            return;
                        }
                    },
                };
            }
            Action::Pair => {
                let result = (|| {
                    let run_id = target.run_id()?;
                    let current = self
                        .snapshot
                        .agents
                        .iter()
                        .find(|view| view.agent.id == target.agent_id)
                        .ok_or_else(|| anyhow::anyhow!("Agent is no longer registered."))?;
                    let candidates: Vec<_> = self
                        .snapshot
                        .agents
                        .iter()
                        .filter(|view| view.agent.project_id == current.agent.project_id)
                        .collect();
                    let index = candidates
                        .iter()
                        .position(|view| view.agent.id == target.agent_id)
                        .ok_or_else(|| anyhow::anyhow!("Agent is no longer in this project."))?;
                    if candidates.len() < 2 {
                        anyhow::bail!("This project needs a second agent to pair panes.");
                    }
                    let other = candidates[(index + 1) % candidates.len()];
                    let other_run = other.run.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "The next agent ({}) has no current run.",
                            crate::sanitize(&other.agent.name)
                        )
                    })?;
                    core.pair(&target.agent_id, run_id, &other.agent.id, &other_run.id)
                })();
                self.report(result, "Paired agent panes.");
            }
            _ => {
                let result = target.run_id().and_then(|run_id| match action {
                    Action::Focus => core.focus(&target.agent_id, run_id),
                    Action::Zoom => core.zoom(&target.agent_id, run_id),
                    Action::Split => core.split(&target.agent_id, run_id),
                    _ => unreachable!(),
                });
                let success = match action {
                    Action::Focus => "Native agent pane focused.",
                    Action::Zoom => "Pane zoom toggled.",
                    _ => "Agent pane separated.",
                };
                self.report(result, success);
            }
        }
    }

    fn confirm(&mut self, core: &Core, confirmation: Confirmation) {
        self.mode = Mode::Normal;
        match confirmation {
            Confirmation::Terminate(target) => self.report(
                target
                    .run_id()
                    .and_then(|run| core.terminate(&target.agent_id, run)),
                "Run terminated.",
            ),
            Confirmation::Restart(target) => self.report(
                target
                    .run_id()
                    .and_then(|run| core.restart(&target.agent_id, run).map(|_| ())),
                "Agent restarted with a new run.",
            ),
            Confirmation::Detach(client) => self.report(core.detach(&client), "Client detached."),
            Confirmation::Interrupt(target) => self.report(target.run_id().and_then(|run| core.send_ctrl_c(&target.agent_id, run)), "Ctrl-C key delivered. Interruption is unverified."),
            Confirmation::Submit(target) => self.report(target.run_id().and_then(|run| core.submit_line(&target.agent_id, run)), "Enter key delivered. Acceptance is unverified."),
            Confirmation::SendDraft(target, text, fixture) => self.report(target.run_id().and_then(|run| {
                if fixture { core.send_synthetic(&target.agent_id, run, &text) }
                else { core.paste_line(&target.agent_id, run, &text) }
            }), if fixture { "Fixture line delivered with newline. Acceptance unverified." } else { "Text transmitted without Enter. It may act immediately. u sends Enter separately." }),
        }
    }

    /// Returns true only when the manager UI should quit.
    fn key(&mut self, core: &Core, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        // Ctrl-C always leaves only the manager, never the managed agents.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        // Modified shortcuts must not accidentally start or manipulate an agent.
        if !matches!(self.mode, Mode::Draft { .. } | Mode::Form(_))
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Help => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.mode = Mode::Help;
                }
            }
            Mode::Inbox {
                target,
                text,
                mut scroll,
            } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return false,
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    _ => {}
                }
                self.mode = Mode::Inbox {
                    target,
                    text,
                    scroll,
                };
            }
            Mode::Form(mut form) => {
                if key.code == KeyCode::Esc {
                    self.notice = "Registration/edit cancelled. Nothing started.".into();
                    return false;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
                    self.save_form(core, form);
                    return false;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
                    form.values[form.selected].clear();
                } else {
                    match key.code {
                        KeyCode::Tab | KeyCode::Down => {
                            form.selected = (form.selected + 1) % form.values.len()
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            form.selected =
                                (form.selected + form.values.len() - 1) % form.values.len()
                        }
                        KeyCode::Enter => {
                            if form.selected + 1 == form.values.len() {
                                self.save_form(core, form);
                                return false;
                            }
                            form.selected += 1;
                        }
                        KeyCode::Backspace => {
                            form.values[form.selected].pop();
                        }
                        KeyCode::Char(ch)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && !ch.is_control()
                                && form.values[form.selected].len() < MAX_DRAFT_BYTES =>
                        {
                            form.values[form.selected].push(ch)
                        }
                        _ => {}
                    }
                }
                self.mode = Mode::Form(form);
            }
            Mode::Draft { target, mut text } => {
                if key.code == KeyCode::Esc {
                    self.notice = "Unsaved draft discarded. Nothing was sent.".into();
                    return false;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
                    let result = target
                        .run_id()
                        .and_then(|run| core.save_draft(&target.agent_id, run, &text));
                    let saved = result.is_ok();
                    self.report(
                        result,
                        "Draft saved locally. Nothing was sent to the agent.",
                    );
                    if !saved {
                        self.mode = Mode::Draft { target, text };
                    }
                    return false;
                }
                match key.code {
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Enter if text.len() < MAX_DRAFT_BYTES => text.push('\n'),
                    KeyCode::Tab if text.len() + 4 <= MAX_DRAFT_BYTES => text.push_str("    "),
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && !ch.is_control()
                            && text.len() + ch.len_utf8() <= MAX_DRAFT_BYTES =>
                    {
                        text.push(ch)
                    }
                    _ => {}
                }
                self.mode = Mode::Draft { target, text };
            }
            Mode::Confirm(confirmation) => match key.code {
                KeyCode::Char('y') if self.confirmation_visible => self.confirm(core, confirmation),
                KeyCode::Char('y') => {
                    self.notice =
                        "Resize to at least 72 × 14 to review this action. Nothing was sent."
                            .into();
                    self.mode = Mode::Confirm(confirmation);
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
                    self.notice = "Cancelled.".into();
                }
                _ => self.mode = Mode::Confirm(confirmation),
            },
            Mode::Menu {
                target,
                mut selected,
                x,
                y,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = (selected + ACTIONS.len() - 1) % ACTIONS.len();
                    self.mode = Mode::Menu {
                        target,
                        selected,
                        x,
                        y,
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % ACTIONS.len();
                    self.mode = Mode::Menu {
                        target,
                        selected,
                        x,
                        y,
                    };
                }
                KeyCode::Enter => self.invoke(core, ACTIONS[selected].0, Some(target)),
                _ => {
                    if let Some(action) = key_action(key.code) {
                        self.invoke(core, action, Some(target));
                    } else {
                        self.mode = Mode::Menu {
                            target,
                            selected,
                            x,
                            y,
                        };
                    }
                }
            },
            Mode::Normal => match key.code {
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('t') => {
                    let next = self.theme.next();
                    match core.set_theme(next) {
                        Ok(()) => {
                            self.theme = next;
                            self.notice =
                                format!("Theme: {} · saved for this state directory", next.label());
                        }
                        Err(e) => self.notice = format!("Theme update failed: {e}"),
                    }
                }
                KeyCode::Char('q') => return true,
                KeyCode::Up | KeyCode::Char('k') => self.move_agent(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_agent(1),
                KeyCode::Left | KeyCode::BackTab => self.move_project(-1),
                KeyCode::Right | KeyCode::Tab => self.move_project(1),
                KeyCode::Char('m') => {
                    if let Some(target) = self.current_target() {
                        let anchor = self
                            .hits
                            .agents
                            .iter()
                            .find(|(_, id)| *id == target.agent_id)
                            .map(|(rect, _)| (rect.x.saturating_add(2), rect.y))
                            .unwrap_or((2, 2));
                        self.open_menu(target, anchor.0, anchor.1);
                    }
                }
                _ => {
                    if let Some(action) = key_action(key.code) {
                        self.invoke(core, action, self.current_target());
                    }
                }
            },
        }
        false
    }

    fn paste(&mut self, pasted: &str) {
        if let Mode::Form(form) = &mut self.mode {
            for ch in pasted.chars().filter(|ch| !ch.is_control()) {
                if form.values[form.selected].len() + ch.len_utf8() > MAX_DRAFT_BYTES {
                    break;
                }
                form.values[form.selected].push(ch);
            }
            return;
        }
        if let Mode::Draft { text, .. } = &mut self.mode {
            // Preserve ordinary multiline text, reject escape/control sequences.
            for ch in pasted
                .chars()
                .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
            {
                if text.len() + ch.len_utf8() > MAX_DRAFT_BYTES {
                    break;
                }
                text.push(ch);
            }
        }
    }

    fn mouse(&mut self, core: &Core, mouse: MouseEvent) {
        if let Mode::Inbox { scroll, .. } = &mut self.mode {
            match mouse.kind {
                MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                _ => {}
            }
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if !matches!(self.mode, Mode::Normal | Mode::Menu { .. })
                && self
                    .hits
                    .cancel
                    .is_some_and(|r| contains(r, mouse.column, mouse.row))
            {
                self.mode = Mode::Normal;
                self.notice = "Cancelled.".into();
                return;
            }
            if matches!(self.mode, Mode::Confirm(_) | Mode::Form(_))
                && self
                    .hits
                    .accept
                    .is_some_and(|r| contains(r, mouse.column, mouse.row))
            {
                let mode = std::mem::replace(&mut self.mode, Mode::Normal);
                match mode {
                    Mode::Confirm(c) if self.confirmation_visible => self.confirm(core, c),
                    Mode::Form(f) => self.save_form(core, f),
                    _ => self.mode = mode,
                }
                return;
            }
            if let Mode::Form(form) = &mut self.mode {
                if let Some((_, index)) = self
                    .hits
                    .form_fields
                    .iter()
                    .find(|(r, _)| contains(*r, mouse.column, mouse.row))
                {
                    form.selected = *index;
                }
                return;
            }
            if matches!(self.mode, Mode::Normal)
                && let Some((_, action)) = self
                    .hits
                    .toolbar
                    .iter()
                    .find(|(r, _)| contains(*r, mouse.column, mouse.row))
            {
                self.invoke(core, *action, self.current_target());
                return;
            }
        }
        if let Mode::Menu { target, .. } = &self.mode {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let target = target.clone();
                if let Some(action) = self.hits.menu_at(mouse.column, mouse.row) {
                    self.invoke(core, action, Some(target));
                } else {
                    self.mode = Mode::Normal;
                }
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                self.mode = Mode::Normal;
            }
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(id) = self
                    .hits
                    .project_at(mouse.column, mouse.row)
                    .map(str::to_owned)
                {
                    self.project = Some(id);
                    self.reconcile_selection();
                } else if let Some(id) = self
                    .hits
                    .agent_at(mouse.column, mouse.row)
                    .map(str::to_owned)
                {
                    self.agent = Some(id);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Only the exact clicked agent row can become the menu target.
                if let Some(id) = self
                    .hits
                    .agent_at(mouse.column, mouse.row)
                    .map(str::to_owned)
                {
                    self.agent = Some(id);
                    if let Some(target) = self.current_target() {
                        self.open_menu(target, mouse.column, mouse.row);
                    }
                }
            }
            MouseEventKind::ScrollUp => self.move_agent(-1),
            MouseEventKind::ScrollDown => self.move_agent(1),
            _ => {}
        }
    }
}

fn key_action(code: KeyCode) -> Option<Action> {
    Some(match code {
        KeyCode::Enter => Action::Focus,
        KeyCode::Char('s') => Action::Start,
        KeyCode::Char('z') => Action::Zoom,
        KeyCode::Char('b') => Action::Split,
        KeyCode::Char('p') => Action::Pair,
        KeyCode::Char('d') => Action::Draft,
        KeyCode::Char('o') => Action::Inbox,
        KeyCode::Char('S') => Action::SendDraft,
        KeyCode::Char('u') => Action::Submit,
        KeyCode::Char('i') => Action::Interrupt,
        KeyCode::Char('e') => Action::EditAgent,
        KeyCode::Char('a') => Action::NewAgent,
        KeyCode::Char('N') => Action::NewProject,
        KeyCode::Char('x') => Action::Terminate,
        KeyCode::Char('r') => Action::Restart,
        KeyCode::Char('D') => Action::Detach,
        _ => return None,
    })
}

fn moved_selection(ids: &[String], current: Option<&str>, delta: isize) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let index = ids
        .iter()
        .position(|id| Some(id.as_str()) == current)
        .unwrap_or(0);
    let next = (index as isize + delta).rem_euclid(ids.len() as isize) as usize;
    Some(ids[next].clone())
}

/// Observation time changes alone do not cause a redraw. The latest observations
/// are still retained for the next user interaction or actual state change.
fn semantically_equal(left: &Snapshot, right: &Snapshot) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    for view in &mut left.agents {
        view.observed_at = 0;
    }
    for view in &mut right.agents {
        view.observed_at = 0;
    }
    left == right
}

/// A failed refresh must never leave an old live/task state presented as current.
/// Run identity remains available solely for Core's guarded actions.
pub(crate) fn invalidate_snapshot(snapshot: &mut Snapshot, error: String) {
    snapshot.connected = false;
    snapshot.error = Some(crate::sanitize(&error));
    for view in &mut snapshot.agents {
        view.process = "unknown".into();
        view.task = "unknown".into();
        view.evidence = "snapshot unavailable; previous observation is stale".into();
        if let Some(report) = &mut view.report {
            report.fresh = false;
        }
        view.guard_error = Some("Current target could not be verified; refresh failed.".into());
    }
}

#[derive(Clone, Copy, Debug)]
struct Areas {
    header: Rect,
    projects: Rect,
    agents: Rect,
    details: Rect,
    footer: Rect,
}

fn areas(area: Rect) -> Areas {
    let detail_height = if area.height >= 26 {
        9
    } else if area.height >= 16 {
        7
    } else if area.height >= 10 {
        4
    } else {
        0
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if area.height >= 18 { 3 } else { 1 }),
            Constraint::Min(0),
            Constraint::Length(detail_height),
            Constraint::Length(if area.height >= 18 {
                3
            } else if area.height >= 5 {
                2
            } else {
                1
            }),
        ])
        .split(area);
    let columns = if area.width >= 55 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length((area.width / 4).clamp(18, 28)),
                Constraint::Min(0),
            ])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if rows[1].height >= 6 { 3 } else { 0 }),
                Constraint::Min(0),
            ])
            .split(rows[1])
    };
    Areas {
        header: rows[0],
        projects: columns[0],
        agents: columns[1],
        details: rows[2],
        footer: rows[3],
    }
}

fn selection_style(palette: Palette) -> Style {
    palette.selected()
}

/// Truncate by terminal display cells, including wide Unicode characters.
pub(crate) fn fit_label(text: &str, width: u16) -> String {
    let clean = crate::sanitize(text);
    if Line::raw(&clean).width() <= width as usize {
        return clean;
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let span = Span::raw(&clean);
    for grapheme in span.styled_graphemes(Style::default()) {
        let mut candidate = out.clone();
        candidate.push_str(grapheme.symbol);
        if Line::raw(&candidate).width() > width.saturating_sub(1) as usize {
            break;
        }
        out.push_str(grapheme.symbol);
    }
    out.push('…');
    out
}

fn state_style(palette: Palette, process: &str, guarded: bool) -> Style {
    palette.body().fg(if guarded {
        palette.danger
    } else if process == "running" {
        palette.accent
    } else if process.starts_with("exited") {
        palette.warn
    } else {
        palette.muted
    })
}

fn visible_start(selected: usize, count: usize, rows: usize) -> usize {
    if rows == 0 || count <= rows {
        0
    } else {
        selected.saturating_sub(rows - 1).min(count - rows)
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let layout = areas(area);
    let p = app.theme.palette();
    frame.render_widget(Block::default().style(p.base()), area);
    // Hit rectangles are rebuilt from the current frame, including every resize.
    app.hits = HitAreas::default();
    app.confirmation_visible = false;
    let connected = app.snapshot.connected;
    let project_name = app
        .snapshot
        .projects
        .iter()
        .find(|project| Some(&project.id) == app.project.as_ref())
        .map(|project| crate::sanitize(&project.name))
        .unwrap_or_else(|| "no project".into());
    let title = format!(" ◎ pctl  /  {} CONTROL", app.theme.label());
    frame.render_widget(Paragraph::new(title).style(p.accent()), layout.header);
    if layout.header.width >= 62 {
        let x = layout.header.right() - 26;
        frame.render_widget(
            Paragraph::new("[N Project]  [a Agent]").style(p.body().fg(p.cyan)),
            Rect::new(x, layout.header.y, 26, 1),
        );
        app.hits
            .toolbar
            .push((Rect::new(x, layout.header.y, 11, 1), Action::NewProject));
        app.hits
            .toolbar
            .push((Rect::new(x + 13, layout.header.y, 10, 1), Action::NewAgent));
    }
    if layout.header.height >= 3 {
        let meta = Line::from(vec![
            Span::styled("   PROJECT ", p.muted()),
            Span::styled(
                fit_label(&project_name, layout.header.width.saturating_sub(42)),
                p.body(),
            ),
            Span::styled(
                if connected {
                    "   ● connected"
                } else {
                    "   ! stale / offline"
                },
                p.body().fg(if connected { p.cyan } else { p.warn }),
            ),
            Span::styled("   native tmux", p.muted()),
        ]);
        frame.render_widget(
            Paragraph::new(meta),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        frame.render_widget(
            Paragraph::new("─".repeat(area.width as usize)).style(p.base().fg(p.border)),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
    render_projects(frame, app, layout.projects);
    render_agents(frame, app, layout.agents);
    render_details(frame, app, layout.details);
    let notice = if !app.notice.is_empty() {
        app.notice.clone()
    } else if let Some(error) = &app.snapshot.error {
        format!("! {}", crate::sanitize(error))
    } else {
        "Enter focus native pane · m / right-click actions · ? help · t theme".into()
    };
    let footer = vec![
        Line::from(vec![
            Span::styled("  ", p.base()),
            Span::styled(
                notice,
                if app.snapshot.error.is_some() {
                    p.base().fg(p.warn)
                } else {
                    p.base().fg(p.text)
                },
            ),
        ]),
        Line::styled(
            "  ↑↓ agents  Tab projects  s start  o inbox  d draft  S transmit  q quit UI",
            p.base().fg(p.muted),
        ),
        Line::from(vec![
            Span::styled("  ◎ ", p.base().fg(p.accent)),
            Span::styled(
                "Native screens stay native. Every action targets an exact run.",
                p.base().fg(p.muted),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(footer), layout.footer);
    match app.mode.clone() {
        Mode::Normal => {}
        Mode::Help => render_help(frame, app),
        Mode::Menu {
            target,
            selected,
            x,
            y,
        } => render_menu(frame, app, &target, selected, x, y),
        Mode::Confirm(confirmation) => render_confirmation(frame, app, confirmation),
        Mode::Form(form) => render_form(frame, app, &form),
        Mode::Draft { target, text } => render_draft(frame, app, &target, &text),
        Mode::Inbox {
            target,
            text,
            scroll,
        } => render_inbox(frame, app, &target, &text, scroll),
    }
}

fn render_help(frame: &mut Frame, app: &mut App) {
    let p = app.theme.palette();
    let area = popup(frame.area(), 86, 25, None);
    let block = p.block(" ◎ CONTROL GUIDE · ? / Esc close ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let lines = vec![
        Line::styled("NAVIGATE", p.accent()),
        Line::raw("↑↓ / j k  select agent     Tab / ← →  switch project"),
        Line::raw("Enter     focus native pane     m / right-click  action menu"),
        Line::raw("Ctrl-b m  native tmux → manager     Ctrl-b d  detach"),
        Line::raw(""),
        Line::styled(
            "LOCAL WORK",
            p.body().fg(p.cyan).add_modifier(Modifier::BOLD),
        ),
        Line::raw("N  new project     a  register agent     e  stopped profile"),
        Line::raw("d  local draft     Ctrl-S  save draft     o  read-only inbox"),
        Line::raw(""),
        Line::styled(
            "EXPLICIT RUN CONTROLS",
            p.body().fg(p.warn).add_modifier(Modifier::BOLD),
        ),
        Line::raw("s start   z zoom   b separate   p pair   x terminate   r restart"),
        Line::raw("S transmit draft   u send Enter   i send Ctrl-C   D detach"),
        Line::raw("Transmission / lifecycle warnings require separate confirmation."),
        Line::raw("Task unknown means no trustworthy task evidence. Running ≠ working."),
        Line::raw("Self-reported status is labeled separately. Inspect native approvals."),
        Line::raw(""),
        Line::styled("APPEARANCE", p.accent()),
        Line::raw("t  cycle Portal / Classic / Mono; saved in this state directory"),
        Line::raw("CLI: pctl --state STATE theme portal|classic|mono"),
        Line::raw("Static, no decorative refresh. Only pctl chrome is themed."),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(p.body())
            .wrap(Wrap { trim: false }),
        inner,
    );
    if inner.height > 0 {
        let close = Rect::new(inner.x, inner.bottom() - 1, inner.width.min(11), 1);
        frame.render_widget(Paragraph::new("[Esc Close]").style(p.accent()), close);
        app.hits.cancel = Some(close);
    }
}

fn render_inbox(frame: &mut Frame, app: &mut App, target: &Target, text: &str, scroll: u16) {
    let area = popup(frame.area(), 100, 28, None);
    let block = app.theme.palette().block(format!(
        " {} · read-only inbox snapshot · no automatic actions ",
        target.name
    ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let lines: Vec<_> = text
        .split('\n')
        .map(|line| Line::raw(crate::sanitize(line)))
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        body,
    );
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new("[Esc Close]  ↑↓/wheel scroll · close and reopen to refresh"),
        footer,
    );
    app.hits.cancel = Some(Rect::new(footer.x, footer.y, 11.min(footer.width), 1));
}

fn render_projects(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = app.theme.palette();
    let block = p.block(" 01 / Projects ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let projects: Vec<_> = app
        .snapshot
        .projects
        .iter()
        .filter(|project| {
            app.filter
                .as_ref()
                .is_none_or(|filter| project.id == *filter || project.name == *filter)
        })
        .collect();
    if projects.is_empty() {
        frame.render_widget(
            Paragraph::new(" N: register project").style(p.muted()),
            inner,
        );
        return;
    }
    let row_height = if inner.height >= 10 { 3 } else { 1 };
    let capacity = inner.height as usize / row_height;
    let selected = projects
        .iter()
        .position(|project| Some(&project.id) == app.project.as_ref())
        .unwrap_or(0);
    let start = visible_start(selected, projects.len(), capacity);
    for (row, project) in projects.iter().skip(start).take(capacity).enumerate() {
        let y = inner.y + (row * row_height) as u16;
        let active = Some(&project.id) == app.project.as_ref();
        let style = if active {
            p.body()
                .bg(p.raised)
                .fg(p.cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            p.body()
        };
        let marker = if active { " ▸ " } else { "   " };
        frame.render_widget(
            Paragraph::new(format!(
                "{marker}{}",
                fit_label(&project.name, inner.width.saturating_sub(3))
            ))
            .style(style),
            Rect::new(inner.x, y, inner.width, 1),
        );
        if row_height > 1 {
            let count = app
                .snapshot
                .agents
                .iter()
                .filter(|a| a.agent.project_id == project.id)
                .count();
            frame.render_widget(
                Paragraph::new(format!("   {count} registered agents")).style(p.muted()),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
        }
        app.hits.projects.push((
            Rect::new(inner.x, y, inner.width, if row_height > 1 { 2 } else { 1 }),
            project.id.clone(),
        ));
    }
    let used = ((projects.len().min(capacity)) * row_height) as u16;
    if app.theme == ThemeName::Portal && inner.height >= used + 6 && inner.width >= 20 {
        let y = inner.bottom() - 5;
        for (i, line) in [
            "       .-((◎))-.",
            "      (  PORTAL  )",
            "      ( CONTROL )",
            "       ·-------·",
            "   C-137 // CONTROL",
        ]
        .iter()
        .enumerate()
        {
            frame.render_widget(
                Paragraph::new(*line).style(p.body().fg(if i == 4 { p.muted } else { p.cyan })),
                Rect::new(inner.x, y + i as u16, inner.width, 1),
            );
        }
    }
}

fn render_agents(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = app.theme.palette();
    let count = app.views().len();
    let block = p.block(format!(" 02 / Agents  ·  {count} registered "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let views = app.views();
    if views.is_empty() {
        frame.render_widget(
            Paragraph::new(
                " a: register an agent
 Profiles never start automatically.",
            )
            .style(p.muted())
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    let row_height = if inner.height >= 6 && inner.width >= 38 {
        3
    } else {
        1
    };
    let capacity = inner.height as usize / row_height;
    let selected = views
        .iter()
        .position(|v| Some(&v.agent.id) == app.agent.as_ref())
        .unwrap_or(0);
    let start = visible_start(selected, views.len(), capacity);
    let mut hits = Vec::new();
    for (row, view) in views.iter().skip(start).take(capacity).enumerate() {
        let y = inner.y + (row * row_height) as u16;
        let active = Some(&view.agent.id) == app.agent.as_ref();
        let process = if view.process.is_empty() {
            "unknown"
        } else {
            &view.process
        };
        let process = crate::sanitize(process);
        let status = format!(
            " [{}] ",
            if view.guard_error.is_some() {
                "guard !"
            } else {
                &process
            }
        );
        let status_w = Line::raw(&status).width().min(inner.width as usize) as u16;
        let mail = if view.pending_messages > 0 {
            format!(" [mail:{}]", view.pending_messages)
        } else {
            String::new()
        };
        let name = format!(
            " {} {}{mail}",
            if active { "▸" } else { "·" },
            crate::sanitize(&view.agent.name)
        );
        let left = Rect::new(inner.x, y, inner.width.saturating_sub(status_w), 1);
        let right = Rect::new(left.right(), y, status_w, 1);
        frame.render_widget(
            Paragraph::new(fit_label(&name, left.width)).style(if active {
                p.selected()
            } else {
                p.body()
            }),
            left,
        );
        frame.render_widget(
            Paragraph::new(status).style(if active {
                p.selected()
            } else {
                state_style(p, &process, view.guard_error.is_some())
            }),
            right,
        );
        if row_height > 1 {
            let task = if view.task.is_empty() {
                "unknown"
            } else {
                &view.task
            };
            let run = view
                .run
                .as_ref()
                .map(|run| format!("{} · {}", &run.id[..run.id.len().min(8)], run.pane))
                .unwrap_or_else(|| "no run".into());
            let line = Line::from(vec![
                Span::styled(
                    format!("   task: {}", crate::sanitize(task)),
                    p.body().fg(if task == "unknown" { p.warn } else { p.cyan }),
                ),
                Span::styled(format!("   ·   {run}"), p.muted()),
            ]);
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
        }
        hits.push((
            Rect::new(inner.x, y, inner.width, if row_height > 1 { 2 } else { 1 }),
            view.agent.id.clone(),
        ));
    }
    app.hits.agents = hits;
}

fn render_details(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(view) = app
        .snapshot
        .agents
        .iter()
        .find(|v| Some(&v.agent.id) == app.agent.as_ref())
    else {
        frame.render_widget(app.theme.palette().block(" 03 / Run inspector "), area);
        return;
    };
    let run = view
        .run
        .as_ref()
        .map(|run| {
            format!(
                "{} · pane {} · pid {}",
                crate::sanitize(&run.id),
                crate::sanitize(&run.pane),
                crate::sanitize(&run.pid)
            )
        })
        .unwrap_or_else(|| "none".into());
    let task = if view.task.is_empty() {
        "unknown".into()
    } else {
        crate::sanitize(&view.task)
    };
    let evidence = if view.evidence.is_empty() {
        "no task evidence".into()
    } else {
        crate::sanitize(&view.evidence)
    };
    let p = app.theme.palette();
    let field = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!(" {label}"), p.muted()),
            Span::styled(value, p.body()),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Process: ", p.muted()),
            Span::styled(
                crate::sanitize(&view.process),
                state_style(p, &view.process, view.guard_error.is_some()),
            ),
            Span::styled("  ·  Task: ", p.muted()),
            Span::styled(
                task.clone(),
                p.body().fg(if task == "unknown" { p.warn } else { p.cyan }),
            ),
            Span::styled(
                format!("  ·  Input: {}", crate::sanitize(&view.agent.input_mode)),
                p.muted(),
            ),
        ]),
        field("Run: ", run),
        field(
            "Evidence: ",
            format!("{evidence} · observed {}", view.observed_at),
        ),
    ];
    let reported = view
        .report
        .as_ref()
        .map(|v| {
            format!(
                "{} ({}) · {}",
                crate::sanitize(&v.report.status),
                if v.fresh { "fresh" } else { "stale" },
                crate::sanitize(&v.report.summary)
            )
        })
        .unwrap_or_else(|| "none".into());
    lines.push(field(
        "Mail: ",
        format!(
            "{} unacked · Self-report: {}",
            view.pending_messages, reported
        ),
    ));
    if area.height >= 9 {
        lines.push(field("Cwd: ", crate::sanitize(&view.agent.cwd)));
        lines.push(field("Adapter: ", crate::sanitize(&view.agent.adapter)));
    }
    if let Some(error) = &view.guard_error {
        lines.push(Line::styled(
            format!(" Guard: {}", crate::sanitize(error)),
            app.theme.palette().body().fg(app.theme.palette().warn),
        ));
    } else {
        lines.push(field(
            "Command: ",
            crate::sanitize(
                &view
                    .agent
                    .argv
                    .iter()
                    .map(|s| crate::shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).block(app.theme.palette().block(format!(
            " 03 / Run inspector · {} ",
            crate::sanitize(&view.agent.name)
        ))),
        area,
    );
}

fn popup(area: Rect, width: u16, height: u16, anchor: Option<(u16, u16)>) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let (x, y) = anchor.unwrap_or((
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
    ));
    Rect::new(
        x.clamp(area.x, area.right().saturating_sub(width)),
        y.clamp(area.y, area.bottom().saturating_sub(height)),
        width,
        height,
    )
}

fn render_menu(frame: &mut Frame, app: &mut App, target: &Target, selected: usize, x: u16, y: u16) {
    let area = popup(frame.area(), 42, ACTIONS.len() as u16 + 2, Some((x, y)));
    let block = app.theme.palette().block(format!(
        " {} · {} ",
        target.name,
        target
            .run_id
            .as_deref()
            .map(|r| &r[..r.len().min(8)])
            .unwrap_or("no run")
    ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let start = visible_start(selected, ACTIONS.len(), inner.height as usize);
    for (row, (action, label)) in ACTIONS
        .iter()
        .skip(start)
        .take(inner.height as usize)
        .enumerate()
    {
        let rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        let style = if start + row == selected {
            selection_style(app.theme.palette())
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(*label).style(style), rect);
        app.hits.menu.push((rect, *action));
    }
}

fn preview_text(text: &str) -> String {
    let prefix: String = text.chars().take(160).collect();
    if prefix.len() < text.len() {
        format!(
            "{}… [preview clipped; Esc then d to review full draft]",
            crate::sanitize(&prefix)
        )
    } else {
        crate::sanitize(text)
    }
}

fn render_confirmation(frame: &mut Frame, app: &mut App, confirmation: Confirmation) {
    if frame.area().width < 72 || frame.area().height < 14 {
        let area = popup(frame.area(), 60, 5, None);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new("Confirmation disabled. Resize to at least 72 × 14 to review the target and action warning. Esc cancels.").wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" Resize to confirm ")), area);
        return;
    }
    app.confirmation_visible = true;
    let (title, detail) = match confirmation {
        Confirmation::Terminate(target) => (
            " Terminate run? ",
            format!(
                "Remove {} native pane and close its PTY? External descendants may survive.\nRun: {}\n\ny confirms · n/Esc cancels",
                target.name.chars().take(28).collect::<String>(),
                target
                    .run_id
                    .as_deref()
                    .map(crate::sanitize)
                    .unwrap_or_default()
            ),
        ),
        Confirmation::Restart(target) => (
            " Restart agent? ",
            format!(
                "Stop {} and start a new run? Unsaved agent work may be lost.\nRun: {}\n\ny confirms · n/Esc cancels",
                target.name.chars().take(28).collect::<String>(),
                target
                    .run_id
                    .as_deref()
                    .map(crate::sanitize)
                    .unwrap_or_default()
            ),
        ),
        Confirmation::Interrupt(target) => (
            " Send Ctrl-C key? ",
            format!(
                "Target: {}\nRun: {}\nThe harness decides what Ctrl-C means. SIGINT or task cancellation is not guaranteed.\n\ny confirms · n/Esc cancels",
                target.name.chars().take(28).collect::<String>(),
                target.run_id.as_deref().unwrap_or("none")
            ),
        ),
        Confirmation::Submit(target) => (
            " Send Enter key? ",
            format!(
                "Target: {}\nRun: {}\nEnter may submit a command, prompt or approval. Inspect the native pane first. Task state is not an authorization.\n\ny confirms · n/Esc cancels",
                target.name.chars().take(28).collect::<String>(),
                target.run_id.as_deref().unwrap_or("none")
            ),
        ),
        Confirmation::SendDraft(target, text, fixture) => (
            " Transmit saved text? ",
            format!(
                "Target: {}\nRun: {}\n{}\nText ({} bytes): {}\n\ny confirms · n/Esc cancels",
                target.name.chars().take(28).collect::<String>(),
                target.run_id.as_deref().unwrap_or("none"),
                if fixture {
                    "Synthetic fixture receives text + newline."
                } else {
                    "Literal PTY paste, no Enter. The harness may act immediately. Inspect its native pane first."
                },
                text.len(),
                preview_text(&text)
            ),
        ),
        Confirmation::Detach(client) => (
            " Detach client? ",
            format!(
                "Detach tmux client {}? The agents keep running.\n\ny confirms · n/Esc cancels",
                crate::sanitize(&client)
            ),
        ),
    };
    let area = popup(frame.area(), 88, 14, None);
    let block = app
        .theme
        .palette()
        .block(title)
        .border_style(app.theme.palette().body().fg(app.theme.palette().warn));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(2),
        ),
    );
    let y = inner.bottom() - 1;
    let yes = Rect::new(inner.x + 1, y, 11, 1);
    let no = Rect::new(inner.x + 14, y, 12, 1);
    frame.render_widget(
        Paragraph::new("[y Confirm]")
            .style(app.theme.palette().body().fg(app.theme.palette().warn)),
        yes,
    );
    frame.render_widget(Paragraph::new("[Esc Cancel]"), no);
    app.hits.accept = Some(yes);
    app.hits.cancel = Some(no);
}

fn render_form(frame: &mut Frame, app: &mut App, form: &ProfileForm) {
    let title = match &form.kind {
        FormKind::Project => " Register project ",
        FormKind::Agent { agent_id: None, .. } => " Register agent · does not start ",
        _ => " Edit stopped agent profile ",
    };
    let area = popup(frame.area(), 96, 20, None);
    let block = app.theme.palette().block(title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let labels = form.labels();
    let rows = inner.height.saturating_sub(5) as usize / 2;
    let start = visible_start(form.selected, labels.len(), rows);
    for (row, index) in (start..labels.len()).take(rows).enumerate() {
        let label = Rect::new(inner.x, inner.y + row as u16 * 2, inner.width, 1);
        let field = Rect::new(inner.x, label.y + 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(labels[index]).style(app.theme.palette().muted()),
            label,
        );
        let display = format!(
            "{}{}",
            crate::sanitize(&form.values[index]),
            if form.selected == index { "▏" } else { "" }
        );
        let lines = draft_display_lines(&form.values[index], field.width);
        let text = if Line::raw(&display).width() > field.width as usize {
            lines.last().cloned().unwrap_or_default()
        } else {
            Line::raw(display)
        };
        frame.render_widget(
            Paragraph::new(text).style(if form.selected == index {
                selection_style(app.theme.palette())
            } else {
                Style::default()
            }),
            field,
        );
        app.hits.form_fields.push((field, index));
    }
    if inner.height >= 5 {
        let y = inner.bottom() - 5;
        frame.render_widget(
            Paragraph::new(
                "Quotes group argv; no shell expansion. Use /absolute/executable.
terminal-line opts in to PTY paste/Enter; it may execute or approve.
Tab/↑↓ field · Ctrl-U clear · Ctrl-S save · Esc cancel",
            )
            .style(app.theme.palette().muted()),
            Rect::new(inner.x, y, inner.width, 3),
        );
        frame.render_widget(
            Paragraph::new(&*form.error)
                .style(app.theme.palette().body().fg(app.theme.palette().warn)),
            Rect::new(inner.x, y + 3, inner.width, 1),
        );
        if inner.width >= 29 {
            let save = Rect::new(inner.x, y + 4, 13, 1);
            let cancel = Rect::new(inner.x + 15, y + 4, 12, 1);
            frame.render_widget(
                Paragraph::new("[Ctrl-S Save]").style(app.theme.palette().accent()),
                save,
            );
            frame.render_widget(Paragraph::new("[Esc Cancel]"), cancel);
            app.hits.accept = Some(save);
            app.hits.cancel = Some(cancel);
        }
    }
}

fn render_draft(frame: &mut Frame, app: &App, target: &Target, text: &str) {
    let area = popup(frame.area(), 88, 15, None);
    let block = app
        .theme
        .palette()
        .block(format!(" Draft for {} · local only ", target.name));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let lines = draft_display_lines(text, parts[0].width);
    let scroll = lines
        .len()
        .saturating_sub(parts[0].height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), parts[0]);
    frame.render_widget(
        Paragraph::new("Ctrl-S save locally · Esc discard · nothing is sent")
            .style(app.theme.palette().body().fg(app.theme.palette().warn)),
        parts[1],
    );
}

/// Hard-wrap at grapheme boundaries without depending on Ratatui's unstable
/// rendered-line-info API. Cursor and Unicode-wide text stay visible at the tail.
fn draft_display_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let display = format!("{text}▏");
    let mut lines = Vec::new();
    for raw_line in display.split('\n') {
        let source = Line::raw(crate::sanitize(raw_line));
        let mut current = String::new();
        let mut used = 0;
        for grapheme in source.styled_graphemes(Style::default()) {
            let cells = Span::raw(grapheme.symbol).width();
            if used + cells > width && !current.is_empty() {
                lines.push(Line::raw(std::mem::take(&mut current)));
                used = 0;
            }
            current.push_str(grapheme.symbol);
            used += cells;
        }
        lines.push(Line::raw(current));
    }
    lines
}

fn handle_input(app: &mut App, core: &Core, input: Event) -> (bool, bool) {
    match input {
        Event::Key(key) => (app.key(core, key), key.kind != KeyEventKind::Release),
        Event::Mouse(mouse) => {
            if !matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
                app.mouse(core, mouse);
                (false, true)
            } else {
                (false, false)
            }
        }
        Event::Paste(text) => {
            if matches!(app.mode, Mode::Draft { .. } | Mode::Form(_)) {
                app.paste(&text);
                (false, true)
            } else {
                (false, false)
            }
        }
        Event::Resize(_, _) => (false, true),
        _ => (false, false),
    }
}

/// Run the manager. Exiting this function never terminates agents.
pub fn run(core: &Core, project_filter: Option<String>) -> Result<()> {
    let initial = core.snapshot().unwrap_or_else(|error| Snapshot {
        projects: Vec::new(),
        agents: Vec::new(),
        connected: false,
        error: Some(crate::sanitize(&error.to_string())),
    });
    let mut app = App::new(initial, project_filter);
    app.theme = core.theme()?;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut input_guard = crate::terminal_input::TerminalInputGuard::default();
    let (request_tx, request_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let observer = core.clone();
    std::thread::spawn(move || {
        while request_rx.recv().is_ok() {
            let result = observer.snapshot().map_err(|error| error.to_string());
            if result_tx.send(result).is_err() {
                break;
            }
        }
    });
    let mut awaiting: Option<Instant> = None;
    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut dirty = true;
    'manager: loop {
        match core.theme() {
            Ok(theme) if theme != app.theme => {
                app.theme = theme;
                dirty = true;
            }
            Err(e) if app.notice != format!("Appearance config: {e}") => {
                app.notice = format!("Appearance config: {e}");
                dirty = true;
            }
            _ => {}
        }
        if let Ok(result) = result_rx.try_recv() {
            awaiting = None;
            match result {
                Ok(snapshot) => {
                    dirty |= !semantically_equal(&app.snapshot, &snapshot);
                    app.snapshot = snapshot;
                    app.reconcile_selection();
                }
                Err(error) => {
                    let previous = app.snapshot.clone();
                    invalidate_snapshot(&mut app.snapshot, error);
                    dirty |= !semantically_equal(&previous, &app.snapshot);
                }
            }
        }
        if awaiting.is_some_and(|started| started.elapsed() > Duration::from_millis(1500))
            && app.snapshot.connected
        {
            invalidate_snapshot(
                &mut app.snapshot,
                "Background refresh pending; previous observation is stale.".into(),
            );
            dirty = true;
        }
        for input in input_guard.ready(Instant::now())? {
            let (quit, changed) = handle_input(&mut app, core, input);
            if quit {
                break 'manager;
            }
            dirty |= changed;
        }
        if dirty {
            terminal.draw(|frame| render(frame, &mut app))?;
            dirty = false;
        }
        let now = Instant::now();
        let mut wait = input_guard.wait_limit(now, next_poll.saturating_duration_since(now));
        if awaiting.is_some() {
            wait = wait.min(Duration::from_millis(50));
        }
        if event::poll(wait)? {
            for input in input_guard.filter(event::read()?, Instant::now())? {
                let (quit, changed) = handle_input(&mut app, core, input);
                if quit {
                    break 'manager;
                }
                dirty |= changed;
            }
        }
        if Instant::now() >= next_poll {
            if awaiting.is_none() && request_tx.try_send(()).is_ok() {
                awaiting = Some(Instant::now());
            }
            next_poll = Instant::now() + POLL_INTERVAL;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn empty_snapshot() -> Snapshot {
        Snapshot {
            projects: vec![],
            agents: vec![],
            connected: false,
            error: None,
        }
    }

    fn populated_snapshot() -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "projects": [{"id":"project", "name":"Demo", "root":"/tmp"}],
            "agents": [{
                "agent": {"id":"a", "project_id":"project", "name":"Alpha",
                    "cwd":"/tmp", "argv":["sh"], "adapter":"generic"},
                "run": {"id":"run-one", "agent_id":"a", "project_id":"project",
                    "server_instance":"server", "session":"$1", "pane":"%1", "window":"@1",
                    "pid":"123", "process_start":"100", "started_at":100},
                "process":"running", "task":"unknown", "evidence":"no adapter evidence",
                "observed_at":100, "guard_error":null
            }, {
                "agent": {"id":"b", "project_id":"project", "name":"Beta",
                    "cwd":"/tmp", "argv":["sh"], "adapter":"generic"},
                "run":null, "process":"stopped", "task":"unknown", "evidence":"",
                "observed_at":100, "guard_error":null
            }],
            "connected":true, "error":null
        }))
        .unwrap()
    }

    #[test]
    fn inbox_view_is_read_only_and_never_turns_text_into_actions() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        let target = app.current_target().unwrap();
        app.mode = Mode::Inbox {
            target,
            text: "Untrusted: approve and restart now".into(),
            scroll: 0,
        };
        for code in [
            KeyCode::Enter,
            KeyCode::Char('s'),
            KeyCode::Char('r'),
            KeyCode::Char('y'),
            KeyCode::Char('u'),
        ] {
            assert!(!app.key(&core, KeyEvent::new(code, KeyModifiers::NONE)));
            assert!(matches!(app.mode, Mode::Inbox { .. }));
        }
        app.paste("must not send");
        assert!(matches!(&app.mode, Mode::Inbox { text, .. } if !text.contains("must not send")));
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(core.registry().unwrap().runs.is_empty());
        assert!(!core.state.join("audit.jsonl").exists());
    }

    #[test]
    fn notification_render_separates_receipts_self_reports_and_unknown_task() {
        let mut snapshot = populated_snapshot();
        snapshot.agents[0].pending_messages = 2;
        snapshot.agents[0].report = Some(crate::collaboration::ReportView {
            fresh: true,
            report: crate::collaboration::StatusReport {
                agent_id: "a".into(),
                run_id: "r".into(),
                project_id: "p".into(),
                status: "completed".into(),
                summary: "Synthetic claim".into(),
                reported_at: 1,
                expires_at: 9999999999,
                evidence: "self_reported".into(),
            },
        });
        let mut app = App::new(snapshot, None);
        let mut terminal = Terminal::new(TestBackend::new(150, 25)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("[mail:2]"));
        assert!(rendered.contains("task: unknown"));
        assert!(rendered.contains("Mail: 2 unacked"));
        assert!(rendered.contains("Self-report: completed (fresh)"));
        invalidate_snapshot(&mut app.snapshot, "failed".into());
        assert!(!app.snapshot.agents[0].report.as_ref().unwrap().fresh);
    }

    #[test]
    fn observation_timestamps_do_not_trigger_semantic_redraws() {
        let original = populated_snapshot();
        let mut update = original.clone();
        update.agents[0].observed_at += 1;
        assert!(semantically_equal(&original, &update));
        update.agents[0].process = "exited".into();
        assert!(!semantically_equal(&original, &update));
    }

    #[test]
    fn failed_snapshot_clears_stale_status_but_retains_guarded_identity() {
        let mut snapshot = populated_snapshot();
        invalidate_snapshot(&mut snapshot, "connection lost".into());
        assert!(!snapshot.connected);
        assert_eq!(snapshot.error.as_deref(), Some("connection lost"));
        for view in &snapshot.agents {
            assert_eq!(view.process, "unknown");
            assert_eq!(view.task, "unknown");
            assert!(view.evidence.contains("stale"));
            assert!(view.guard_error.is_some());
        }
        assert_eq!(snapshot.agents[0].run.as_ref().unwrap().id, "run-one");
    }

    #[test]
    fn start_refuses_captured_or_current_existing_runs() {
        let temporary = tempfile::tempdir().unwrap();
        let core = Core::new(temporary.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        let captured = app.current_target().unwrap();
        app.snapshot.agents[0].run = None;
        app.invoke(&core, Action::Start, Some(captured));
        assert!(app.notice.contains("explicitly restart"));
        let captured_without_run = app.current_target().unwrap();
        app.snapshot = populated_snapshot();
        app.invoke(&core, Action::Start, Some(captured_without_run));
        assert!(app.notice.contains("explicitly restart"));
    }

    #[test]
    fn populated_render_exposes_unknown_task_and_exact_row_targets() {
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Alpha"));
        assert!(rendered.contains("task: unknown"));
        assert_eq!(app.hits.agents.len(), 2);
        let second = app.hits.agents[1].0;
        let temporary = tempfile::tempdir().unwrap();
        let core = Core::new(temporary.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        app.mouse(
            &core,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: second.x,
                row: second.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(&app.mode, Mode::Menu { target, .. } if target.agent_id == "b"));
        app.mode = Mode::Normal;
        app.mouse(
            &core,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn modal_target_is_frozen_and_draft_enter_does_not_send() {
        let temporary = tempfile::tempdir().unwrap();
        let core = Core::new(temporary.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        app.key(&core, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.snapshot.agents[0].run.as_mut().unwrap().id = "run-two".into();
        assert!(
            matches!(&app.mode, Mode::Confirm(Confirmation::Terminate(target))
            if target.run_id.as_deref() == Some("run-one"))
        );
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut registry = core.registry().unwrap();
        registry.projects = app.snapshot.projects.clone();
        registry.agents = app
            .snapshot
            .agents
            .iter()
            .map(|v| v.agent.clone())
            .collect();
        registry.runs = app
            .snapshot
            .agents
            .iter()
            .filter_map(|v| v.run.clone())
            .collect();
        core.save(&registry).unwrap();
        app.key(&core, KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        app.key(&core, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        app.key(&core, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&app.mode, Mode::Draft { text, .. } if text == "h\n"));
        assert!(!core.state.join("drafts").exists());
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.notice.clear();
        app.key(
            &core,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        assert!(
            app.notice.is_empty(),
            "Ctrl-S outside the draft must not start an agent"
        );
    }

    #[test]
    fn draft_wrap_keeps_graphemes_and_cursor() {
        let lines = draft_display_lines("한국어 e\u{301}", 4);
        assert!(lines.iter().all(|line| line.width() <= 4));
        let content = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(content, "한국어 e\u{301}▏");
    }

    #[test]
    fn hit_testing_matches_exact_rows_and_excludes_borders() {
        let hits = HitAreas {
            agents: vec![
                (Rect::new(10, 3, 15, 1), "first".into()),
                (Rect::new(10, 4, 15, 1), "second".into()),
            ],
            projects: vec![],
            menu: vec![(Rect::new(12, 6, 22, 1), Action::Terminate)],
            ..HitAreas::default()
        };
        assert_eq!(hits.agent_at(10, 3), Some("first"));
        assert_eq!(hits.agent_at(24, 4), Some("second"));
        assert_eq!(hits.agent_at(25, 4), None);
        assert_eq!(hits.agent_at(9, 3), None);
        assert_eq!(hits.agent_at(10, 5), None);
        assert_eq!(hits.menu_at(12, 6), Some(Action::Terminate));
        assert_eq!(hits.menu_at(12, 7), None);
    }

    #[test]
    fn layouts_and_popups_stay_inside_tiny_terminals() {
        for width in [0, 1, 2, 12, 24, 54, 55, 80, 160] {
            for height in [0, 1, 2, 4, 8, 12, 24, 60] {
                let area = Rect::new(0, 0, width, height);
                let layout = areas(area);
                for rect in [
                    layout.header,
                    layout.projects,
                    layout.agents,
                    layout.details,
                    layout.footer,
                    popup(area, 72, 9, Some((u16::MAX, u16::MAX))),
                ] {
                    assert!(rect.right() <= area.right(), "{rect:?} exceeds {area:?}");
                    assert!(rect.bottom() <= area.bottom(), "{rect:?} exceeds {area:?}");
                }
            }
        }
    }

    #[test]
    fn test_backend_renders_empty_and_modal_narrow_layouts() {
        for (width, height) in [(1, 1), (12, 4), (24, 8), (40, 14), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::new(empty_snapshot(), None);
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            app.mode = Mode::Menu {
                target: Target {
                    agent_id: "a".into(),
                    name: "Agent".into(),
                    run_id: Some("r".into()),
                },
                selected: 8,
                x: width,
                y: height,
            };
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            app.mode = Mode::Draft {
                target: Target {
                    agent_id: "a".into(),
                    name: "Agent".into(),
                    run_id: Some("r".into()),
                },
                text: "hello\nworld\n한국어".into(),
            };
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            app.mode = Mode::Confirm(Confirmation::Detach("client".into()));
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
        }
    }

    #[test]
    fn paste_is_draft_only_and_strips_terminal_controls() {
        let mut app = App::new(empty_snapshot(), None);
        app.paste("ignore this");
        assert!(matches!(app.mode, Mode::Normal));
        app.mode = Mode::Draft {
            target: Target {
                agent_id: "a".into(),
                name: "a".into(),
                run_id: None,
            },
            text: String::new(),
        };
        app.paste("a\u{1b}b\u{0}c\n한글");
        assert!(matches!(&app.mode, Mode::Draft { text, .. } if text == "abc\n한글"));
    }

    #[test]
    fn selection_and_viewport_are_bounded() {
        let ids = vec!["a".into(), "b".into()];
        assert_eq!(moved_selection(&ids, Some("a"), -1), Some("b".into()));
        assert_eq!(moved_selection(&ids, Some("b"), 1), Some("a".into()));
        assert_eq!(moved_selection(&[], None, 1), None);
        assert_eq!(visible_start(9, 10, 3), 7);
        assert_eq!(visible_start(0, 10, 3), 0);
    }
    #[test]
    fn project_and_agent_forms_save_without_launching() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(empty_snapshot(), None);
        app.key(&core, KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));
        let Mode::Form(mut form) = app.mode.clone() else {
            panic!("missing project form")
        };
        form.values = vec![
            "Project 한국어".into(),
            temp.path().to_string_lossy().into_owned(),
        ];
        app.save_form(&core, form);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(core.registry().unwrap().projects.len(), 1);
        app.key(&core, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let Mode::Form(mut form) = app.mode.clone() else {
            panic!("missing agent form")
        };
        form.values[0] = "writer".into();
        form.values[2] = "/bin/echo 'literal ; $HOME'".into();
        app.save_form(&core, form);
        let registry = core.registry().unwrap();
        assert_eq!(registry.agents[0].argv, ["/bin/echo", "literal ; $HOME"]);
        assert_eq!(registry.agents[0].input_mode, "native");
        assert!(registry.runs.is_empty());
        assert!(!core.state.join("tmux.sock").exists());
        app.key(&core, KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Mode::Form(mut form) = app.mode.clone() else {
            panic!("missing edit form")
        };
        form.values[0] = "renamed".into();
        form.values[4] = "terminal-line".into();
        form.values[5] = "mailbox".into();
        app.save_form(&core, form);
        let changed = core.registry().unwrap();
        assert_eq!(changed.agents.len(), 1);
        assert_eq!(changed.agents[0].id, registry.agents[0].id);
        assert_eq!(changed.agents[0].input_mode, "terminal-line");
        assert_eq!(changed.agents[0].collaboration_mode, "mailbox");
    }

    #[test]
    fn form_errors_keep_user_text_and_cancel_has_no_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let project = core.register_project("p", temp.path()).unwrap();
        let mut app = App::new(core.snapshot().unwrap(), Some(project.id));
        app.key(&core, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let Mode::Form(mut form) = app.mode.clone() else {
            panic!()
        };
        form.values[0] = "kept".into();
        form.values[2] = "'unclosed".into();
        app.save_form(&core, form);
        assert!(
            matches!(&app.mode, Mode::Form(f) if f.values[0] == "kept" && f.error.contains("unfinished"))
        );
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(core.registry().unwrap().agents.is_empty());
    }

    #[test]
    fn form_paste_is_literal_singleline_and_never_launches() {
        let mut app = App::new(empty_snapshot(), None);
        app.open_form(Action::NewProject, None);
        app.paste("project\n\u{1b}\u{0}name");
        assert!(matches!(&app.mode, Mode::Form(f) if f.values[0] == "projectname"));
    }

    #[test]
    fn native_input_is_disabled_and_ctrl_c_requires_frozen_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        app.invoke(&core, Action::SendDraft, app.current_target());
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.notice.contains("disabled"));
        app.invoke(&core, Action::Submit, app.current_target());
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.notice.contains("disabled"));
        app.invoke(&core, Action::Interrupt, app.current_target());
        assert!(
            matches!(&app.mode, Mode::Confirm(Confirmation::Interrupt(t)) if t.run_id.as_deref() == Some("run-one"))
        );
        app.snapshot.agents[0].run.as_mut().unwrap().id = "run-two".into();
        assert!(
            matches!(&app.mode, Mode::Confirm(Confirmation::Interrupt(t)) if t.run_id.as_deref() == Some("run-one"))
        );
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!core.state.join("audit.jsonl").exists());
    }

    #[test]
    fn saved_draft_reopens_and_transmission_captures_exact_text() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut snapshot = populated_snapshot();
        snapshot.agents[0].agent.input_mode = "terminal-line".into();
        let mut registry = core.registry().unwrap();
        registry.projects = snapshot.projects.clone();
        registry.agents = snapshot.agents.iter().map(|v| v.agent.clone()).collect();
        registry.runs = snapshot
            .agents
            .iter()
            .filter_map(|v| v.run.clone())
            .collect();
        core.save(&registry).unwrap();
        core.save_draft("a", "run-one", "first prompt").unwrap();
        let mut app = App::new(snapshot, None);
        app.invoke(&core, Action::Draft, app.current_target());
        assert!(matches!(&app.mode, Mode::Draft { text, .. } if text == "first prompt"));
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.invoke(&core, Action::SendDraft, app.current_target());
        core.save_draft("a", "run-one", "changed after confirmation")
            .unwrap();
        assert!(
            matches!(&app.mode, Mode::Confirm(Confirmation::SendDraft(t, text, false)) if text == "first prompt" && t.run_id.as_deref() == Some("run-one"))
        );
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        core.save_draft("a", "run-one", "multi\nline").unwrap();
        app.invoke(&core, Action::SendDraft, app.current_target());
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.notice.contains("Multiline drafts stay local"));
    }

    #[test]
    fn mouse_toolbar_and_form_cancel_match_keyboard() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(empty_snapshot(), None);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let r = app.hits.toolbar[0].0;
        app.mouse(
            &core,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(app.mode, Mode::Form(_)));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let r = app.hits.cancel.unwrap();
        app.mouse(
            &core,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(app.mode, Mode::Normal));
        assert!(core.registry().unwrap().projects.is_empty());
    }

    #[test]
    fn form_renders_on_narrow_and_short_screens() {
        for (w, h) in [(1, 1), (20, 5), (40, 10), (96, 20), (120, 36)] {
            let mut app = App::new(populated_snapshot(), None);
            app.open_form(Action::NewAgent, None);
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            for (r, _) in &app.hits.form_fields {
                assert!(r.right() <= w && r.bottom() <= h);
            }
        }
    }
    #[test]
    fn confirmation_cannot_accept_before_full_warning_is_rendered() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("state"), "/nonexistent-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        app.invoke(&core, Action::Interrupt, app.current_target());
        let mut small = Terminal::new(TestBackend::new(40, 8)).unwrap();
        small.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(!app.confirmation_visible);
        assert!(app.hits.accept.is_none());
        app.key(&core, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Confirm(_)));
        assert!(!core.state.join("audit.jsonl").exists());
        let mut large = Terminal::new(TestBackend::new(72, 14)).unwrap();
        large.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.confirmation_visible);
        assert!(app.hits.accept.is_some());
        let display = large
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(display.contains("run-one"));
        assert!(display.contains("cancellation"));
    }
    #[test]
    fn themes_preserve_exact_hits_and_observed_status() {
        let mut reference = None;
        for theme in [ThemeName::Portal, ThemeName::Classic, ThemeName::Mono] {
            let mut app = App::new(populated_snapshot(), None);
            app.theme = theme;
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let current = app.hits.agents.clone();
            if let Some(expected) = &reference {
                assert_eq!(&current, expected);
            } else {
                reference = Some(current);
            }
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("[running]"));
            assert!(text.contains("task: unknown"));
            assert!(text.contains("Run: run-one"));
        }
    }
    #[test]
    fn long_unicode_names_leave_process_badge_visible() {
        let mut snapshot = populated_snapshot();
        snapshot.agents[0].agent.name = "차원간 연구 에이전트 Long Agent Name".repeat(4);
        let mut app = App::new(snapshot, None);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rect = app.hits.agents[0].0;
        let buf = terminal.backend().buffer();
        let row: String = (rect.x..rect.right())
            .map(|x| buf[(x, rect.y)].symbol())
            .collect();
        assert!(row.contains("[running]"));
        assert!(row.contains('…'));
        for width in 0..24 {
            assert!(
                Line::raw(fit_label("한글 e\u{301} 🚀 rocket", width)).width() <= width as usize
            );
        }
    }
    #[test]
    fn help_and_theme_keys_never_operate_agents() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(temp.path().join("s"), "/no-tmux".into()).unwrap();
        let mut app = App::new(populated_snapshot(), None);
        app.key(&core, KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Help));
        for key in ['s', 'S', 'y', 'u', 'i', 'r', 'x', 't'] {
            app.key(&core, KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            assert!(matches!(app.mode, Mode::Help));
        }
        app.key(&core, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.key(&core, KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(app.theme, ThemeName::Portal.next());
        assert_eq!(core.theme().unwrap(), ThemeName::Garage);
        assert!(core.registry().unwrap().runs.is_empty());
        assert!(!core.state.join("audit.jsonl").exists());
    }
}
