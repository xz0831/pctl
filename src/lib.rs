//! Shared registry, observation and guarded control core. Agent output stays in native tmux panes.
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
pub mod collaboration;
pub mod orchestration;
pub mod session;
pub mod sidebar;
pub mod terminal_input;
pub mod theme;
pub mod ui;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '�' } else { c })
        .collect()
}
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub adapter: String,
    #[serde(default = "native_input")]
    pub input_mode: String,
    #[serde(default = "collaboration::disabled_collaboration")]
    pub collaboration_mode: String,
    /// Harness conversation ID (Claude session, Codex thread); starts resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub id: String,
    pub agent_id: String,
    pub project_id: String,
    pub server_instance: String,
    pub session: String,
    pub pane: String,
    pub window: String,
    pub pid: String,
    pub process_start: String,
    pub started_at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Registry {
    pub schema_version: u32,
    pub server_instance: String,
    pub projects: Vec<Project>,
    pub agents: Vec<Agent>,
    pub runs: Vec<Run>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            server_instance: id(),
            projects: vec![],
            agents: vec![],
            runs: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentView {
    pub agent: Agent,
    pub run: Option<Run>,
    pub process: String,
    pub task: String,
    pub evidence: String,
    pub observed_at: u64,
    pub guard_error: Option<String>,
    #[serde(default)]
    pub pending_messages: usize,
    #[serde(default)]
    pub report: Option<collaboration::ReportView>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub projects: Vec<Project>,
    pub agents: Vec<AgentView>,
    pub connected: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
struct Pane {
    session: String,
    window: String,
    pane: String,
    pid: String,
    dead: bool,
    exit: String,
    run: String,
    agent: String,
    project: String,
    in_mode: bool,
    synchronized: bool,
}
const PANE_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_dead_status}\t#{@pctl_run}\t#{@pctl_agent}\t#{@pctl_project}\t#{pane_in_mode}\t#{synchronize-panes}";
fn parse_panes(s: &str) -> Result<Vec<Pane>> {
    s.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p: Vec<_> = l.split('\t').collect();
            ensure!(p.len() == 11, "unexpected tmux snapshot format");
            Ok(Pane {
                session: p[0].into(),
                window: p[1].into(),
                pane: p[2].into(),
                pid: p[3].into(),
                dead: p[4] == "1",
                exit: p[5].into(),
                run: p[6].into(),
                agent: p[7].into(),
                project: p[8].into(),
                in_mode: p[9] != "0",
                synchronized: p[10] == "1" || p[10] == "on",
            })
        })
        .collect()
}
fn parse_snapshot_panes(output: &str, expected_instance: &str) -> Result<Vec<Pane>> {
    let (marker, rows) = output
        .split_once('\n')
        .context("unexpected tmux snapshot marker/panes format")?;
    let canonical = Uuid::parse_str(expected_instance)
        .context("server identity is not a valid UUID")?
        .hyphenated()
        .to_string();
    ensure!(
        canonical == expected_instance,
        "server identity is not a canonical UUID"
    );
    // Named option output uses tmux args_escape, unlike raw -v output. Requiring
    // this exact simple UUID line rejects escaped/newline marker suffixes.
    ensure!(
        marker == format!("@pctl_instance {expected_instance}"),
        "server identity changed"
    );
    let panes = parse_panes(rows)?;
    ensure!(!panes.is_empty(), "tmux snapshot contained no panes");
    Ok(panes)
}

#[derive(Clone, Debug)]
pub struct Core {
    pub state: PathBuf,
    pub tmux: PathBuf,
}

impl Core {
    pub fn new(state: PathBuf, tmux: PathBuf) -> Result<Self> {
        fs::create_dir_all(&state)?;
        let state = fs::canonicalize(state)?;
        ensure!(
            state.join("tmux.sock").as_os_str().len() < 104,
            "state path too long for a private Unix socket (use a shorter --state)"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        }
        let core = Self { state, tmux };
        let _lock = core.lock()?;
        if !core.state.join("registry.json").exists() {
            core.save(&Registry::default())?;
        }
        Ok(core)
    }
    fn lock(&self) -> Result<File> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.state.join("registry.lock"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match f.try_lock_exclusive() {
                Ok(()) => return Ok(f),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(
                        Instant::now() < deadline,
                        "registry lock timeout; another operation is still active"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    pub fn registry(&self) -> Result<Registry> {
        let r: Registry = serde_json::from_slice(&fs::read(self.state.join("registry.json"))?)?;
        ensure!(r.schema_version == 1, "unsupported registry schema");
        Ok(r)
    }
    fn save(&self, r: &Registry) -> Result<()> {
        let target = self.state.join("registry.json");
        atomic_json(&target, r)
    }
    fn tmux_cmd(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        // Force the tmux client's UTF-8 transport independently of a harness's
        // minimal LANG/LC_* environment. Non-UTF8 clients sanitize our protocol
        // tab separators to underscores; never relax the strict row parser.
        c.arg("-u")
            .arg("-S")
            .arg(self.state.join("tmux.sock"))
            .arg("-f")
            .arg("/dev/null");
        c.env_remove("TMUX");
        // A server started from a Herdr pane would copy that pane's identity into every PCTL
        // pane (msg sender, Herdr hooks). Permissions follow process lineage, not environment.
        for (k, _) in std::env::vars_os() {
            if k.to_str()
                .is_some_and(|k| k.starts_with("HERDR_") || k == "CODEX_THREAD_ID")
            {
                c.env_remove(k);
            }
        }
        c
    }
    /// File-backed subprocess I/O avoids pipe deadlocks even if an unexpected
    /// descendant retains a stdout descriptor. No agent terminal output is read.
    fn command_output(&self, command: &mut Command, input: Option<&[u8]>) -> Result<Output> {
        let prefix = format!("command-{}", id());
        let paths: Vec<_> = ["out", "err", "in"]
            .iter()
            .map(|s| self.state.join(format!("{prefix}.{s}")))
            .collect();
        let result = (|| -> Result<Output> {
            let stdout = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&paths[0])?;
            let stderr = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&paths[1])?;
            command
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            if let Some(input) = input {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&paths[2])?;
                file.write_all(input)?;
                drop(file);
                command.stdin(Stdio::from(File::open(&paths[2])?));
            } else {
                command.stdin(Stdio::null());
            }
            let mut child = command
                .spawn()
                .with_context(|| format!("cannot execute {}", self.tmux.display()))?;
            // Blocking wait on a helper thread: macOS timer coalescing stretches a 10 ms
            // poll sleep to ~70 ms, which made every tmux call ~80 ms.
            let pid = child.id();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(child.wait());
            });
            let status = match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(status) => status?,
                Err(_) => {
                    let _ = Command::new("/bin/kill")
                        .args(["-9", &pid.to_string()])
                        .status();
                    bail!(
                        "tmux command timeout; outcome_unknown for mutations; inspect before retrying"
                    );
                }
            };
            Ok(Output {
                status,
                stdout: fs::read(&paths[0])?,
                stderr: fs::read(&paths[1])?,
            })
        })();
        for path in paths {
            let _ = fs::remove_file(path);
        }
        result
    }
    pub fn tm(&self, args: &[&str]) -> Result<String> {
        let o = self.command_output(self.tmux_cmd().args(args), None)?;
        ensure!(
            o.status.success(),
            "tmux {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&o.stderr).trim()
        );
        Ok(String::from_utf8(o.stdout)?
            .trim_end_matches('\n')
            .to_string())
    }
    fn tm_input(&self, args: &[&str], input: &[u8]) -> Result<String> {
        let o = self.command_output(self.tmux_cmd().args(args), Some(input))?;
        ensure!(
            o.status.success(),
            "tmux write failed; outcome_unknown: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        Ok(String::from_utf8(o.stdout)?)
    }
    fn server(&self) -> Result<String> {
        self.tm(&["show-options", "-gqv", "@pctl_instance"])
    }
    fn panes(&self) -> Result<Vec<Pane>> {
        let output = self.tm(&["list-panes", "-a", "-F", PANE_FORMAT])?;
        parse_panes(&output)
    }
    /// Observation only: one tmux client reads the marker and panes in order.
    /// Mutation guards deliberately retain their independent fresh queries.
    fn snapshot_panes(&self, expected_instance: &str) -> Result<Vec<Pane>> {
        let output = self.tm(&[
            "show-options",
            "-gq",
            "@pctl_instance",
            ";",
            "list-panes",
            "-a",
            "-F",
            PANE_FORMAT,
        ])?;
        parse_snapshot_panes(&output, expected_instance)
    }
    pub fn register_project(&self, name: &str, root: &Path) -> Result<Project> {
        validate_name(name)?;
        let root = fs::canonicalize(root)?;
        ensure!(root.is_dir(), "project root is not a directory");
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        let p = Project {
            id: id(),
            name: name.into(),
            root: root.to_string_lossy().into(),
        };
        r.projects.push(p.clone());
        self.save(&r)?;
        Ok(p)
    }
    pub fn rename_project(&self, project: &str, name: &str) -> Result<()> {
        validate_name(name)?;
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        r.projects
            .iter_mut()
            .find(|p| p.id == project)
            .context("project not found")?
            .name = name.into();
        self.save(&r)
    }
    pub fn register_agent(
        &self,
        project: &str,
        name: &str,
        cwd: &Path,
        argv: Vec<String>,
        adapter: &str,
    ) -> Result<Agent> {
        validate_name(name)?;
        validate_argv(&argv)?;
        ensure!(
            adapter == "generic" || adapter == "synthetic",
            "adapter must be generic or synthetic"
        );
        if adapter == "synthetic" {
            ensure_fixture_command(&argv)?;
        }
        let cwd = fs::canonicalize(cwd)?;
        ensure!(cwd.is_dir(), "agent cwd is not a directory");
        ensure!(argv.iter().all(|a| !a.contains('\0')), "argv contains NUL");
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        ensure!(
            r.projects.iter().any(|p| p.id == project),
            "project not found"
        );
        let a = Agent {
            id: id(),
            project_id: project.into(),
            name: name.into(),
            cwd: cwd.to_string_lossy().into(),
            argv,
            adapter: adapter.into(),
            input_mode: native_input(),
            collaboration_mode: collaboration::disabled_collaboration(),
            conversation: None,
        };
        r.agents.push(a.clone());
        self.save(&r)?;
        Ok(a)
    }
    /// Change a stopped profile without changing its immutable agent identity.
    /// Existing and unresolved runs must be explicitly reconciled first.
    pub fn update_agent(
        &self,
        agent: &str,
        name: &str,
        cwd: &Path,
        argv: Vec<String>,
        adapter: &str,
    ) -> Result<Agent> {
        validate_name(name)?;
        validate_argv(&argv)?;
        ensure!(
            adapter == "generic" || adapter == "synthetic",
            "adapter must be generic or synthetic"
        );
        if adapter == "synthetic" {
            ensure_fixture_command(&argv)?;
        }
        let cwd = fs::canonicalize(cwd)?;
        ensure!(cwd.is_dir(), "agent cwd is not a directory");
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        ensure!(
            !r.runs.iter().any(|run| run.agent_id == agent),
            "profile has a current run; explicitly terminate it before editing"
        );
        ensure!(
            !self.state.join(format!("pending-{agent}.json")).exists(),
            "unresolved launch intent; profile changes locked"
        );
        let a = r
            .agents
            .iter_mut()
            .find(|a| a.id == agent)
            .context("agent not found")?;
        a.name = name.into();
        a.cwd = cwd.to_string_lossy().into();
        a.argv = argv;
        a.adapter = adapter.into();
        let result = a.clone();
        self.save(&r)?;
        Ok(result)
    }
    pub fn configure_input(&self, agent: &str, mode: &str) -> Result<()> {
        ensure!(
            mode == "native" || mode == "terminal-line",
            "input mode must be native or terminal-line"
        );
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        ensure!(
            !r.runs.iter().any(|run| run.agent_id == agent),
            "profile has a current run; explicitly terminate it before changing input mode"
        );
        ensure!(
            !self.state.join(format!("pending-{agent}.json")).exists(),
            "unresolved launch intent; profile changes locked"
        );
        r.agents
            .iter_mut()
            .find(|a| a.id == agent)
            .context("agent not found")?
            .input_mode = mode.into();
        self.save(&r)
    }
    fn manager_command(&self, project: &str) -> Result<String> {
        let exe = std::env::current_exe()?.to_string_lossy().into_owned();
        Ok([
            exe,
            "--state".into(),
            self.state.to_string_lossy().into(),
            "--tmux".into(),
            self.tmux.to_string_lossy().into(),
            "ui".into(),
            "--project".into(),
            project.into(),
        ]
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" "))
    }
    fn ensure_session(&self, r: &Registry, project: &str) -> Result<String> {
        let name = format!("p-{}", project.replace('-', ""));
        if let Ok(marker) = self.server() {
            ensure!(
                marker == r.server_instance,
                "server identity changed; controls locked"
            );
        } else {
            ensure!(
                r.runs.is_empty(),
                "server disconnected with existing runs; controls locked"
            );
            server_start_allowed()?;
        }
        // display-message -t is a pane target: a missing session can return an empty
        // format successfully. Enumerate sessions and match the generated name exactly.
        if let Ok(sessions) = self.tm(&[
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_id}\t#{@pctl_project}",
        ]) {
            for line in sessions.lines() {
                let fields: Vec<_> = line.split('\t').collect();
                if fields.first() == Some(&name.as_str()) {
                    ensure!(
                        fields.len() == 3
                            && valid_native_id(fields[1], '$')
                            && fields[2] == project,
                        "invalid registered session identity"
                    );
                    return Ok(fields[1].to_string());
                }
            }
        }
        let cmd = self.manager_command(project)?;
        let session = self.tm(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "-n",
            "manager",
            "-x",
            "120",
            "-y",
            "36",
            "-P",
            "-F",
            "#{session_id}",
            &cmd,
        ])?;
        ensure!(
            valid_native_id(&session, '$'),
            "new session identity unavailable; inspect before retrying"
        );
        let marker = self.server()?;
        if marker.is_empty() {
            self.tm(&["set-option", "-g", "@pctl_instance", &r.server_instance])?;
        } else {
            ensure!(
                marker == r.server_instance,
                "server identity changed; controls locked"
            );
        }
        self.tm(&["set-option", "-g", "mouse", "on"])?;
        self.tm(&["set-option", "-g", "history-limit", "2000"])?;
        self.tm(&["set-option", "-g", "set-clipboard", "off"])?;
        self.tm(&["set-option", "-g", "remain-on-exit", "on"])?;
        self.apply_theme_chrome()?;
        self.tm(&["set-option", "-t", &session, "@pctl_project", project])?;
        self.tm(&[
            "set-option",
            "-w",
            "-t",
            &format!("{session}:manager"),
            "synchronize-panes",
            "off",
        ])?;
        // Forward manager/agent application mouse requests; keep terminal clipboard policy untouched.
        self.tm(&["bind-key", "-n", "MouseDown3Pane", "send-keys", "-M"])?;
        self.tm(&["bind-key", "-n", "MouseUp3Pane", "send-keys", "-M"])?;
        // Prefix+m returns to this session's dedicated manager window.
        self.tm(&["bind-key", "m", "select-window", "-t", ":manager"])?;
        Ok(session)
    }
    fn start_locked(&self, r: &mut Registry, agent: &str) -> Result<Run> {
        let a = r
            .agents
            .iter()
            .find(|a| a.id == agent)
            .context("agent not found")?
            .clone();
        let intent = self.state.join(format!("pending-{agent}.json"));
        ensure!(
            !intent.exists(),
            "unresolved launch intent; inspect isolated server and registry before retrying"
        );
        ensure!(
            !r.runs.iter().any(|v| v.agent_id == agent),
            "existing run; use restart with expected-run (including exited runs)"
        );
        let session = self.ensure_session(r, &a.project_id)?;
        let run_id = id();
        fs::create_dir_all(self.state.join("launchers"))?;
        fs::create_dir_all(self.state.join("events"))?;
        let launcher = self.state.join("launchers").join(format!("{run_id}.sh"));
        let event = self.state.join("events").join(format!("{run_id}.json"));
        let gate = self.state.join("launchers").join(format!("{run_id}.ready"));
        let script = format!(
            "#!/bin/sh\nwhile [ ! -f {} ]; do sleep 0.05; done\ncd -- {} || exit 126\nexport PCTL_RUN_ID={}\nexport PCTL_EVENT_FILE={}\nexport PCTL_BIN={}\nexport PCTL_STATE={}\nexport PCTL_TMUX={}\nexport PCTL_PROJECT_ID={}\nexport PCTL_AGENT_ID={}\nexec {}\n",
            shell_quote(&gate.to_string_lossy()),
            shell_quote(&a.cwd),
            shell_quote(&run_id),
            shell_quote(&event.to_string_lossy()),
            shell_quote(&std::env::current_exe()?.to_string_lossy()),
            shell_quote(&self.state.to_string_lossy()),
            shell_quote(&self.tmux.to_string_lossy()),
            shell_quote(&a.project_id),
            shell_quote(&a.id),
            session::launch_argv(r, agent)?
                .iter()
                .map(|s| shell_quote(s))
                .collect::<Vec<_>>()
                .join(" ")
        );
        fs::write(&launcher, script)?;
        let launch = format!("exec /bin/sh {}", shell_quote(&launcher.to_string_lossy()));
        atomic_json(
            &intent,
            &serde_json::json!({"agent_id":agent,"run_id":run_id,"session":session,"created_at":now(),"state":"pending_native_pane"}),
        )?;
        let raw = self.tm(&[
            "new-window",
            "-d",
            "-t",
            &session,
            "-n",
            &format!("a-{}", &a.id[..8]),
            "-P",
            "-F",
            "#{pane_id}\t#{window_id}\t#{pane_pid}",
            &launch,
        ])?;
        let parts: Vec<_> = raw.split('\t').collect();
        ensure!(
            parts.len() == 3,
            "unexpected launch result; inspect isolated server before retrying"
        );
        ensure!(
            valid_native_id(parts[0], '%')
                && valid_native_id(parts[1], '@')
                && !parts[2].is_empty()
                && parts[2].chars().all(|c| c.is_ascii_digit()),
            "invalid pane/window/process identity; launch remains gated"
        );
        let run = Run {
            id: run_id,
            agent_id: a.id,
            project_id: a.project_id,
            server_instance: r.server_instance.clone(),
            session,
            pane: parts[0].into(),
            window: parts[1].into(),
            pid: parts[2].into(),
            process_start: process_start(parts[2]).context(
                "cannot identify new pane process; launch remains gated; inspect before retrying",
            )?,
            started_at: now(),
        };
        // Commit the generation before enabling the real command. Any subsequent failure leaves
        // a blocked launcher and registered run; Start cannot accidentally duplicate the command.
        r.runs.retain(|x| x.agent_id != agent);
        r.runs.push(run.clone());
        self.save(r)?;
        for (key, value) in [
            ("@pctl_run", &run.id),
            ("@pctl_agent", &run.agent_id),
            ("@pctl_project", &run.project_id),
        ] {
            self.tm(&["set-option", "-p", "-t", &run.pane, key, value])?;
        }
        self.tm(&[
            "set-option",
            "-w",
            "-t",
            &run.window,
            "synchronize-panes",
            "off",
        ])?;
        self.audit("start", &run.id, "registered; releasing launch gate")?;
        fs::write(&gate, b"ready\n")?;
        fs::remove_file(intent)?;
        Ok(run)
    }
    pub fn start(&self, agent: &str) -> Result<Run> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        self.start_locked(&mut r, agent)
    }
    fn guard(&self, r: &Registry, agent: &str, expected: &str, alive: bool) -> Result<Pane> {
        let run = r
            .runs
            .iter()
            .find(|x| x.agent_id == agent)
            .context("no current run")?;
        ensure!(
            run.id == expected,
            "stale_run: expected-run does not match current run"
        );
        ensure!(
            self.server()? == run.server_instance,
            "server identity changed; controls locked"
        );
        let panes = self.panes()?;
        let p = panes
            .iter()
            .find(|p| p.pane == run.pane)
            .context("pane missing; controls locked")?
            .clone();
        check_identity(run, &p)?;
        ensure!(
            panes.iter().filter(|p| p.pane == run.pane).count() == 1,
            "linked window is unsupported; controls locked"
        );
        if !p.dead {
            ensure!(
                !run.process_start.is_empty()
                    && process_start(&p.pid)
                        .context("live process identity unavailable; controls locked")?
                        == run.process_start,
                "process identity changed (external respawn); controls locked"
            );
        }
        if alive {
            ensure!(!p.dead, "pane process exited");
        }
        Ok(p)
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        let r = self.registry()?;
        let observation = self.snapshot_panes(&r.server_instance);
        let connected = observation.is_ok();
        let error = observation.as_ref().err().map(|e| e.to_string());
        let mailboxes = self.mailbox_summaries(&r)?;
        let agents = r
            .agents
            .iter()
            .map(|a| {
                let mut run = r.runs.iter().find(|v| v.agent_id == a.id).cloned();
                let mut guard_error = None;
                let process = if let Some(run) = &mut run {
                    match &observation {
                        Err(_) => "unknown".into(),
                        Ok(panes) => match panes.iter().find(|p| p.pane == run.pane) {
                            None => "missing".into(),
                            Some(p) => {
                                run.window = p.window.clone();
                                if panes.iter().filter(|v| v.pane == run.pane).count() != 1 {
                                    guard_error = Some(
                                        "linked window is unsupported; controls locked".into(),
                                    );
                                    "unknown".into()
                                } else if let Err(e) = check_identity(run, p) {
                                    guard_error = Some(e.to_string());
                                    "unknown".into()
                                } else if !p.dead
                                    && (run.process_start.is_empty()
                                        || process_start(&p.pid).ok().as_deref()
                                            != Some(run.process_start.as_str()))
                                {
                                    guard_error =
                                        Some("process identity changed (external respawn)".into());
                                    "unknown".into()
                                } else if p.dead {
                                    format!("exited({})", p.exit)
                                } else if !self
                                    .state
                                    .join("launchers")
                                    .join(format!("{}.ready", run.id))
                                    .exists()
                                {
                                    "starting".into()
                                } else {
                                    "running".into()
                                }
                            }
                        },
                    }
                } else {
                    "not_started".into()
                };
                let mut task = "unknown".to_string();
                let mut evidence = if connected {
                    "observed:tmux"
                } else {
                    "unknown"
                }
                .to_string();
                if a.adapter == "synthetic"
                    && guard_error.is_none()
                    && connected
                    && process == "running"
                    && let Some(run) = &run
                {
                    if let Ok(event) =
                        read_event(&self.state.join("events").join(format!("{}.json", run.id)))
                    {
                        if event.run_id == run.id
                            && event.observed_at <= now()
                            && now().saturating_sub(event.observed_at) <= 10
                            && [
                                "working",
                                "waiting_input",
                                "waiting_approval",
                                "completed",
                                "unknown",
                            ]
                            .contains(&event.state.as_str())
                        {
                            task = event.state;
                            evidence = "observed:synthetic_fixture".into();
                        } else {
                            evidence =
                                "tmux current; synthetic task evidence stale or invalid".into();
                        }
                    } else {
                        evidence = "tmux current; no valid synthetic task event".into();
                    }
                }
                let pending_messages = run
                    .as_ref()
                    .map(|run| *mailboxes.get(&run.id).unwrap_or(&0))
                    .unwrap_or(0);
                let mut report = run
                    .as_ref()
                    .and_then(|run| self.load_report(a, run).ok().flatten());
                if let Some(report) = &mut report {
                    report.fresh &= process == "running" && guard_error.is_none();
                }
                AgentView {
                    pending_messages,
                    report,
                    agent: a.clone(),
                    run,
                    process,
                    task,
                    evidence,
                    observed_at: now(),
                    guard_error,
                }
            })
            .collect();
        Ok(Snapshot {
            projects: r.projects,
            agents,
            connected,
            error,
        })
    }
    pub fn focus(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.focus_locked(&r, agent, expected)
    }
    fn focus_locked(&self, r: &Registry, agent: &str, expected: &str) -> Result<()> {
        let p = self.guard(r, agent, expected, false)?;
        let clients = self.clients()?;
        ensure!(
            clients.len() <= 1,
            "focus supports one interactive client in this pilot"
        );
        let p = if self.follow_sidebar_locked(r, &p, agent, expected)? {
            self.guard(r, agent, expected, false)?
        } else {
            p
        };
        self.tm(&["select-window", "-t", &p.window])?;
        self.tm(&["select-pane", "-t", &p.pane])?;
        if let Some(client) = clients.first() {
            let name = client
                .split('|')
                .next()
                .context("invalid client identity")?;
            self.tm(&["switch-client", "-c", name, "-t", &p.session])?;
        }
        Ok(())
    }
    pub fn zoom(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.zoom_locked(&r, agent, expected)
    }
    fn zoom_locked(&self, r: &Registry, agent: &str, expected: &str) -> Result<()> {
        let p = self.guard(r, agent, expected, false)?;
        self.tm(&["resize-pane", "-Z", "-t", &p.pane])?;
        Ok(())
    }
    pub fn pair(&self, agent: &str, expected: &str, other: &str, other_run: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.pair_locked(&r, agent, expected, other, other_run)
    }
    fn pair_locked(
        &self,
        r: &Registry,
        agent: &str,
        expected: &str,
        other: &str,
        other_run: &str,
    ) -> Result<()> {
        let p = self.guard(r, agent, expected, false)?;
        let q = self.guard(r, other, other_run, false)?;
        ensure!(p.project == q.project, "cross-project layout is prohibited");
        ensure!(p.pane != q.pane, "cannot pair a pane with itself");
        ensure!(
            !p.synchronized && !q.synchronized,
            "synchronize-panes must be off before layout changes"
        );
        if p.window != q.window {
            self.tm(&["join-pane", "-d", "-h", "-s", &q.pane, "-t", &p.pane])?;
        }
        self.tm(&[
            "set-option",
            "-w",
            "-t",
            &p.window,
            "synchronize-panes",
            "off",
        ])?;
        Ok(())
    }
    pub fn split(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.split_locked(&r, agent, expected)
    }
    fn split_locked(&self, r: &Registry, agent: &str, expected: &str) -> Result<()> {
        let p = self.guard(r, agent, expected, false)?;
        ensure!(
            !p.synchronized,
            "synchronize-panes must be off before layout changes"
        );
        let sidebar_pane = self.sidebar_record()?.map(|s| s.pane);
        let count = self
            .panes()?
            .iter()
            .filter(|v| v.window == p.window && Some(&v.pane) != sidebar_pane.as_ref())
            .count();
        if count > 1 {
            self.tm(&[
                "break-pane",
                "-d",
                "-s",
                &p.pane,
                "-t",
                &format!("{}:", p.session),
            ])?;
        }
        Ok(())
    }
    pub fn terminate(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        self.terminate_locked(&mut r, agent, expected)
    }
    fn terminate_locked(&self, r: &mut Registry, agent: &str, expected: &str) -> Result<()> {
        let p = self.guard(r, agent, expected, false)?;
        self.tm(&["kill-pane", "-t", &p.pane])?;
        ensure!(
            !self.panes()?.iter().any(|v| v.pane == p.pane),
            "termination outcome_unknown"
        );
        r.runs.retain(|x| x.agent_id != agent);
        self.save(r)?;
        self.audit(
            "terminate",
            expected,
            "pane_removed; external descendants not guaranteed",
        )
    }
    pub fn restart(&self, agent: &str, expected: &str) -> Result<Run> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        self.restart_locked(&mut r, agent, expected)
    }
    fn restart_locked(&self, r: &mut Registry, agent: &str, expected: &str) -> Result<Run> {
        let p = self.guard(r, agent, expected, false)?;
        self.tm(&["kill-pane", "-t", &p.pane])?;
        ensure!(
            !self.panes()?.iter().any(|v| v.pane == p.pane),
            "restart outcome_unknown; no new run created"
        );
        r.runs.retain(|x| x.agent_id != agent);
        self.save(r)?;
        self.audit("restart", expected, "old_pane_removed")?;
        self.start_locked(r, agent)
    }
    pub fn save_draft(&self, agent: &str, expected: &str, text: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.save_draft_locked(&r, agent, expected, text)
    }
    fn save_draft_locked(
        &self,
        r: &Registry,
        agent: &str,
        expected: &str,
        text: &str,
    ) -> Result<()> {
        let run = r
            .runs
            .iter()
            .find(|r| r.agent_id == agent)
            .context("no current run")?;
        ensure!(
            run.id == expected,
            "stale_run: draft binding no longer matches"
        );
        ensure!(text.len() <= 1024 * 1024, "draft exceeds 1 MiB");
        fs::create_dir_all(self.state.join("drafts"))?;
        atomic_json(
            &self.state.join("drafts").join(format!("{}.json", run.id)),
            &serde_json::json!({"agent_id":agent,"run_id":expected,"text":text,"saved_at":now(),"sent":false}),
        )
    }
    pub fn load_draft(&self, agent: &str, expected: &str) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let run = r
            .runs
            .iter()
            .find(|r| r.agent_id == agent)
            .context("no current run")?;
        ensure!(
            run.id == expected,
            "stale_run: draft binding no longer matches"
        );
        let path = self.state.join("drafts").join(format!("{expected}.json"));
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let draft: serde_json::Value = serde_json::from_slice(&raw)?;
        ensure!(
            draft["agent_id"] == agent && draft["run_id"] == expected,
            "draft identity mismatch"
        );
        Ok(Some(
            draft["text"].as_str().context("invalid draft text")?.into(),
        ))
    }
    /// Deliver the literal Ctrl-C key only. A raw-mode harness may interpret it
    /// differently; this does not assert SIGINT or task cancellation.
    pub fn send_ctrl_c(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.send_ctrl_c_locked(&r, agent, expected)
    }
    fn send_ctrl_c_locked(&self, r: &Registry, agent: &str, expected: &str) -> Result<()> {
        let pane = self.guard(r, agent, expected, true)?;
        ensure!(!pane.in_mode, "pane is in copy/other mode");
        ensure!(
            !pane.synchronized,
            "synchronize-panes is enabled; input rejected"
        );
        match self.tm(&["send-keys", "-t", &pane.pane, "C-c"]) {
            Ok(_) => self
                .audit(
                    "ctrl_c_key",
                    expected,
                    "key_delivered; interruption unverified",
                )
                .context("key_delivered but audit failed; do not retry"),
            Err(error) => bail!("outcome_unknown; do not retry automatically: {error}"),
        }
    }
    /// Explicit, opt-in PTY input. Paste is transmission and may be interpreted
    /// immediately by a harness; it is never represented as a safe local draft.
    pub fn paste_line(&self, agent: &str, expected: &str, text: &str) -> Result<()> {
        ensure!(
            !text.is_empty() && !text.chars().any(char::is_control),
            "terminal paste accepts one nonempty printable line only"
        );
        ensure!(text.len() <= 4096, "terminal input exceeds 4096 bytes");
        let _lock = self.lock()?;
        let r = self.registry()?;
        ensure_terminal_input(&r, agent)?;
        self.deliver_text(
            &r,
            agent,
            expected,
            text.as_bytes(),
            "terminal_paste",
            false,
        )
    }
    /// Wake a recipient with one notice line and Enter. Only for terminal-line opt-in agents.
    pub(crate) fn wake(&self, agent: &str, run: &str, text: &str) -> Result<&'static str> {
        if ensure_terminal_input(&self.registry()?, agent).is_err() {
            return Ok("skipped: recipient input mode is native");
        }
        self.paste_line(agent, run, text)?;
        // Codex treats an Enter right after a paste burst as a newline; wait it out.
        std::thread::sleep(Duration::from_millis(500));
        self.submit_line(agent, run)?;
        Ok("delivered; acceptance unverified")
    }
    pub fn submit_line(&self, agent: &str, expected: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        ensure_terminal_input(&r, agent)?;
        let pane = self.guard(&r, agent, expected, true)?;
        ensure!(!pane.in_mode, "pane is in copy/other mode");
        ensure!(
            !pane.synchronized,
            "synchronize-panes is enabled; input rejected"
        );
        match self.tm(&["send-keys", "-t", &pane.pane, "Enter"]) {
            Ok(_) => self
                .audit(
                    "terminal_enter",
                    expected,
                    "key_delivered; acceptance unverified",
                )
                .context("key_delivered but audit failed; do not retry"),
            Err(error) => bail!("outcome_unknown; do not retry automatically: {error}"),
        }
    }
    fn deliver_text(
        &self,
        r: &Registry,
        agent: &str,
        expected: &str,
        text: &[u8],
        action: &str,
        bracketed: bool,
    ) -> Result<()> {
        let p = self.guard(r, agent, expected, true)?;
        ensure!(!p.in_mode, "pane is in copy/other mode");
        ensure!(
            !p.synchronized,
            "synchronize-panes is enabled; input rejected"
        );
        let name = format!("pctl-{}", id());
        if let Err(error) = self.tm_input(&["load-buffer", "-b", &name, "-"], text) {
            let _ = self.tm(&["delete-buffer", "-b", &name]);
            return Err(error);
        }
        let latest = match self.guard(r, agent, expected, true) {
            Ok(p) if !p.in_mode && !p.synchronized => p,
            _ => {
                let _ = self.tm(&["delete-buffer", "-b", &name]);
                bail!("target changed while staging; nothing sent")
            }
        };
        let mut paste = vec!["paste-buffer", "-d", "-b", &name, "-t", &latest.pane];
        if bracketed {
            paste.push("-p");
        }
        match self.tm(&paste) {
            Ok(_) => self
                .audit(action, expected, "input_delivered; acceptance unverified")
                .context("input_delivered but audit failed; do not retry"),
            Err(error) => {
                let _ = self.tm(&["delete-buffer", "-b", &name]);
                bail!("outcome_unknown; do not retry automatically: {error}")
            }
        }
    }
    pub fn send_synthetic(&self, agent: &str, expected: &str, text: &str) -> Result<()> {
        ensure!(
            !text.chars().any(|c| c.is_control()),
            "synthetic send accepts one printable line only"
        );
        ensure!(text.len() <= 4096, "synthetic input exceeds 4096 bytes");
        let _lock = self.lock()?;
        let r = self.registry()?;
        let a = r
            .agents
            .iter()
            .find(|a| a.id == agent)
            .context("agent not found")?;
        ensure!(
            a.adapter == "synthetic",
            "unsupported: generic agents require direct native input"
        );
        ensure_fixture_command(&a.argv)?;
        self.deliver_text(
            &r,
            agent,
            expected,
            format!("{text}\n").as_bytes(),
            "synthetic_input",
            false,
        )
    }
    pub fn clients(&self) -> Result<Vec<String>> {
        Ok(self
            .tm(&[
                "list-clients",
                "-F",
                "#{client_name}|#{client_pid}|#{client_created}",
            ])?
            .lines()
            .map(str::to_string)
            .collect())
    }
    pub fn detach(&self, client: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        ensure!(
            self.server()? == r.server_instance,
            "server identity changed"
        );
        ensure!(
            self.clients()?.iter().any(|v| v == client),
            "client identity token not present on managed server"
        );
        let target = client.split('|').next().context("invalid client token")?;
        ensure!(
            target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c)),
            "unsupported client name"
        );
        self.tm(&["detach-client", "-t", target])?;
        Ok(())
    }
    pub fn attach(&self, project: &str) -> Result<()> {
        {
            let _lock = self.lock()?;
            let r = self.registry()?;
            ensure!(
                r.projects.iter().any(|p| p.id == project),
                "project not found"
            );
            let session = self.ensure_session(&r, project)?;
            let manager = format!("{session}:manager");
            let panes = self.tm(&[
                "list-panes",
                "-t",
                &manager,
                "-F",
                "#{pane_id}\t#{pane_dead}\t#{@pctl_agent}",
            ])?;
            let rows: Vec<_> = panes.lines().collect();
            if rows.len() == 1 {
                let fields: Vec<_> = rows[0].split('\t').collect();
                if fields.len() == 3
                    && fields[1] == "1"
                    && fields[2].is_empty()
                    && valid_native_id(fields[0], '%')
                {
                    self.tm(&[
                        "respawn-pane",
                        "-t",
                        fields[0],
                        &self.manager_command(project)?,
                    ])?;
                }
            }
            self.tm(&["select-window", "-t", &manager])?;
        }
        let session = format!("p-{}", project.replace('-', ""));
        let status = self
            .tmux_cmd()
            .args(["attach-session", "-t", &session])
            .status()?;
        ensure!(status.success(), "tmux attach failed");
        Ok(())
    }
    pub fn doctor(&self) -> serde_json::Value {
        let version = self
            .command_output(Command::new(&self.tmux).arg("-V"), None)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|e| e.to_string());
        serde_json::json!({"tmux":version,"state":self.state,"socket":self.state.join("tmux.sock"),"connection":self.server().ok(),"platform":std::env::consts::OS,"task_adapter":"generic = unknown; synthetic fixture only","security_boundary":false,"requires":"tmux >=3.0a pane options; Linux /proc process generation checks","scope":"isolated prototype; not production or RHEL certification"})
    }
    fn audit(&self, action: &str, run: &str, result: &str) -> Result<()> {
        let file = self.state.join("audit.jsonl");
        if fs::metadata(&file)
            .map(|m| m.len() > 1024 * 1024)
            .unwrap_or(false)
        {
            fs::rename(&file, self.state.join("audit.previous.jsonl"))?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(file)?;
        writeln!(
            f,
            "{}",
            serde_json::json!({"time":now(),"action":action,"run_id":run,"result":result})
        )?;
        Ok(())
    }
}
fn ensure_fixture_command(argv: &[String]) -> Result<()> {
    ensure!(
        argv.len() >= 2 && argv[1] == "worker",
        "synthetic adapter requires the bundled pctl worker command"
    );
    ensure!(
        fs::canonicalize(&argv[0]).context("synthetic fixture executable unavailable")?
            == fs::canonicalize(std::env::current_exe()?)?,
        "synthetic adapter requires this pctl executable, not an arbitrary harness"
    );
    Ok(())
}
fn native_input() -> String {
    "native".into()
}
fn ensure_terminal_input(r: &Registry, agent: &str) -> Result<()> {
    let a = r
        .agents
        .iter()
        .find(|a| a.id == agent)
        .context("agent not found")?;
    ensure!(
        a.input_mode == "terminal-line",
        "unsupported: terminal input is disabled; use the native pane or explicitly configure terminal-line while stopped"
    );
    Ok(())
}
/// macOS permissions follow the process lineage that starts the server: in a Herdr setup only a
/// Herdr pane carries them, so a launchd/ssh/front-door caller must not create one. Hosts without
/// Herdr opt out explicitly with PCTL_STANDALONE=1.
pub(crate) fn server_start_allowed() -> Result<()> {
    let on = |k| std::env::var(k).as_deref() == Ok("1");
    ensure!(
        on("HERDR_ENV") || on("PCTL_STANDALONE"),
        "server_start_outside_herdr: no PCTL server is running and this caller is not in a Herdr pane; start it from Herdr (pctl-server-herdr), or set PCTL_STANDALONE=1 on a host without Herdr"
    );
    Ok(())
}
/// Stable error categories for machine callers; human detail is retained.
pub fn error_code(message: &str) -> &'static str {
    for code in [
        "capability_denied",
        "ownership_denied",
        "launch_limit",
        "orchestration_capacity",
        "operator_only",
        "server_start_outside_herdr",
        "conversation_unknown",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    if message.contains("ambiguous_role") {
        "ambiguous_role"
    } else if message.contains("context_missing") {
        "context_missing"
    } else if message.contains("context_mismatch") {
        "context_mismatch"
    } else if message.contains("ambiguous_reply") {
        "ambiguous_reply"
    } else if message.contains("stale_sidebar") {
        "stale_sidebar"
    } else if message.contains("idempotency_conflict") {
        "idempotency_conflict"
    } else if message.contains("mailbox_full") {
        "mailbox_full"
    } else if message.contains("cross-project") {
        "cross_project"
    } else if message.contains("stale_run") {
        "stale_run"
    } else if message.contains("outcome_unknown") || message.contains("do not retry") {
        "outcome_unknown"
    } else if message.contains("unsupported") {
        "unsupported"
    } else if message.contains("copy/other mode") {
        "pane_in_mode"
    } else if message.contains("synchronize-panes") {
        "synchronized_input"
    } else if message.contains("identity")
        || message.contains("controls locked")
        || message.contains("outside registered project")
    {
        "target_unverified"
    } else if message.contains("existing run")
        || message.contains("current run")
        || message.contains("unresolved launch")
    {
        "state_conflict"
    } else if message.contains("not found") {
        "not_found"
    } else {
        "operation_failed"
    }
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(!name.trim().is_empty(), "name must not be blank");
    ensure!(
        name.len() <= 256 && !name.chars().any(char::is_control),
        "name must be at most 256 bytes with no control characters"
    );
    Ok(())
}
fn validate_argv(argv: &[String]) -> Result<()> {
    ensure!(
        !argv.is_empty() && !argv[0].trim().is_empty(),
        "argv must have an executable"
    );
    ensure!(argv.iter().all(|a| !a.contains('\0')), "argv contains NUL");
    Ok(())
}
/// Split an explicit launch command into literal argv. Quotes and backslashes
/// group text; variables, globs, pipes, redirects and substitutions never expand.
pub fn parse_command(text: &str) -> Result<Vec<String>> {
    let mut result = vec![];
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut present = false;
    for ch in text.chars() {
        if escaped {
            token.push(ch);
            escaped = false;
            present = true;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
            present = true;
        } else if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                token.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            present = true;
        } else if ch.is_whitespace() {
            if present {
                result.push(std::mem::take(&mut token));
                present = false;
            }
        } else {
            token.push(ch);
            present = true;
        }
    }
    ensure!(
        !escaped && quote.is_none(),
        "unfinished quote or backslash in command"
    );
    if present {
        result.push(token);
    }
    validate_argv(&result)?;
    Ok(result)
}
fn valid_native_id(id: &str, prefix: char) -> bool {
    id.starts_with(prefix) && id.len() > 1 && id[1..].chars().all(|c| c.is_ascii_digit())
}
fn check_identity(run: &Run, p: &Pane) -> Result<()> {
    ensure!(
        p.run == run.id && p.agent == run.agent_id && p.project == run.project_id,
        "pane metadata mismatch; controls locked"
    );
    ensure!(
        p.session == run.session,
        "pane moved outside registered project; controls locked"
    );
    ensure!(
        // tmux 3.8 reports an empty pane_pid once the pane is dead.
        p.pid == run.pid || (p.dead && p.pid.is_empty()),
        "process identity changed (external respawn); controls locked"
    );
    Ok(())
}
#[cfg(target_os = "macos")]
fn process_start(pid: &str) -> Result<String> {
    // ponytail: ps lstart has 1 s resolution; PID reuse within the same second goes unnoticed. Use proc_pidinfo if that matters.
    ensure!(pid.chars().all(|c| c.is_ascii_digit()), "invalid pid");
    let out = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", pid])
        .env("LC_ALL", "C")
        .output()?;
    let s = String::from_utf8(out.stdout)?.trim().to_string();
    ensure!(
        out.status.success() && !s.is_empty(),
        "missing process starttime"
    );
    Ok(s)
}
#[cfg(not(target_os = "macos"))]
fn process_start(pid: &str) -> Result<String> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = raw
        .rsplit_once(')')
        .context("invalid /proc stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    Ok(fields
        .get(19)
        .context("missing process starttime")?
        .to_string())
}
fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", id()));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    File::open(path.parent().context("missing parent")?)?.sync_all()?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SyntheticEvent {
    run_id: String,
    state: String,
    observed_at: u64,
}
fn read_event(p: &Path) -> Result<SyntheticEvent> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
/// Test-only harness. Its events are never interpreted as a real model adapter.
pub fn worker(label: &str, tick_ms: u64) -> Result<()> {
    let run = std::env::var("PCTL_RUN_ID").unwrap_or_else(|_| "standalone".into());
    let event = std::env::var("PCTL_EVENT_FILE").ok();
    let mut state = "waiting_input".to_string();
    let emit = |state: &str| -> Result<()> {
        if let Some(p) = &event {
            atomic_json(
                Path::new(p),
                &SyntheticEvent {
                    run_id: run.clone(),
                    state: state.into(),
                    observed_at: now(),
                },
            )?;
        }
        Ok(())
    };
    println!("PCTL SYNTHETIC {label} run={run}");
    println!("Fixture only. echo TEXT | work | approval | complete | exit N");
    emit(&state)?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(s) => {
                    if tx.send(s).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut n = 0;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(if tick_ms == 0 {
            1000
        } else {
            tick_ms
        })) {
            Ok(line) => {
                println!("RECEIVED run={run} {line}");
                if let Some(code) = line.strip_prefix("exit ") {
                    std::process::exit(code.parse().unwrap_or(1));
                }
                state = match line.as_str() {
                    "work" => "working",
                    "approval" => "waiting_approval",
                    "complete" => "completed",
                    _ => "waiting_input",
                }
                .into();
                emit(&state)?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                emit(&state)?;
                if tick_ms > 0 {
                    n += 1;
                    println!("tick={n} run={run}");
                }
            }
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tmux_protocol_is_utf8_even_without_a_locale() {
        let core = Core {
            state: PathBuf::from("/tmp/test-state"),
            tmux: PathBuf::from("/usr/bin/tmux"),
        };
        let cmd = core.tmux_cmd();
        assert_eq!(cmd.get_args().next().unwrap(), "-u");
        assert!(cmd.get_envs().any(|(k, v)| k == "TMUX" && v.is_none()));
    }
    #[test]
    fn native_ids_never_accept_implicit_targets() {
        assert!(valid_native_id("$12", '$'));
        assert!(valid_native_id("%0", '%'));
        for value in ["", "$", "$1;", "wrong", "$-1"] {
            assert!(!valid_native_id(value, '$'));
        }
    }
    #[test]
    fn quoting_roundtrip() {
        for s in [
            "a;b",
            "semi;",
            "'double\"",
            "line\nnext",
            "#{pane_id}",
            "한글 경로",
            "-x",
            "back\\slash",
        ] {
            let script = format!("printf %s {}", shell_quote(s));
            let out = Command::new("/bin/sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
        }
    }
    #[test]
    fn names_not_identity() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let a = c.register_project("same", d.path()).unwrap();
        let b = c.register_project("same", d.path()).unwrap();
        assert_ne!(a.id, b.id);
        c.rename_project(&a.id, "다른 이름").unwrap();
        assert_eq!(c.registry().unwrap().projects[0].id, a.id);
    }
    #[test]
    fn sanitizes_control_characters() {
        assert_eq!(sanitize("x\x1b[31m\ny"), "x�[31m�y");
    }
    #[test]
    fn atomic_registry_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().into(), "tmux".into()).unwrap();
        assert_eq!(c.registry().unwrap().schema_version, 1);
        assert!(c.registry().unwrap().runs.is_empty());
    }
    #[test]
    fn missing_tmux_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().into(), "/does-not-exist".into()).unwrap();
        let s = c.snapshot().unwrap();
        assert!(!s.connected);
        assert!(s.error.is_some());
    }
    #[test]
    fn command_parser_roundtrips_literal_argv_without_expansion() {
        let argv = vec![
            "/bin/echo",
            "one two",
            "",
            "'quoted'",
            "$HOME",
            "$(touch NEVER)",
            "a; b",
            "한국어",
            "back\\slash",
        ];
        let command = argv
            .iter()
            .map(|s| shell_quote(s))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(parse_command(&command).unwrap(), argv);
        assert_eq!(
            parse_command(r#"/bin/echo a\ b "c d""#).unwrap(),
            ["/bin/echo", "a b", "c d"]
        );
        for bad in ["", "   ", "''", "echo '", "echo \\"] {
            assert!(parse_command(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn legacy_profiles_default_to_native_input() {
        let a: Agent = serde_json::from_value(serde_json::json!({"id":"a","project_id":"p","name":"a","cwd":"/tmp","argv":["/bin/cat"],"adapter":"generic"})).unwrap();
        assert_eq!(a.input_mode, "native");
    }
    #[test]
    fn stopped_profile_edit_and_capability_changes_keep_identity() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let p = c.register_project("p", d.path()).unwrap();
        let a = c
            .register_agent(&p.id, "first", d.path(), vec!["/bin/cat".into()], "generic")
            .unwrap();
        assert!(
            c.paste_line(&a.id, "not-a-run", "text")
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        let b = c
            .update_agent(
                &a.id,
                "second",
                d.path(),
                vec!["/bin/echo".into(), "literal".into()],
                "generic",
            )
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.name, "second");
        c.configure_input(&a.id, "terminal-line").unwrap();
        assert_eq!(c.registry().unwrap().agents[0].input_mode, "terminal-line");
        assert!(c.configure_input(&a.id, "automatic-approval").is_err());
        std::fs::write(c.state.join(format!("pending-{}.json", a.id)), "{}").unwrap();
        assert!(
            c.configure_input(&a.id, "native")
                .unwrap_err()
                .to_string()
                .contains("unresolved launch")
        );
        assert!(
            c.update_agent(
                &a.id,
                "third",
                d.path(),
                vec!["/bin/echo".into()],
                "generic"
            )
            .is_err()
        );
    }
    #[test]
    fn blank_names_and_executables_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        for name in ["", "  ", "bad\nname", "escape\u{1b}"] {
            assert!(c.register_project(name, d.path()).is_err());
        }
        let p = c.register_project("p", d.path()).unwrap();
        assert!(
            c.register_agent(&p.id, "a", d.path(), vec!["".into()], "generic")
                .is_err()
        );
        assert!(
            c.register_agent(
                &p.id,
                "a",
                d.path(),
                vec!["/bin/echo".into(), "\u{0}".into()],
                "generic"
            )
            .is_err()
        );
    }
    #[test]
    fn terminal_line_rejects_multiline_and_control_bytes_before_tmux() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().into(), "/does-not-exist".into()).unwrap();
        for text in ["", "hello\n", "hello\r", "hello\t", "hello\u{1b}", "\u{0}"] {
            assert!(
                c.paste_line("a", "r", text)
                    .unwrap_err()
                    .to_string()
                    .contains("printable")
            );
        }
        assert!(
            c.paste_line("a", "r", &"x".repeat(4097))
                .unwrap_err()
                .to_string()
                .contains("4096")
        );
    }
    #[test]
    fn batched_snapshot_requires_exact_marker_and_valid_pane_rows() {
        let expected = "12345678-1234-4321-8765-1234567890ab";
        let row = "$1\t@2\t%3\t123\t0\t\trun\tagent\tproject\t0\t0";
        let header = format!("@pctl_instance {expected}");
        let panes = parse_snapshot_panes(&format!("{header}\n{row}"), expected).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane, "%3");
        assert_eq!(panes[0].run, "run");
        assert!(!panes[0].dead && !panes[0].in_mode && !panes[0].synchronized);
        for output in [
            format!("@pctl_instance wrong\n{row}"),
            format!("\n{row}"),
            header.clone(),
            format!("{header}\n"),
            format!("{header}\nshort\trow"),
            format!("{header}\\nforged\\trow\n{row}"),
            format!("{expected}\n{row}\n{row}"),
        ] {
            assert!(
                parse_snapshot_panes(&output, expected).is_err(),
                "{output:?}"
            );
        }
        assert!(
            parse_snapshot_panes(&format!("@pctl_instance invalid\n{row}"), "invalid").is_err()
        );
        assert_eq!(parse_panes(row).unwrap()[0].pane, "%3");
    }
    #[test]
    fn stable_error_codes_preserve_safety_meanings() {
        assert_eq!(error_code("stale_run: mismatch"), "stale_run");
        for code in [
            "capability_denied",
            "ownership_denied",
            "launch_limit",
            "orchestration_capacity",
            "operator_only",
        ] {
            assert_eq!(error_code(&format!("{code}: rejected")), code);
        }
        assert_eq!(
            error_code("input_delivered but audit failed; do not retry"),
            "outcome_unknown"
        );
        assert_eq!(
            error_code("unsupported: terminal input is disabled"),
            "unsupported"
        );
        assert_eq!(error_code("pane is in copy/other mode"), "pane_in_mode");
        assert_eq!(
            error_code("synchronize-panes is enabled"),
            "synchronized_input"
        );
        assert_eq!(error_code("process identity changed"), "target_unverified");
    }
    #[test]
    fn synthetic_label_cannot_enable_input_to_arbitrary_harness() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let p = c.register_project("p", d.path()).unwrap();
        assert!(
            c.register_agent(&p.id, "bad", d.path(), vec!["/bin/cat".into()], "synthetic")
                .unwrap_err()
                .to_string()
                .contains("bundled")
        );
        assert!(
            c.register_agent(
                &p.id,
                "bad",
                d.path(),
                vec!["/bin/echo".into(), "worker".into()],
                "synthetic"
            )
            .unwrap_err()
            .to_string()
            .contains("this pctl executable")
        );
        let a = c
            .register_agent(
                &p.id,
                "good",
                d.path(),
                vec![
                    std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    "worker".into(),
                ],
                "synthetic",
            )
            .unwrap();
        assert!(
            c.update_agent(&a.id, "bad", d.path(), vec!["/bin/cat".into()], "synthetic")
                .is_err()
        );
    }
    #[test]
    fn held_registry_lock_times_out_instead_of_hanging() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let held = c.lock().unwrap();
        let begin = Instant::now();
        assert!(c.lock().unwrap_err().to_string().contains("lock timeout"));
        assert!(begin.elapsed() < Duration::from_secs(4));
        drop(held);
        assert!(c.lock().is_ok());
    }
    #[test]
    fn hung_command_has_deadline_and_removes_private_io_files() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let begin = Instant::now();
        let error = c
            .command_output(Command::new("/bin/sh").args(["-c", "exec sleep 10"]), None)
            .unwrap_err();
        assert!(error.to_string().contains("outcome_unknown"));
        assert!(begin.elapsed() < Duration::from_secs(5));
        assert!(fs::read_dir(&c.state).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("command-")
        }));
        let out = c
            .command_output(
                Command::new("/bin/cat").arg("-"),
                Some("한글 input".as_bytes()),
            )
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "한글 input");
    }
}
