//! Optional one-pane navigation. Native agent panes are never re-rendered or moved.
use crate::{
    Core, Pane, Registry, atomic_json, id, now, process_start, shell_quote, valid_native_id,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sidebar {
    pub schema_version: u32,
    pub id: String,
    pub server_instance: String,
    pub pane: String,
    pub pid: String,
    pub process_start: String,
    pub width: u16,
    pub host_project_id: String,
    pub host_agent_id: String,
    pub host_run_id: String,
    pub started_at: u64,
}
struct SidebarPane {
    window: String,
    dead: bool,
    synchronized: bool,
}

impl Core {
    pub fn sidebar_record(&self) -> Result<Option<Sidebar>> {
        let path = self.state.join("sidebar.json");
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            fs::metadata(&path)?.len() <= 8192,
            "sidebar registry exceeds read limit"
        );
        let record: Sidebar = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(record.schema_version == 1, "unsupported sidebar schema");
        Ok(Some(record))
    }
    fn guard_sidebar(&self, r: &Registry, s: &Sidebar) -> Result<SidebarPane> {
        ensure!(
            s.server_instance == r.server_instance && self.server()? == s.server_instance,
            "sidebar server identity changed; controls locked"
        );
        let raw = self.tm(&["display-message", "-p", "-t", &s.pane,
            "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@pctl_sidebar}\t#{@pctl_sidebar_instance}\t#{@pctl_agent}\t#{window_id}\t#{synchronize-panes}"])?;
        let f: Vec<_> = raw.split('\t').collect();
        ensure!(
            f.len() == 8
                && f[0] == s.pane
                && f[1] == s.pid
                && f[3] == s.id
                && f[4] == s.server_instance
                && f[5].is_empty()
                && valid_native_id(f[6], '@'),
            "sidebar pane identity changed; controls locked"
        );
        let dead = f[2] == "1";
        if !dead {
            ensure!(
                process_start(&s.pid)? == s.process_start,
                "sidebar process identity changed; controls locked"
            );
        }
        Ok(SidebarPane {
            window: f[6].into(),
            dead,
            synchronized: f[7] == "1" || f[7] == "on",
        })
    }
    fn sidebar_command(&self, sidebar: &str) -> Result<String> {
        Ok([
            std::env::current_exe()?.to_string_lossy().into_owned(),
            "--state".into(),
            self.state.to_string_lossy().into_owned(),
            "--tmux".into(),
            self.tmux.to_string_lossy().into_owned(),
            "sidebar".into(),
            "ui".into(),
            "--id".into(),
            sidebar.into(),
        ]
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" "))
    }
    fn ensure_sidebar_room(&self, target: &Pane, width: u16) -> Result<()> {
        ensure!(
            (20..=40).contains(&width),
            "sidebar width must be 20..40 columns"
        );
        ensure!(
            !target.synchronized,
            "synchronize-panes must be off before sidebar layout changes"
        );
        let raw = self.tm(&[
            "display-message",
            "-p",
            "-t",
            &target.pane,
            "#{window_width}\t#{window_height}",
        ])?;
        let f: Vec<_> = raw.split('\t').collect();
        ensure!(
            f.len() == 2 && f[0].parse::<u16>()? >= width + 41 && f[1].parse::<u16>()? >= 10,
            "sidebar needs at least width+41 columns and 10 rows; enlarge the native window"
        );
        Ok(())
    }
    pub fn open_sidebar(&self, agent: &str, expected: &str, width: u16) -> Result<Sidebar> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let target = self.guard(&r, agent, expected, false)?;
        self.ensure_sidebar_room(&target, width)?;
        ensure!(
            self.clients()?.len() <= 1,
            "sidebar supports one interactive client in this pilot"
        );
        ensure!(
            !self.state.join("pending-sidebar.json").exists(),
            "unresolved sidebar launch intent; controls locked"
        );
        if let Some(mut existing) = self.sidebar_record()? {
            let observed = self.guard_sidebar(&r, &existing)?;
            ensure!(
                !observed.dead,
                "sidebar UI exited; explicitly close this sidebar ID before opening another"
            );
            existing.width = width;
            self.move_sidebar_locked(&r, &target, agent, expected, &mut existing)?;
            return Ok(existing);
        }
        let sidebar_id = id();
        let gate = self.state.join(format!("sidebar-{sidebar_id}.ready"));
        let launcher = self.state.join(format!("sidebar-{sidebar_id}.sh"));
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\ni=0\nwhile [ ! -f {} ]; do i=$((i+1)); [ \"$i\" -le 300 ] || exit 75; sleep 0.05; done\nexec {}\n",
                shell_quote(&gate.to_string_lossy()),
                self.sidebar_command(&sidebar_id)?
            ),
        )?;
        atomic_json(
            &self.state.join("pending-sidebar.json"),
            &serde_json::json!({"sidebar_id":sidebar_id,"target_agent":agent,"target_run":expected}),
        )?;
        let command = format!("exec /bin/sh {}", shell_quote(&launcher.to_string_lossy()));
        let raw = self.tm(&[
            "split-window",
            "-d",
            "-f",
            "-h",
            "-b",
            "-l",
            &width.to_string(),
            "-t",
            &target.pane,
            "-P",
            "-F",
            "#{pane_id}\t#{pane_pid}",
            &command,
        ])?;
        let f: Vec<_> = raw.split('\t').collect();
        ensure!(
            f.len() == 2 && valid_native_id(f[0], '%') && f[1].chars().all(|c| c.is_ascii_digit()),
            "sidebar launch identity unavailable; launch remains gated"
        );
        let sidebar = Sidebar {
            schema_version: 1,
            id: sidebar_id,
            server_instance: r.server_instance.clone(),
            pane: f[0].into(),
            pid: f[1].into(),
            process_start: process_start(f[1])?,
            width,
            host_project_id: target.project.clone(),
            host_agent_id: agent.into(),
            host_run_id: expected.into(),
            started_at: now(),
        };
        atomic_json(
            &self.state.join("pending-sidebar.json"),
            &serde_json::json!({"sidebar_id":sidebar.id,"phase":"gated_pane_created","sidebar":sidebar,"window":target.window}),
        )?;
        atomic_json(&self.state.join("sidebar.json"), &sidebar)?;
        self.tm(&[
            "set-option",
            "-p",
            "-t",
            &sidebar.pane,
            "@pctl_sidebar",
            &sidebar.id,
        ])?;
        self.tm(&[
            "set-option",
            "-p",
            "-t",
            &sidebar.pane,
            "@pctl_sidebar_instance",
            &sidebar.server_instance,
        ])?;
        self.tm(&["set-option", "-p", "-t", &sidebar.pane, "@pctl_agent", ""])?;
        self.audit(
            "sidebar_open",
            &sidebar.id,
            "registered one optional native sidebar; no agent moved",
        )?;
        ensure!(
            !self.guard_sidebar(&r, &sidebar)?.dead,
            "sidebar launcher exited before readiness gate; inspect pending intent"
        );
        fs::write(gate, b"ready\n")?;
        fs::remove_file(self.state.join("pending-sidebar.json"))?;
        Ok(sidebar)
    }
    fn move_sidebar_locked(
        &self,
        r: &Registry,
        target: &Pane,
        agent: &str,
        expected: &str,
        sidebar: &mut Sidebar,
    ) -> Result<()> {
        let observed = self.guard_sidebar(r, sidebar)?;
        ensure!(
            !observed.dead,
            "sidebar UI exited; close its exact ID before continuing"
        );
        ensure!(
            !observed.synchronized,
            "synchronize-panes must be off in the sidebar source window"
        );
        self.ensure_sidebar_room(target, sidebar.width)?;
        if observed.window != target.window {
            self.tm(&[
                "join-pane",
                "-d",
                "-f",
                "-h",
                "-b",
                "-l",
                &sidebar.width.to_string(),
                "-s",
                &sidebar.pane,
                "-t",
                &target.pane,
            ])?;
        } else {
            self.tm(&[
                "resize-pane",
                "-t",
                &sidebar.pane,
                "-x",
                &sidebar.width.to_string(),
            ])?;
        }
        sidebar.host_project_id = target.project.clone();
        sidebar.host_agent_id = agent.into();
        sidebar.host_run_id = expected.into();
        atomic_json(&self.state.join("sidebar.json"), sidebar).context(
            "sidebar layout persistence failed; outcome_unknown: inspect before retrying",
        )?;
        Ok(())
    }
    pub(crate) fn follow_sidebar_locked(
        &self,
        r: &Registry,
        target: &Pane,
        agent: &str,
        expected: &str,
    ) -> Result<bool> {
        if let Some(mut sidebar) = self.sidebar_record()? {
            self.move_sidebar_locked(r, target, agent, expected, &mut sidebar)?;
            return Ok(true);
        }
        Ok(false)
    }
    fn pending_sidebar(&self) -> Result<Option<serde_json::Value>> {
        let path = self.state.join("pending-sidebar.json");
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            fs::metadata(&path)?.len() <= 16384,
            "sidebar intent exceeds read limit"
        );
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
    }
    pub fn inspect_sidebar(&self) -> Result<serde_json::Value> {
        let r = self.registry()?;
        let sidebar = self.sidebar_record()?;
        let error = sidebar
            .as_ref()
            .and_then(|s| self.guard_sidebar(&r, s).err())
            .map(|e| e.to_string());
        Ok(
            serde_json::json!({"sidebar":sidebar,"pending":self.pending_sidebar()?,"guard_error":error,"scope":"one optional native pane; follows pctl focus only; raw tmux window cycling may leave it behind; zoom hides it"}),
        )
    }
    pub fn close_sidebar(&self, expected_sidebar: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let pending = self.pending_sidebar()?;
        let record = self.sidebar_record()?;
        let sidebar = match record {
            Some(s) => s,
            None => serde_json::from_value(pending.as_ref().and_then(|p|p.get("sidebar")).context("sidebar not found or pending pane identity unavailable; inspect ambiguous launch manually")?.clone())?,
        };
        ensure!(sidebar.schema_version == 1, "unsupported sidebar schema");
        if !self.state.join("sidebar.json").exists() {
            ensure!(
                pending
                    .as_ref()
                    .is_some_and(|p| p["sidebar_id"].as_str() == Some(&sidebar.id)
                        && p["phase"] == "gated_pane_created"),
                "pending sidebar identity/phase mismatch; controls locked"
            );
        }
        ensure!(
            sidebar.id == expected_sidebar,
            "stale_sidebar: sidebar generation changed"
        );
        ensure!(
            sidebar.server_instance == r.server_instance
                && self.server()? == sidebar.server_instance,
            "sidebar server identity changed; controls locked"
        );
        let exists = self.panes()?.iter().any(|p| p.pane == sidebar.pane);
        if exists {
            if let Err(guard_error) = self.guard_sidebar(&r, &sidebar) {
                // A tagged registry may have been interrupted mid-install. Only an
                // exact, still-gated launcher with matching PID/start can be removed.
                let p = pending.as_ref().context(guard_error.to_string())?;
                ensure!(
                    p["sidebar_id"].as_str() == Some(&sidebar.id)
                        && p["phase"] == "gated_pane_created"
                        && !self
                            .state
                            .join(format!("sidebar-{}.ready", sidebar.id))
                            .exists(),
                    "sidebar identity changed and no exact gated launch can be reconciled"
                );
                let raw=self.tm(&["display-message","-p","-t",&sidebar.pane,"#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@pctl_sidebar}\t#{@pctl_sidebar_instance}\t#{@pctl_agent}\t#{window_id}"])?;
                let f: Vec<_> = raw.split('\t').collect();
                ensure!(
                    f.len() == 7
                        && f[0] == sidebar.pane
                        && f[1] == sidebar.pid
                        && (f[3].is_empty() || f[3] == sidebar.id)
                        && (f[4].is_empty() || f[4] == sidebar.server_instance)
                        && f[5].is_empty()
                        && p["window"].as_str() == Some(f[6]),
                    "pending sidebar pane identity mismatch; controls locked"
                );
                if f[2] != "1" {
                    ensure!(
                        process_start(&sidebar.pid)? == sidebar.process_start,
                        "pending sidebar process identity changed"
                    );
                }
            }
            self.tm(&["kill-pane", "-t", &sidebar.pane])?;
        }
        // A verified missing pane on the same server may be reconciled without
        // targeting any other pane. A replacement server never reaches this path.
        if self.state.join("sidebar.json").exists() {
            fs::remove_file(self.state.join("sidebar.json")).context(
                "sidebar removed; outcome_unknown for registry cleanup; inspect before retrying",
            )?;
        }
        if pending
            .as_ref()
            .is_some_and(|p| p["sidebar_id"].as_str() == Some(&sidebar.id))
        {
            fs::remove_file(self.state.join("pending-sidebar.json"))?;
        }
        self.audit(
            "sidebar_close",
            &sidebar.id,
            if exists {
                "only sidebar pane removed; agents retained"
            } else {
                "verified missing sidebar registration cleared; no pane touched"
            },
        )?;
        Ok(())
    }
    pub fn sidebar_manager(&self, project: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let clients = self.clients()?;
        ensure!(
            clients.len() <= 1,
            "sidebar supports one interactive client in this pilot"
        );
        ensure!(
            r.projects.iter().any(|p| p.id == project),
            "project not found"
        );
        ensure!(
            self.server()? == r.server_instance,
            "manager server identity changed; controls locked"
        );
        let session_name = format!("p-{}", project.replace('-', ""));
        let manager = format!("{session_name}:manager");
        let raw=self.tm(&["list-panes","-t",&manager,"-F","#{pane_id}\t#{pane_dead}\t#{@pctl_agent}\t#{pane_pid}\t#{session_id}\t#{@pctl_project}"])?;
        let rows: Vec<_> = raw.lines().collect();
        ensure!(
            rows.len() == 1,
            "manager layout identity changed; inspect before focusing"
        );
        let f: Vec<_> = rows[0].split('\t').collect();
        ensure!(
            f.len() == 6
                && valid_native_id(f[0], '%')
                && f[1] == "0"
                && f[2].is_empty()
                && f[3].chars().all(|c| c.is_ascii_digit())
                && valid_native_id(f[4], '$')
                && f[5] == project,
            "live manager identity unavailable; explicitly attach the intended project to restore its manager"
        );
        let start = process_start(f[3])?;
        let exe = std::env::current_exe()?;
        #[cfg(not(target_os = "macos"))]
        let argv: Vec<_> = {
            ensure!(
                fs::canonicalize(format!("/proc/{}/exe", f[3]))? == fs::canonicalize(&exe)?,
                "manager executable identity mismatch; controls locked"
            );
            let bytes = fs::read(format!("/proc/{}/cmdline", f[3]))?;
            bytes
                .split(|b| *b == 0)
                .filter(|v| !v.is_empty())
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .collect()
        };
        // ponytail: macOS ps joins argv with spaces, so compare the joined line; paths containing spaces could alias. Use sysctl KERN_PROCARGS2 if that matters.
        #[cfg(target_os = "macos")]
        let argv: Vec<String> = {
            let out = std::process::Command::new("/bin/ps")
                .args(["-ww", "-o", "args=", "-p", f[3]])
                .output()?;
            vec![String::from_utf8(out.stdout)?.trim_end().to_string()]
        };
        #[cfg(target_os = "macos")]
        let expected = vec![
            [
                exe.to_string_lossy().into_owned(),
                "--state".into(),
                self.state.to_string_lossy().into_owned(),
                "--tmux".into(),
                self.tmux.to_string_lossy().into_owned(),
                "ui".into(),
                "--project".into(),
                project.into(),
            ]
            .join(" "),
        ];
        #[cfg(not(target_os = "macos"))]
        let expected = vec![
            exe.to_string_lossy().into_owned(),
            "--state".into(),
            self.state.to_string_lossy().into_owned(),
            "--tmux".into(),
            self.tmux.to_string_lossy().into_owned(),
            "ui".into(),
            "--project".into(),
            project.into(),
        ];
        ensure!(
            argv == expected && process_start(f[3])? == start,
            "manager command/process identity mismatch; controls locked"
        );
        self.tm(&["select-window", "-t", &manager])?;
        self.tm(&["select-pane", "-t", f[0]])?;
        if let Some(client) = clients.first() {
            self.tm(&[
                "switch-client",
                "-c",
                client
                    .split('|')
                    .next()
                    .context("invalid client identity")?,
                "-t",
                f[4],
            ])?;
        }
        Ok(())
    }
    /// A short-lived separate CLI closes the UI's pane after terminal modes restore.
    /// It is not a daemon and never writes input to an agent pane.
    pub fn request_sidebar_close(&self, sidebar: &str) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("sidebar-close.log"))?;
        Command::new(std::env::current_exe()?)
            // The UI's foreground process group receives PTY SIGHUP on exit.
            // The one-shot closer must survive long enough to persist cleanup.
            .process_group(0)
            .args([
                "--state",
                self.state.to_str().context("invalid state path")?,
                "--tmux",
                self.tmux.to_str().context("invalid tmux path")?,
                "sidebar",
                "close",
                "--id",
                sidebar,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()?;
        Ok(())
    }
}

#[derive(Clone)]
enum Row {
    Project(crate::Project),
    Agent(Box<crate::AgentView>),
}
impl Row {
    fn key(&self) -> String {
        match self {
            Self::Project(p) => format!("p:{}", p.id),
            Self::Agent(a) => format!("a:{}", a.agent.id),
        }
    }
    fn project(&self) -> &str {
        match self {
            Self::Project(p) => &p.id,
            Self::Agent(a) => &a.agent.project_id,
        }
    }
}
fn rows(snapshot: &crate::Snapshot) -> Vec<Row> {
    let mut rows = Vec::new();
    for p in &snapshot.projects {
        rows.push(Row::Project(p.clone()));
        rows.extend(
            snapshot
                .agents
                .iter()
                .filter(|a| a.agent.project_id == p.id)
                .cloned()
                .map(|a| Row::Agent(Box::new(a))),
        );
    }
    rows
}
fn activate(core: &Core, row: &Row, snapshot: &crate::Snapshot) -> Result<()> {
    let view = match row {
        Row::Agent(a) => Some(a.as_ref()),
        Row::Project(p) => snapshot
            .agents
            .iter()
            .find(|a| a.agent.project_id == p.id && a.run.is_some()),
    };
    if let Some(view) = view
        && let Some(run) = &view.run
    {
        core.focus(&view.agent.id, &run.id)
    } else {
        core.sidebar_manager(row.project())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavAction {
    Up,
    Down,
    Activate,
    Manager,
    Close,
}
fn nav_action(key: crossterm::event::KeyEvent) -> Option<NavAction> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        return Some(NavAction::Close);
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(NavAction::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(NavAction::Down),
        KeyCode::Enter => Some(NavAction::Activate),
        KeyCode::Char('m') => Some(NavAction::Manager),
        KeyCode::Char('q') => Some(NavAction::Close),
        _ => None,
    }
}

/// Compact navigation only. Lifecycle/input/profile confirmations stay in full manager.
/// Only visible footer labels are actionable; blank columns never close the pane.
fn sidebar_footer_hits(
    area: ratatui::layout::Rect,
) -> (Option<ratatui::layout::Rect>, Option<ratatui::layout::Rect>) {
    use ratatui::layout::Rect;
    if area.height < 8 || area.width < 11 {
        return (None, None);
    }
    let manager = Some(Rect::new(area.x, area.bottom() - 2, 11, 1));
    let close = if area.width >= 20 {
        Some(Rect::new(
            area.x + 12,
            area.bottom() - 2,
            if area.width >= 21 { 9 } else { 8 },
            1,
        ))
    } else {
        None
    };
    (manager, close)
}

pub fn run(core: &Core, expected_sidebar: &str) -> Result<()> {
    use crossterm::event::{self, Event, MouseButton, MouseEventKind};
    use ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::Rect,
        style::Modifier,
        text::{Line, Span},
        widgets::{Block, Paragraph},
    };
    let sidebar = core.sidebar_record()?.context("sidebar not found")?;
    ensure!(
        sidebar.id == expected_sidebar && sidebar.pid == std::process::id().to_string(),
        "sidebar UI process identity mismatch"
    );
    core.guard_sidebar(&core.registry()?, &sidebar)?;
    let mut snapshot = core.snapshot()?;
    let mut selected = format!("a:{}", sidebar.host_agent_id);
    let mut notice = String::new();
    let mut hits: Vec<(u16, Row)> = Vec::new();
    let mut manager_hit: Option<Rect> = None;
    let mut close_hit: Option<Rect> = None;
    let mut theme = core.theme()?;
    {
        let _terminal_guard = crate::ui::TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        terminal.clear()?;
        let mut input_guard = crate::terminal_input::TerminalInputGuard::default();
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let observer = core.clone();
        std::thread::spawn(move || {
            while request_rx.recv().is_ok() {
                if result_tx
                    .send(observer.snapshot().map_err(|e| e.to_string()))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut awaiting: Option<Instant> = None;
        let mut next_poll = Instant::now() + Duration::from_secs(1);
        let mut dirty = true;
        'ui: loop {
            match core.theme() {
                Ok(next) if theme != next => {
                    theme = next;
                    dirty = true;
                }
                Err(e) if notice != format!("Appearance config: {e}") => {
                    notice = format!("Appearance config: {e}");
                    dirty = true;
                }
                _ => {}
            }
            if let Ok(result) = result_rx.try_recv() {
                awaiting = None;
                match result {
                    Ok(mut next) => {
                        // Observation timestamps alone need not redraw every pane.
                        let mut previous = snapshot.clone();
                        for a in &mut previous.agents {
                            a.observed_at = 0;
                        }
                        for a in &mut next.agents {
                            a.observed_at = 0;
                        }
                        dirty |= previous != next;
                        snapshot = next;
                    }
                    Err(e) => {
                        crate::ui::invalidate_snapshot(&mut snapshot, e);
                        dirty = true;
                    }
                }
            }
            if awaiting.is_some_and(|s| s.elapsed() > Duration::from_millis(1500))
                && snapshot.connected
            {
                crate::ui::invalidate_snapshot(&mut snapshot, "Observation stale".into());
                dirty = true;
            }
            let visible_rows = rows(&snapshot);
            if !visible_rows.iter().any(|r| r.key() == selected) {
                selected = visible_rows.first().map(Row::key).unwrap_or_default();
            }
            let index = visible_rows
                .iter()
                .position(|r| r.key() == selected)
                .unwrap_or(0);
            if dirty {
                terminal.draw(|f| {
                    let area = f.area();
                    let p = theme.palette();
                    hits.clear();
                    manager_hit = None;
                    close_hit = None;
                    f.render_widget(Block::default().style(p.base()), area);
                    if area.height > 0 {
                        let brand = Line::from(vec![
                            Span::styled("◎ pctl ", p.accent()),
                            Span::styled(format!("/ {}", theme.label()), p.body().fg(p.muted)),
                        ]);
                        f.render_widget(Paragraph::new(brand), Rect::new(0, 0, area.width, 1));
                    }
                    if area.height > 1 {
                        f.render_widget(
                            Paragraph::new(if snapshot.connected {
                                " NAV · connected"
                            } else {
                                " NAV · stale / offline"
                            })
                            .style(
                                p.base()
                                    .fg(if snapshot.connected { p.cyan } else { p.warn }),
                            ),
                            Rect::new(0, 1, area.width, 1),
                        );
                    }
                    if area.height > 2 {
                        f.render_widget(
                            Paragraph::new("─".repeat(area.width as usize))
                                .style(p.base().fg(p.border)),
                            Rect::new(0, 2, area.width, 1),
                        );
                    }
                    let capacity = area.height.saturating_sub(8) as usize;
                    let start = index.saturating_sub(capacity.saturating_sub(1));
                    for (offset, row) in visible_rows.iter().skip(start).take(capacity).enumerate()
                    {
                        let y = offset as u16 + 3;
                        let active = row.key() == selected;
                        match row {
                            Row::Project(project) => {
                                let label = format!(
                                    " {} {}",
                                    if active { "▸" } else { "◇" },
                                    crate::ui::fit_label(
                                        &project.name,
                                        area.width.saturating_sub(3)
                                    )
                                );
                                f.render_widget(
                                    Paragraph::new(label).style(
                                        p.body()
                                            .fg(p.cyan)
                                            .bg(if active { p.raised } else { p.panel })
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Rect::new(0, y, area.width, 1),
                                );
                            }
                            Row::Agent(agent) => {
                                let process = match agent.process.as_str() {
                                    "running" => "run",
                                    "not_started" | "stopped" => "off",
                                    s if s.starts_with("exited") => "exit",
                                    _ => "?",
                                };
                                let status = format!(
                                    "[{}]",
                                    if agent.guard_error.is_some() {
                                        "guard!"
                                    } else {
                                        process
                                    }
                                );
                                let status_width =
                                    Line::raw(&status).width().min(area.width as usize) as u16;
                                let width = area.width.saturating_sub(status_width + 1);
                                let name = format!(
                                    " {} {}{}",
                                    if active { "▸" } else { "·" },
                                    crate::sanitize(&agent.agent.name),
                                    if agent.pending_messages > 0 {
                                        format!(" +{}", agent.pending_messages)
                                    } else {
                                        String::new()
                                    }
                                );
                                let style = if active { p.selected() } else { p.body() };
                                f.render_widget(
                                    Paragraph::new(crate::ui::fit_label(&name, width)).style(style),
                                    Rect::new(0, y, width, 1),
                                );
                                f.render_widget(
                                    Paragraph::new(format!(" {status}")).style(if active {
                                        p.selected()
                                    } else {
                                        p.body().fg(if agent.guard_error.is_some() {
                                            p.danger
                                        } else if process == "run" {
                                            p.accent
                                        } else {
                                            p.warn
                                        })
                                    }),
                                    Rect::new(width, y, area.width.saturating_sub(width), 1),
                                );
                            }
                        }
                        hits.push((y, row.clone()));
                    }
                    if area.height >= 8 {
                        let agent = visible_rows.get(index).and_then(|r| match r {
                            Row::Agent(agent) => Some(agent.as_ref()),
                            _ => None,
                        });
                        let task = agent
                            .map(|a| format!(" task: {}", crate::sanitize(&a.task)))
                            .unwrap_or_else(|| " Native panes · exact runs".into());
                        let report = agent
                            .map(|a| {
                                format!(
                                    " self: {}",
                                    a.report
                                        .as_ref()
                                        .map(|r| if r.fresh {
                                            crate::sanitize(&r.report.status)
                                        } else {
                                            "stale".into()
                                        })
                                        .unwrap_or_else(|| "none".into())
                                )
                            })
                            .unwrap_or_else(|| " Enter / click to focus".into());
                        f.render_widget(
                            Paragraph::new("─".repeat(area.width as usize))
                                .style(p.base().fg(p.border)),
                            Rect::new(0, area.height - 5, area.width, 1),
                        );
                        f.render_widget(
                            Paragraph::new(task).style(p.base().fg(p.warn)),
                            Rect::new(0, area.height - 4, area.width, 1),
                        );
                        f.render_widget(
                            Paragraph::new(report).style(p.base().fg(p.muted)),
                            Rect::new(0, area.height - 3, area.width, 1),
                        );
                        let (manager, close) = sidebar_footer_hits(area);
                        if let Some(rect) = manager {
                            f.render_widget(
                                Paragraph::new("[m manager]").style(p.base().fg(p.cyan)),
                                rect,
                            );
                        }
                        if let Some(rect) = close {
                            f.render_widget(
                                Paragraph::new(if area.width >= 21 {
                                    "[q close]"
                                } else {
                                    "[q quit]"
                                })
                                .style(p.base().fg(p.muted)),
                                rect,
                            );
                        }
                        manager_hit = manager;
                        close_hit = close;
                        f.render_widget(
                            Paragraph::new(if notice.is_empty() {
                                " ↑↓ select · Enter focus".into()
                            } else {
                                crate::sanitize(&notice)
                            })
                            .style(
                                p.base()
                                    .fg(if notice.is_empty() { p.muted } else { p.warn }),
                            ),
                            Rect::new(0, area.height - 1, area.width, 1),
                        );
                    }
                })?;
                dirty = false;
            }
            let now = Instant::now();
            let mut wait = input_guard.wait_limit(now, next_poll.saturating_duration_since(now));
            if awaiting.is_some() {
                wait = wait.min(Duration::from_millis(50));
            }
            let mut inputs = input_guard.ready(Instant::now())?;
            if event::poll(if inputs.is_empty() {
                wait
            } else {
                Duration::ZERO
            })? {
                inputs.extend(input_guard.filter(event::read()?, Instant::now())?);
            }
            for input in inputs {
                let mut action: Option<Row> = None;
                match input {
                    Event::Key(k) => {
                        match nav_action(k) {
                            Some(NavAction::Close) => break 'ui,
                            Some(NavAction::Up) if !visible_rows.is_empty() => {
                                selected = visible_rows
                                    [(index + visible_rows.len() - 1) % visible_rows.len()]
                                .key()
                            }
                            Some(NavAction::Down) if !visible_rows.is_empty() => {
                                selected = visible_rows[(index + 1) % visible_rows.len()].key()
                            }
                            Some(NavAction::Activate) => action = visible_rows.get(index).cloned(),
                            Some(NavAction::Manager) => {
                                if let Some(row) = visible_rows.get(index)
                                    && let Err(e) = core.sidebar_manager(row.project())
                                {
                                    notice = e.to_string();
                                }
                            }
                            _ => {}
                        }
                        dirty = true;
                    }
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let contains = |r: Rect| {
                                m.column >= r.x
                                    && m.column < r.right()
                                    && m.row >= r.y
                                    && m.row < r.bottom()
                            };
                            if close_hit.is_some_and(contains) {
                                break 'ui;
                            }
                            if manager_hit.is_some_and(contains) {
                                if let Some(row) = visible_rows.get(index)
                                    && let Err(e) = core.sidebar_manager(row.project())
                                {
                                    notice = e.to_string();
                                }
                                dirty = true;
                            }
                            if let Some((_, row)) = hits.iter().find(|(y, _)| *y == m.row) {
                                selected = row.key();
                                action = Some(row.clone());
                                dirty = true;
                            }
                        }
                        MouseEventKind::ScrollUp if !visible_rows.is_empty() => {
                            selected = visible_rows[index.saturating_sub(1)].key();
                            dirty = true;
                        }
                        MouseEventKind::ScrollDown if !visible_rows.is_empty() => {
                            selected = visible_rows[(index + 1).min(visible_rows.len() - 1)].key();
                            dirty = true;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
                if let Some(row) = action {
                    notice = match activate(core, &row, &snapshot) {
                        Ok(()) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    dirty = true;
                }
            }
            if Instant::now() >= next_poll {
                if awaiting.is_none() && request_tx.try_send(()).is_ok() {
                    awaiting = Some(Instant::now());
                }
                next_poll = Instant::now() + Duration::from_secs(1);
            }
        }
    }
    core.request_sidebar_close(expected_sidebar)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    #[test]
    fn navigation_never_maps_lifecycle_input_or_approval_shortcuts() {
        for ch in [
            's', 'S', 'x', 'r', 'y', 'u', 'i', 'a', 'e', 'N', 'd', 'z', 'p', 'b',
        ] {
            assert_eq!(
                nav_action(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                None
            );
        }
        assert_eq!(
            nav_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(NavAction::Activate)
        );
        assert_eq!(
            nav_action(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
            Some(NavAction::Manager)
        );
    }
    #[test]
    fn modified_q_does_not_close_and_control_c_closes_only_navigation() {
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert_eq!(
                nav_action(KeyEvent::new(KeyCode::Char('q'), modifier)),
                None
            );
        }
        assert_eq!(
            nav_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(NavAction::Close)
        );
        assert_eq!(
            nav_action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(NavAction::Close)
        );
    }
    #[test]
    fn sidebar_state_is_optional_and_schema_bound() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().into(), "/not-a-tmux".into()).unwrap();
        assert!(c.sidebar_record().unwrap().is_none());
        fs::write(d.path().join("sidebar.json"), "x".repeat(8193)).unwrap();
        assert!(
            c.sidebar_record()
                .unwrap_err()
                .to_string()
                .contains("read limit")
        );
    }
    #[test]
    fn grouped_rows_use_immutable_keys_even_when_names_match() {
        let mut snapshot = crate::Snapshot {
            projects: vec![],
            agents: vec![],
            connected: false,
            error: None,
        };
        for id in ["one", "two"] {
            snapshot.projects.push(crate::Project {
                id: id.into(),
                name: "same".into(),
                root: "/tmp".into(),
            });
        }
        let rows = rows(&snapshot);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].key(), rows[1].key());
        assert_eq!(rows[1].project(), "two");
    }
    #[test]
    fn inspect_exposes_ambiguous_intent_without_attempting_launch() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().into(), "/not-a-tmux".into()).unwrap();
        atomic_json(
            &d.path().join("pending-sidebar.json"),
            &serde_json::json!({"sidebar_id":"pending"}),
        )
        .unwrap();
        let status = c.inspect_sidebar().unwrap();
        assert_eq!(status["pending"]["sidebar_id"], "pending");
        assert!(status["sidebar"].is_null());
        assert!(
            c.close_sidebar("pending")
                .unwrap_err()
                .to_string()
                .contains("identity unavailable")
        );
        assert!(d.path().join("pending-sidebar.json").exists());
    }
    #[test]
    fn footer_hits_cover_only_visible_labels_at_every_supported_width() {
        use ratatui::layout::Rect;
        for width in 20..=40 {
            let (manager, close) = sidebar_footer_hits(Rect::new(0, 0, width, 24));
            let manager = manager.unwrap();
            let close = close.unwrap();
            assert_eq!(manager, Rect::new(0, 22, 11, 1));
            assert_eq!(close.x, 12);
            assert!(close.right() <= width);
            assert!(manager.right() < close.x);
            assert_eq!(close.right(), if width >= 21 { 21 } else { 20 });
        }
        assert_eq!(sidebar_footer_hits(Rect::new(0, 0, 40, 7)), (None, None));
    }
}
