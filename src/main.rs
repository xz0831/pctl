use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pctl::Core;
use serde_json::json;
use std::io::IsTerminal;
use std::path::PathBuf;
#[derive(Parser)]
#[command(
    version,
    about = "Guarded native-tmux project-agent pilot. JSON CLI and shared Ratatui TUI."
)]
struct Args {
    /// Default: ~/.local/state/pctl (one server for every project; ssh callers get it too).
    #[arg(long, global = true, env = "PCTL_STATE")]
    state: Option<PathBuf>,
    /// Default: tmux on PATH, else /opt/homebrew/bin/tmux (non-login ssh PATH lacks it).
    #[arg(long, global = true, env = "PCTL_TMUX")]
    tmux: Option<PathBuf>,
    /// Save a state-local appearance preset; existing UIs refresh automatically.
    #[arg(long, global = true)]
    theme: Option<pctl::theme::ThemeName>,
    /// Without a command on an interactive terminal: open the pctl app.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}
#[derive(Subcommand)]
enum Cmd {
    Doctor,
    /// Explicit delegated lifecycle/layout for policy-authorized registered local profiles.
    Team {
        #[command(subcommand)]
        cmd: pctl::orchestration::Command,
    },
    /// Other computers saved by `pctl machine`; reach them with `--machine <label>`.
    Machine {
        #[command(subcommand)]
        cmd: MachineCmd,
    },
    /// Internal: serve one `Remote` client over stdin and stdout. ssh starts this, not people.
    #[command(hide = true)]
    Bridge,
    /// Inspect or save a state-local visual theme (no agent controls).
    /// Show or set the built-in theme; --new writes your own theme file to edit; --list shows all.
    Theme {
        name: Option<pctl::theme::ThemeName>,
        /// Write ~/.config/pctl/themes/NAME.toml (or under $XDG_CONFIG_HOME) to edit; the app's
        /// theme picker shows it.
        #[arg(long, value_name = "NAME", conflicts_with = "name")]
        new: Option<String>,
        /// The built-in theme a new file starts from (default portal).
        #[arg(long, requires = "new")]
        from: Option<pctl::theme::ThemeName>,
        /// Built-in themes and your own, with any problems in the files.
        #[arg(long, conflicts_with_all = ["name", "new"])]
        list: bool,
    },
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    Layout {
        #[command(subcommand)]
        cmd: LayoutCmd,
    },
    Input {
        #[command(subcommand)]
        cmd: InputCmd,
    },
    Peer {
        #[command(subcommand)]
        cmd: PeerCmd,
    },
    /// Name-addressed sessions: list, read, prompt, keys, wait, restart, restore.
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Worktrees made by `session add --worktree`: list, and remove when merged.
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCmd,
    },
    /// Short role-name mailbox workflow, bound to the launcher PCTL_* context.
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },
    Message {
        #[command(subcommand)]
        cmd: MessageCmd,
    },
    /// Tell another agent something: a session here, `<machine>:<name>` on another computer, an
    /// A2A agent from `[tell.a2a.<name>]` in config.toml, or `codex:<prefix>`. A sender line
    /// `[from <me> <host> <cwd>]` goes in front. Exit: 0 sent, 1 refused, 2 usage, 3 no token,
    /// 4 no connection, 5 could not send, 6 the session waits for a human (nothing typed).
    Tell {
        to: String,
        text: String,
        /// Wait for the answer: a session's last 40 lines once it stops working, an A2A reply.
        #[arg(long)]
        wait: bool,
        /// A2A conversation key; the default is one per sender per day.
        #[arg(long)]
        context: Option<String>,
        /// One human line instead of JSON.
        #[arg(long)]
        plain: bool,
        /// The sender line of a computer that ran this over ssh. Refused unless this is an ssh login.
        #[arg(long, hide = true)]
        from_line: Option<String>,
    },
    /// Read a Markdown file or folder in the terminal: search, copy, and ask the project's main
    /// agent about a passage. Needs an interactive terminal.
    View {
        path: PathBuf,
        /// Project whose main receives questions; defaults to the document path.
        #[arg(long)]
        project: Option<String>,
        /// portal or garage.
        #[arg(long, value_parser = ["portal", "garage"])]
        look: Option<String>,
        #[arg(long)]
        dark: bool,
        /// Set by the app for a document on another computer: no clipboard command runs here.
        #[arg(long, hide = true)]
        remote: bool,
    },
    /// Show the person a file or folder: it appears in the app's side bar as `new`, with one
    /// notice; nothing opens by itself. `--answer` is how an agent answers a question asked in
    /// `pctl view`. `--list` prints the documents as JSON.
    Show {
        path: Option<PathBuf>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        note: Option<String>,
        /// A project name or ID. Default: the project of the session that runs this.
        #[arg(long)]
        project: Option<String>,
        /// Answer question REQ_ID; TEXT `-` reads standard input.
        #[arg(long, num_args = 2, value_names = ["REQ_ID", "TEXT"], conflicts_with_all = ["path", "list", "read"])]
        answer: Option<Vec<String>>,
        #[arg(long, conflicts_with_all = ["path", "read"])]
        list: bool,
        /// Mark a document read (the app does this when the person opens it).
        #[arg(long, value_name = "ID", conflicts_with = "path")]
        read: Option<String>,
        /// Ask the person's app to open it: beside this session's cell (without moving focus), or
        /// as a tab of the project. Pictures are recorded but not opened.
        #[arg(long, value_parser = pctl::docs::OPEN_WAYS, conflicts_with_all = ["answer", "list", "read"])]
        open: Option<String>,
    },
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    Client {
        #[command(subcommand)]
        cmd: ClientCmd,
    },
    /// Attach a raw tmux client: to a project's agent window (name or ID), or without
    /// --project to the most recently used project. `pctl` alone opens the app instead.
    Attach {
        #[arg(long)]
        project: Option<String>,
    },
    /// Phone page: list sessions, read one screen, type a line. Off unless started; loopback or
    /// Tailscale address only; needs the token from a 0600 file.
    Web {
        /// IP:PORT on loopback or a Tailscale address (100.64.0.0/10, fd7a:115c:a1e0::/48).
        #[arg(long)]
        listen: String,
        /// File holding the access token (mode 0600, at least 32 characters).
        #[arg(long)]
        token_file: PathBuf,
    },
    /// Create two synthetic projects with two native agent panes each.
    Demo,
    /// Synthetic fixture harness only; no model is called.
    Worker {
        #[arg(long, default_value = "fixture")]
        label: String,
        #[arg(long, default_value_t = 0)]
        tick_ms: u64,
    },
}
#[derive(Subcommand)]
enum MachineCmd {
    /// Save this computer after its bridge answers the hello.
    Add {
        label: String,
        #[arg(value_name = "ssh-target")]
        ssh_target: String,
    },
    /// Every saved machine, in file order.
    List,
    /// Reachable or not, host name, pctl version, pctl server and agent count.
    Status,
    Remove {
        label: String,
    },
    Rename {
        old: String,
        new: String,
    },
    Enable {
        label: String,
    },
    Disable {
        label: String,
    },
}
#[derive(Subcommand)]
enum ProjectCmd {
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        root: PathBuf,
    },
    List,
    Rename {
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: String,
    },
}
#[derive(Subcommand)]
enum AgentCmd {
    Add {
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long, default_value = "generic")]
        adapter: String,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Edit a stopped profile. Existing runs must first be explicitly terminated.
    Edit {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long, default_value = "generic")]
        adapter: String,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Opt in to literal single-line PTY paste + separate Enter while stopped.
    ConfigureInput {
        #[arg(long)]
        agent: String,
        #[arg(long, value_parser = ["native", "terminal-line"])]
        mode: String,
    },
    /// Opt in to explicit local mailbox coordination while stopped.
    ConfigureCollaboration {
        #[arg(long)]
        agent: String,
        #[arg(long, value_parser = ["disabled", "mailbox"])]
        mode: String,
    },
    /// Resolve the caller-claimed PCTL_* environment for a newly launched run.
    Identity,
    /// Publish a bounded self-report, kept separate from observed task state.
    Report {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long, value_parser = ["working", "waiting", "blocked", "completed", "unknown"])]
        status: String,
        #[arg(long)]
        summary: String,
        #[arg(long, default_value_t = 60)]
        ttl_seconds: u64,
    },
    /// Send the literal Ctrl-C key. This does not guarantee SIGINT or cancellation.
    Interrupt {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    List,
    Inspect {
        #[arg(long)]
        agent: String,
    },
    Start {
        #[arg(long)]
        agent: String,
    },
    Focus {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    Terminate {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    Restart {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
}
#[derive(Subcommand)]
enum LayoutCmd {
    Pair {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        with_agent: String,
        #[arg(long)]
        with_run: String,
    },
    Split {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    Zoom {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
}
#[derive(Subcommand)]
enum InputCmd {
    Draft {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        from_file: PathBuf,
    },
    /// Read this run's local draft without transmitting it.
    ReadDraft {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    /// Explicit transmission of one printable line, with no Enter. Requires opt-in.
    /// A harness may act immediately; this is not a safe local staging action.
    Paste {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        text: String,
    },
    /// Explicit Enter keystroke. Requires terminal-line opt-in; no approval guessing.
    Submit {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
    /// Test harness only. Sends a printable single line and newline. No retries.
    SendFixture {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        text: String,
    },
}
#[derive(Subcommand)]
enum PeerCmd {
    /// Read same-project registered peers and exact run IDs; no screen scraping.
    List {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
    },
}
#[derive(Subcommand)]
enum MessageCmd {
    /// Store an explicit peer message. Never writes to a native terminal.
    Send {
        #[arg(long)]
        from_agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        to_agent: String,
        #[arg(long)]
        to_run: String,
        #[arg(long, value_parser = ["handoff", "result", "update", "question"])]
        kind: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body_file: PathBuf,
        /// Caller-generated UUID, reused only for an identical request.
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        in_reply_to: Option<String>,
    },
    /// Read without marking acknowledged; optional bounded wait never holds a lock.
    Inbox {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        unacked: bool,
        #[arg(long, default_value_t = 0)]
        after_sequence: u64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        wait_seconds: u64,
    },
    /// Acknowledge the unique existing direct reply to our own outgoing ID.
    /// Zero/multiple matches never select or acknowledge another message.
    AckReply {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        reply_to: String,
    },
    /// Acknowledge receipt only; not acceptance, authorization, or task completion.
    Ack {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long)]
        message: String,
    },
}
#[derive(Subcommand)]
enum WorktreeCmd {
    /// Session, status, path, branch, base, dirty files, commits ahead, merged, locked.
    List,
    /// Move a stopped session's clean, merged worktree to ~/.Trash, prune it, delete the merged
    /// branch and unregister the session. Refused otherwise; there is no --force.
    Remove { name: String },
}
#[derive(Subcommand)]
enum SessionCmd {
    /// Swap an idle, stale Claude session to its installed build, preserving its conversation.
    Refresh {
        #[arg(required_unless_present = "idle", conflicts_with = "idle")]
        name: Option<String>,
        #[arg(long, requires = "dry_run")]
        idle: bool,
        #[arg(long, requires = "idle")]
        dry_run: bool,
    },
    /// Hold this session out of refresh until --off.
    Protect {
        name: String,
        #[arg(long)]
        off: bool,
    },
    /// Explain the screen status, matching rule and considered rows (1-based screen rows).
    Explain {
        name: String,
    },
    /// Every registered session with process, screen status and conversation ID.
    List,
    Get {
        name: String,
    },
    /// Register in the cwd's project, opt in to terminal input and mailbox, and start.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: PathBuf,
        /// Project name; default is the cwd's folder name.
        #[arg(long)]
        project: Option<String>,
        /// Resume this existing conversation (moving a session from another tool) instead of a new one.
        #[arg(long)]
        conversation: Option<String>,
        /// Create this new branch in a new worktree beside the cwd's repo (<repo>-wt-<name>) and
        /// run there. Refused for an existing branch or folder, or a dirty base checkout.
        #[arg(long, value_name = "BRANCH")]
        worktree: Option<String>,
        /// Start point of the branch and where it should merge back (default: the cwd's branch).
        #[arg(long, requires = "worktree")]
        base: Option<String>,
        /// OpenCode model (provider/model) for this session only: a private OpenCode server
        /// started with OPENCODE_CONFIG_CONTENT. No file is written.
        #[arg(long)]
        model: Option<String>,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Paste text and press Enter. Refused while the session waits for a human decision.
    Prompt {
        name: String,
        text: String,
    },
    /// Press named keys (Escape, Enter, Tab, BTab, arrows, BSpace, Space, 0-9, y, n) in order.
    /// Answers a blocked session too (`was_blocked` in the output); pasted text never does.
    Keys {
        name: String,
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Wait until the status is one of --until (default: anything but working), or until
    /// --match TEXT shows on the screen.
    Wait {
        name: String,
        #[arg(long, conflicts_with = "matched")]
        until: Vec<String>,
        /// Wait until this text appears on the visible screen or 200 lines of scrollback.
        #[arg(long = "match", value_name = "TEXT")]
        matched: Option<String>,
        #[arg(long, default_value_t = 1_800_000)]
        timeout_ms: u64,
    },
    /// Print the last lines of the session's screen.
    Read {
        name: String,
        #[arg(long, default_value_t = 40)]
        lines: usize,
    },
    /// Restart in the same conversation. Refused when blocked; when working unless --force;
    /// when the conversation ID is unknown unless --new.
    Restart {
        name: String,
        #[arg(long)]
        force: bool,
        /// Start a fresh conversation instead of resuming.
        #[arg(long)]
        new: bool,
    },
    /// Start again, in the same conversation, every session whose pane was lost (reboot).
    /// Sessions with an unknown conversation ID are skipped unless --new.
    Restore {
        #[arg(long)]
        new: bool,
    },
    /// Name of the caller's own session (from PCTL_AGENT_ID).
    Whoami,
    /// Start a stopped session in its saved conversation. Unknown conversation needs --new.
    Start {
        name: String,
        #[arg(long)]
        new: bool,
    },
    /// Unregister a session that has no run (refused while it has one).
    Remove {
        name: String,
    },
    /// SessionStart hook: read the hook JSON on stdin and record the conversation ID.
    Hook {
        /// Record Claude readiness/activity events; never changes real hook settings.
        #[arg(long)]
        refresh_observe: bool,
    },
}
#[derive(Subcommand)]
enum MsgCmd {
    /// List exact same-project role names. Does not grant permission to send.
    Peers,
    /// Store an authorized handoff to one exact unique role. Keep the key for retries.
    Send {
        role: String,
        #[arg(long)]
        key: String,
        #[arg(long, value_parser = ["handoff", "result", "update", "question"], default_value = "handoff")]
        kind: String,
        #[arg(long, default_value = "handoff")]
        subject: String,
        #[arg(
            long,
            required_unless_present = "body_file",
            conflicts_with = "body_file"
        )]
        text: Option<String>,
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Reply to an actual received short handle; resolves the exact reverse run pair.
    Reply {
        handle: String,
        #[arg(long)]
        key: String,
        #[arg(long, value_parser = ["result", "update", "question"], default_value = "result")]
        kind: String,
        #[arg(long, default_value = "reply")]
        subject: String,
        #[arg(
            long,
            required_unless_present = "body_file",
            conflicts_with = "body_file"
        )]
        text: Option<String>,
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Read-only inbox snapshot. Does not record retrieval or acknowledgment.
    Inbox {
        #[arg(long)]
        unacked: bool,
        #[arg(long, default_value_t = 0)]
        after_sequence: u64,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        wait_seconds: u64,
    },
    /// Explicitly record a consume request and return the message; not comprehension.
    Consume { handle: String },
    /// Record receipt only, separately from consume, permission or completion.
    Ack { handle: String },
    /// Record immutable recipient self-reported handoff completion; never verified success.
    Complete {
        handle: String,
        #[arg(long)]
        summary: String,
        /// Inert relative reference only. No file is opened, read or executed.
        #[arg(long)]
        artifact: Option<String>,
    },
    /// Read stored/retrieval/ack/completion evidence for one sent or received handle.
    Status { handle: String },
}
fn msg_body(text: Option<String>, file: Option<PathBuf>) -> Result<String> {
    if let Some(file) = file {
        anyhow::ensure!(
            std::fs::metadata(&file)?.len() <= 8192,
            "message body file exceeds 8192 bytes"
        );
        Ok(std::fs::read_to_string(file)?)
    } else {
        text.context("message text or body file required")
    }
}
#[derive(Subcommand)]
enum SkillCmd {
    /// Install into this project's native OpenCode discovery path; never global.
    Install {
        #[arg(long)]
        project: String,
    },
}
#[derive(Subcommand)]
enum ClientCmd {
    List,
    Detach {
        #[arg(long)]
        client: String,
    },
}
fn print<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
/// `pctl --host <ssh-target> ARGS...` runs `PCTL_REMOTE_BIN ARGS...` (default `pctl`) on that
/// computer over the user's existing ssh setup. Paths in ARGS are the remote host's paths.
/// `pctl --machine <label> ARGS...` is the same with the ssh target saved by `pctl machine add`.
/// With a terminal on both ends ssh gets `-t`, so `pctl --machine rick` alone opens rick's app here.
fn remote(argv: &[String]) -> Option<i32> {
    let (at, saved) = match argv.iter().position(|a| a == "--host") {
        Some(i) => (i, false),
        None => (argv.iter().position(|a| a == "--machine")?, true),
    };
    let target = if saved {
        match argv.get(at + 1).map(|label| pctl::machine::resolve(label)) {
            Some(Ok(target)) => target,
            Some(Err(e)) => return Some(fail(&format!("{e:#}"))),
            None => return Some(fail("--machine needs a saved machine label")),
        }
    } else {
        match argv.get(at + 1).filter(|h| !h.starts_with('-')) {
            Some(host) => host.clone(),
            None => return Some(fail("--host needs an ssh target")),
        }
    };
    let bin = std::env::var("PCTL_REMOTE_BIN").unwrap_or_else(|_| "pctl".into());
    let rest = argv[1..at].iter().chain(&argv[at + 2..]);
    let line = std::iter::once(&bin)
        .chain(rest)
        .map(|a| pctl::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let mut ssh = std::process::Command::new("ssh");
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        ssh.arg("-t");
    }
    let status = ssh
        .args(["-o", "BatchMode=yes", "--", &target, &line])
        .status();
    Some(match status {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => fail(&format!("ssh: {e}")),
    })
}
fn fail(error: &str) -> i32 {
    eprintln!(
        "{}",
        json!({"schema_version":1,"ok":false,"error":error,"error_code":pctl::error_code(error)})
    );
    2
}
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if let Some(code) = remote(&argv) {
        std::process::exit(code)
    }
    // Claude Code reads hook exit 2 as "block" (tool denied, prompt erased); a failing observer exits 1.
    let hook = argv.windows(2).any(|w| w == ["session", "hook"]);
    if hook && let Err(e) = Args::try_parse() {
        let _ = e.print();
        std::process::exit(1)
    }
    if let Err(e) = run() {
        eprintln!(
            "{}",
            json!({"schema_version":1,"ok":false,"error":format!("{e:#}"),"error_code":pctl::error_code(&format!("{e:#}"))})
        );
        std::process::exit(if hook { 1 } else { 2 })
    }
}
fn run() -> Result<()> {
    let mut args = Args::parse();
    let state = args.state.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/pctl")
    });
    let tmux = args.tmux.clone().unwrap_or_else(|| {
        let brew = PathBuf::from("/opt/homebrew/bin/tmux");
        let on_path = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("tmux").is_file()));
        if !on_path && brew.is_file() {
            brew
        } else {
            PathBuf::from("tmux")
        }
    });
    let Some(cmd) = args.cmd.take() else {
        // A person typed `pctl` alone: open the app. Agents and scripts have no
        // terminal and keep getting the usage error.
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            <Args as clap::CommandFactory>::command()
                .error(
                    clap::error::ErrorKind::MissingSubcommand,
                    "a command is required; `pctl` alone opens the workspace only on an interactive terminal",
                )
                .exit();
        }
        let core = Core::new(state, tmux)?;
        if let Some(theme) = args.theme {
            core.set_theme(theme)?;
        }
        if let Err(e) = pctl::app::run(&core) {
            eprintln!("pctl: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    };
    if let Cmd::Worker { label, tick_ms } = &cmd {
        return pctl::worker(label, *tick_ms);
    }
    if let Cmd::Team { cmd } = &cmd {
        pctl::orchestration::preflight(cmd)?;
    }
    let bound = matches!(&cmd, Cmd::Msg { .. })
        || match &cmd {
            Cmd::Team { cmd } => pctl::orchestration::requires_context(cmd),
            _ => false,
        };
    let core = if bound {
        anyhow::ensure!(
            args.theme.is_none(),
            "unsupported: --theme is not permitted on context-bound workflow commands; use the explicit operator theme command"
        );
        pctl::collaboration::workflow::open_bound_core(state, tmux)?
    } else {
        Core::new(state, tmux)?
    };
    if let Some(theme) = args.theme {
        core.set_theme(theme)?;
    }
    match cmd {
        Cmd::Theme {
            name,
            new,
            from,
            list,
        } => {
            use pctl::theme::{ThemeName, custom};
            if let Some(n) = new {
                let path = custom::create(&n, from.unwrap_or_default())?;
                print(
                    &json!({"created": path, "next": "edit the colours, then pick it from ◐ Theme in pctl"}),
                )?;
            } else if list {
                let (mine, warnings) = custom::load_all();
                print(&json!({
                    "built_in": ThemeName::ALL.iter().map(|t| json!({"name": t.label().to_lowercase(), "about": t.about()})).collect::<Vec<_>>(),
                    "yours": mine.iter().map(|c| json!({"name": c.name, "about": c.about})).collect::<Vec<_>>(),
                    "folder": custom::dir(),
                    "warnings": warnings,
                }))?;
            } else {
                if let Some(theme) = name {
                    core.set_theme(theme)?;
                }
                print(
                    &json!({"theme": core.theme()?, "config": core.state.join("appearance.json")}),
                )?;
            }
        }
        Cmd::Doctor => print(&core.doctor())?,
        Cmd::Team { cmd } => print(&pctl::orchestration::run(&core, cmd)?)?,
        Cmd::Machine { cmd } => print(&match cmd {
            MachineCmd::Add { label, ssh_target } => pctl::machine::add(&label, &ssh_target)?,
            MachineCmd::List => pctl::machine::list()?,
            MachineCmd::Status => pctl::machine::status()?,
            MachineCmd::Remove { label } => pctl::machine::remove(&label)?,
            MachineCmd::Rename { old, new } => pctl::machine::rename(&old, &new)?,
            MachineCmd::Enable { label } => pctl::machine::set_enabled(&label, true)?,
            MachineCmd::Disable { label } => pctl::machine::set_enabled(&label, false)?,
        })?,
        Cmd::Bridge => pctl::bridge::run(&core)?,
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Add { name, root } => print(&core.register_project(&name, &root)?)?,
            ProjectCmd::List => print(&core.registry()?.projects)?,
            ProjectCmd::Rename { project, name } => {
                core.rename_project(&project, &name)?;
                print(&json!({"ok":true}))?
            }
        },
        Cmd::Agent { cmd } => match cmd {
            AgentCmd::Add {
                project,
                name,
                cwd,
                adapter,
                argv,
            } => print(&core.register_agent(&project, &name, &cwd, argv, &adapter)?)?,
            AgentCmd::Edit {
                agent,
                name,
                cwd,
                adapter,
                argv,
            } => {
                let (edited, cleared) = core.update_agent(&agent, &name, &cwd, argv, &adapter)?;
                let mut out = serde_json::to_value(&edited)?;
                out["conversation"] = json!(edited.conversation);
                out["conversation_cleared"] = json!(cleared);
                print(&out)?;
            }
            AgentCmd::ConfigureInput { agent, mode } => {
                core.configure_input(&agent, &mode)?;
                print(&json!({"ok":true,"input_mode":mode}))?;
            }
            AgentCmd::ConfigureCollaboration { agent, mode } => {
                core.configure_collaboration(&agent, &mode)?;
                print(
                    &json!({"ok":true,"collaboration_mode":mode,"scope":"local mailbox only; capability is not action permission"}),
                )?;
            }
            AgentCmd::Identity => print(&core.identity()?)?,
            AgentCmd::Report {
                agent,
                expected_run,
                status,
                summary,
                ttl_seconds,
            } => print(&core.report_status(
                &agent,
                &expected_run,
                &status,
                &summary,
                ttl_seconds,
            )?)?,
            AgentCmd::Interrupt {
                agent,
                expected_run,
            } => {
                core.send_ctrl_c(&agent, &expected_run)?;
                print(
                    &json!({"ok":true,"result":"ctrl_c_key_delivered; interruption unverified"}),
                )?;
            }
            AgentCmd::List => print(&core.snapshot()?)?,
            AgentCmd::Inspect { agent } => print(
                &core
                    .snapshot()?
                    .agents
                    .into_iter()
                    .find(|a| a.agent.id == agent)
                    .context("agent not found")?,
            )?,
            AgentCmd::Start { agent } => print(&core.start(&agent)?)?,
            AgentCmd::Focus {
                agent,
                expected_run,
            } => {
                core.focus(&agent, &expected_run)?;
                print(&json!({"ok":true}))?
            }
            AgentCmd::Terminate {
                agent,
                expected_run,
            } => {
                core.terminate(&agent, &expected_run)?;
                print(
                    &json!({"ok":true,"result":"pane_removed; external descendants not guaranteed"}),
                )?
            }
            AgentCmd::Restart {
                agent,
                expected_run,
            } => print(&core.restart(&agent, &expected_run)?)?,
        },
        Cmd::Layout { cmd } => {
            match cmd {
                LayoutCmd::Pair {
                    agent,
                    expected_run,
                    with_agent,
                    with_run,
                } => core.pair(&agent, &expected_run, &with_agent, &with_run)?,
                LayoutCmd::Split {
                    agent,
                    expected_run,
                } => core.split(&agent, &expected_run)?,
                LayoutCmd::Zoom {
                    agent,
                    expected_run,
                } => core.zoom(&agent, &expected_run)?,
            };
            print(&json!({"ok":true}))?
        }
        Cmd::Input { cmd } => match cmd {
            InputCmd::Draft {
                agent,
                expected_run,
                from_file,
            } => {
                core.save_draft(&agent, &expected_run, &std::fs::read_to_string(from_file)?)?;
                print(&json!({"ok":true,"result":"stored locally; nothing sent"}))?
            }
            InputCmd::ReadDraft {
                agent,
                expected_run,
            } => print(&json!({"ok":true,"text":core.load_draft(&agent, &expected_run)?}))?,
            InputCmd::Paste {
                agent,
                expected_run,
                text,
            } => {
                core.paste_line(&agent, &expected_run, &text)?;
                print(
                    &json!({"ok":true,"result":"text_delivered; no_enter; acceptance unverified"}),
                )?;
            }
            InputCmd::Submit {
                agent,
                expected_run,
            } => {
                core.submit_line(&agent, &expected_run)?;
                print(&json!({"ok":true,"result":"enter_key_delivered; acceptance unverified"}))?;
            }
            InputCmd::SendFixture {
                agent,
                expected_run,
                text,
            } => {
                core.send_synthetic(&agent, &expected_run, &text)?;
                print(&json!({"ok":true,"result":"input_delivered; acceptance unverified"}))?
            }
        },
        Cmd::Peer { cmd } => match cmd {
            PeerCmd::List {
                agent,
                expected_run,
            } => print(&core.peers(&agent, &expected_run)?)?,
        },
        Cmd::Session { cmd } => match cmd {
            SessionCmd::Refresh {
                name,
                idle,
                dry_run: _,
            } => {
                if idle {
                    print(&core.session_refresh_idle_dry_run()?)?;
                } else {
                    print(
                        &core.session_refresh(name.as_deref().context("session name required")?)?,
                    )?;
                }
            }
            SessionCmd::Protect { name, off } => print(&core.session_protect(&name, off)?)?,
            SessionCmd::List => print(&core.session_list()?)?,
            SessionCmd::Explain { name } => print(&core.session_explain(&name)?)?,
            SessionCmd::Get { name } => {
                let mut v = core.session_get(&name)?;
                pctl::tellq::after_read(&core, &mut v);
                print(&v)?
            }
            SessionCmd::Add {
                name,
                cwd,
                project,
                conversation,
                worktree,
                base,
                model,
                argv,
            } => print(&core.session_add(
                &name,
                &cwd,
                project.as_deref(),
                argv,
                conversation.as_deref(),
                worktree.as_deref().map(|b| (b, base.as_deref())),
                model.as_deref(),
            )?)?,
            SessionCmd::Prompt { name, text } => print(&core.session_prompt(&name, &text)?)?,
            SessionCmd::Keys { name, keys } => print(&core.session_keys(&name, &keys)?)?,
            SessionCmd::Wait {
                name,
                until,
                matched,
                timeout_ms,
            } => {
                let mut v = core.session_wait(
                    &name,
                    &until,
                    std::time::Duration::from_millis(timeout_ms),
                    matched.as_deref(),
                )?;
                pctl::tellq::after_read(&core, &mut v);
                print(&v)?
            }
            SessionCmd::Read { name, lines } => println!("{}", core.session_read(&name, lines)?),
            SessionCmd::Restart { name, force, new } => {
                print(&core.session_restart(&name, force, new)?)?
            }
            SessionCmd::Restore { new } => print(&core.session_restore(new)?)?,
            SessionCmd::Whoami => print(&core.session_whoami()?)?,
            SessionCmd::Start { name, new } => print(&core.session_start(&name, new)?)?,
            SessionCmd::Remove { name } => print(&core.session_remove(&name)?)?,
            SessionCmd::Hook { refresh_observe } => {
                // Legacy/out-of-context hooks stay silent. The opt-in handler never
                // treats missing evidence as ready; metadata I/O errors remain visible.
                let mut input = String::new();
                let _ = std::io::Read::read_to_string(
                    &mut std::io::Read::take(std::io::stdin(), 1 << 20),
                    &mut input,
                );
                if refresh_observe {
                    core.session_refresh_hook(&input)?;
                } else {
                    let _ = core.session_hook(&input);
                }
            }
        },
        Cmd::Msg { cmd } => {
            let caller = core.msg_caller()?;
            match cmd {
                MsgCmd::Peers => print(&core.msg_peers(&caller)?)?,
                MsgCmd::Send {
                    role,
                    key,
                    kind,
                    subject,
                    text,
                    body_file,
                } => print(&core.msg_send(
                    &caller,
                    &pctl::collaboration::workflow::RoleSend {
                        role: Some(role),
                        reply: None,
                        key,
                        kind,
                        subject,
                        body: msg_body(text, body_file)?,
                    },
                )?)?,
                MsgCmd::Reply {
                    handle,
                    key,
                    kind,
                    subject,
                    text,
                    body_file,
                } => print(&core.msg_send(
                    &caller,
                    &pctl::collaboration::workflow::RoleSend {
                        role: None,
                        reply: Some(handle),
                        key,
                        kind,
                        subject,
                        body: msg_body(text, body_file)?,
                    },
                )?)?,
                MsgCmd::Inbox {
                    unacked,
                    after_sequence,
                    limit,
                    wait_seconds,
                } => {
                    anyhow::ensure!(
                        wait_seconds <= 120,
                        "inbox wait must be at most 120 seconds"
                    );
                    let started = std::time::Instant::now();
                    let deadline = started + std::time::Duration::from_secs(wait_seconds);
                    loop {
                        let mut inbox = core.msg_inbox(&caller, unacked, after_sequence, limit)?;
                        let empty = inbox["messages"].as_array().is_some_and(Vec::is_empty);
                        if !empty || std::time::Instant::now() >= deadline {
                            inbox["waited_ms"] = json!(started.elapsed().as_millis());
                            inbox["timed_out"] = json!(wait_seconds > 0 && empty);
                            print(&inbox)?;
                            break;
                        }
                        std::thread::sleep(
                            std::time::Duration::from_millis(250)
                                .min(deadline.saturating_duration_since(std::time::Instant::now())),
                        );
                    }
                }
                MsgCmd::Consume { handle } => {
                    print(&core.msg_record(&caller, &handle, "consume", None, None)?)?
                }
                MsgCmd::Ack { handle } => {
                    print(&core.msg_record(&caller, &handle, "ack", None, None)?)?
                }
                MsgCmd::Complete {
                    handle,
                    summary,
                    artifact,
                } => print(&core.msg_record(
                    &caller,
                    &handle,
                    "complete",
                    Some(&summary),
                    artifact.as_deref(),
                )?)?,
                MsgCmd::Status { handle } => print(&core.msg_status(&caller, &handle)?)?,
            }
        }
        Cmd::Message { cmd } => match cmd {
            MessageCmd::Send {
                from_agent,
                expected_run,
                to_agent,
                to_run,
                kind,
                subject,
                body_file,
                request_id,
                in_reply_to,
            } => {
                anyhow::ensure!(
                    std::fs::metadata(&body_file)?.len() <= 8192,
                    "message body file exceeds 8192 bytes"
                );
                print(&core.send_message(&pctl::collaboration::SendRequest {
                    request_id,
                    from_agent,
                    from_run: expected_run,
                    to_agent,
                    to_run,
                    kind,
                    subject,
                    body: std::fs::read_to_string(body_file)?,
                    in_reply_to,
                })?)?;
            }
            MessageCmd::Inbox {
                agent,
                expected_run,
                unacked,
                after_sequence,
                limit,
                wait_seconds,
            } => {
                anyhow::ensure!(
                    wait_seconds <= 120,
                    "inbox wait must be at most 120 seconds"
                );
                let started = std::time::Instant::now();
                let deadline = started + std::time::Duration::from_secs(wait_seconds);
                loop {
                    let inbox =
                        core.inbox(&agent, &expected_run, unacked, after_sequence, limit)?;
                    if !inbox.messages.is_empty() || std::time::Instant::now() >= deadline {
                        let timed_out = wait_seconds > 0 && inbox.messages.is_empty();
                        let mut result = serde_json::to_value(inbox)?;
                        result["waited_ms"] = json!(started.elapsed().as_millis());
                        result["timed_out"] = json!(timed_out);
                        print(&result)?;
                        break;
                    }
                    std::thread::sleep(
                        std::time::Duration::from_millis(250)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                }
            }
            MessageCmd::AckReply {
                agent,
                expected_run,
                reply_to,
            } => print(
                &json!({"message":core.ack_reply(&agent,&expected_run,&reply_to)?,"resolved_from_outgoing_id":reply_to,"semantics":"receipt only; unique existing direct reply; not permission, task acceptance, or completion"}),
            )?,
            MessageCmd::Ack {
                agent,
                expected_run,
                message,
            } => print(
                &json!({"message":core.ack_message(&agent, &expected_run, &message)?,"semantics":"receipt only; not permission, task acceptance, or completion"}),
            )?,
        },
        Cmd::Tell {
            to,
            text,
            wait,
            context,
            plain,
            from_line,
        } => {
            anyhow::ensure!(
                !to.trim().is_empty() && !text.trim().is_empty(),
                "usage: pctl tell <to> <text> [--wait] [--context KEY] [--plain]"
            );
            if let Some(line) = &from_line {
                anyhow::ensure!(
                    std::env::var_os("SSH_CONNECTION").is_some(),
                    "--from-line is only accepted over ssh"
                );
                anyhow::ensure!(
                    pctl::tell::valid_line(line),
                    "--from-line is not a sender line"
                );
            }
            let out = pctl::tell::run(
                &core,
                &pctl::tell::Request {
                    to,
                    text,
                    wait,
                    context,
                    from_line,
                },
            );
            match (plain, out.result.exit_code()) {
                (false, _) => println!("{}", serde_json::to_string_pretty(&out.json())?),
                (true, 0) => println!("{}", out.plain()),
                (true, _) => eprintln!("{}", out.plain()),
            }
            if out.result.exit_code() != 0 {
                std::process::exit(out.result.exit_code());
            }
        }
        Cmd::View {
            path,
            project,
            look,
            dark,
            remote,
        } => pctl::view::run(
            &core,
            &path,
            mdv::view::ViewOptions {
                project,
                look: look.map(|l| match l.as_str() {
                    "garage" => mdv::Look::Garage,
                    _ => mdv::Look::Portal,
                }),
                dark,
            },
            remote,
        )?,
        Cmd::Show {
            path,
            title,
            note,
            project,
            answer,
            list,
            read,
            open,
        } => {
            if let Some(a) = answer {
                let mut text = a[1].clone();
                if text == "-" {
                    use std::io::Read;
                    text.clear();
                    std::io::stdin().take(16001).read_to_string(&mut text)?;
                }
                mdv::threads::answer(&a[0], &text).map_err(anyhow::Error::msg)?;
                print(&json!({"ok": true, "answered": a[0]}))?
            } else if list {
                print(&core.doc_list(project.as_deref()))?
            } else if let Some(id) = read {
                anyhow::ensure!(
                    !pctl::app::agent_caller(&core),
                    "agent_caller: only the person opening a document marks it read"
                );
                print(&core.doc_read(&id)?)?
            } else {
                let path = path.context(
                    "usage: pctl show <file|folder> | --list | --answer <req-id> <text>",
                )?;
                let doc = core.doc_show(
                    &path,
                    title.as_deref(),
                    note.as_deref(),
                    project.as_deref(),
                    open.as_deref(),
                )?;
                let mut out = serde_json::to_value(&doc)?;
                if open.is_some() && doc.open.is_none() {
                    out["open_refused"] = json!("pictures open in the page (later)");
                }
                print(&out)?
            }
        }
        Cmd::Skill { cmd } => match cmd {
            SkillCmd::Install { project } => print(&core.install_skill(&project)?)?,
        },
        Cmd::Client { cmd } => match cmd {
            ClientCmd::List => print(&core.clients()?)?,
            ClientCmd::Detach { client } => {
                core.detach(&client)?;
                print(&json!({"ok":true,"result":"client detached; agents retained"}))?
            }
        },
        Cmd::Attach { project } => match project {
            Some(p) => core.attach(&core.resolve_project(&p)?)?,
            None => core.open_default()?,
        },
        Cmd::Worktree { cmd } => match cmd {
            WorktreeCmd::List => print(&core.worktree_list()?)?,
            WorktreeCmd::Remove { name } => print(&core.worktree_remove(&name)?)?,
        },
        Cmd::Web { listen, token_file } => pctl::web::serve(&core, &listen, &token_file)?,
        Cmd::Demo => {
            anyhow::ensure!(
                core.registry()?.projects.is_empty(),
                "demo requires a fresh --state directory"
            );
            let exe = std::env::current_exe()?.to_string_lossy().into_owned();
            let mut projects = vec![];
            let mut runs = vec![];
            for (name, labels) in [
                ("Alpha · 구현", ["implementer", "reviewer"]),
                ("Beta · 실험", ["researcher", "tester"]),
            ] {
                let root = core.state.join(format!("workspace-{}", projects.len() + 1));
                std::fs::create_dir_all(&root)?;
                let p = core.register_project(name, &root)?;
                for label in labels {
                    let a = core.register_agent(
                        &p.id,
                        label,
                        &root,
                        vec![exe.clone(), "worker".into(), "--label".into(), label.into()],
                        "synthetic",
                    )?;
                    runs.push(core.start(&a.id)?);
                }
                projects.push(p);
            }
            print(
                &json!({"projects":projects,"runs":runs,"next":"pctl --state <this-state> attach --project <project-id>","scope":"four synthetic workers; no actual model calls"}),
            )?;
        }
        Cmd::Worker { .. } => unreachable!(),
    }
    Ok(())
}
