use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pctl::Core;
use serde_json::json;
use std::path::PathBuf;
#[derive(Parser)]
#[command(
    version,
    about = "Guarded native-tmux project-agent pilot. JSON CLI and shared Ratatui TUI."
)]
struct Args {
    #[arg(long, global = true, env = "PCTL_STATE", default_value = ".pctl")]
    state: PathBuf,
    #[arg(long, global = true, env = "PCTL_TMUX", default_value = "tmux")]
    tmux: PathBuf,
    /// Save a state-local appearance preset; existing UIs refresh automatically.
    #[arg(long, global = true)]
    theme: Option<pctl::theme::ThemeName>,
    #[command(subcommand)]
    cmd: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    Doctor,
    /// Explicit delegated lifecycle/layout for policy-authorized registered local profiles.
    Team {
        #[command(subcommand)]
        cmd: pctl::orchestration::Command,
    },
    /// Inspect or save a state-local visual theme (no agent controls).
    Theme {
        name: Option<pctl::theme::ThemeName>,
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
    /// Short role-name mailbox workflow, bound to the launcher PCTL_* context.
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },
    Message {
        #[command(subcommand)]
        cmd: MessageCmd,
    },
    Sidebar {
        #[command(subcommand)]
        cmd: SidebarCmd,
    },
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    Client {
        #[command(subcommand)]
        cmd: ClientCmd,
    },
    Ui {
        #[arg(long)]
        project: Option<String>,
    },
    Attach {
        #[arg(long)]
        project: String,
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
enum SidebarCmd {
    /// Focus only a verified live full-manager process. Never starts an agent.
    Manager {
        #[arg(long)]
        project: String,
    },
    /// Open one optional native navigation pane. Existing agents are not restarted.
    Open {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        expected_run: String,
        #[arg(long, default_value_t = 28)]
        width: u16,
    },
    Inspect,
    /// Close only the exact registered sidebar generation.
    Close {
        #[arg(long)]
        id: String,
    },
    /// Internal native sidebar process, launched only through sidebar open.
    #[command(hide = true)]
    Ui {
        #[arg(long)]
        id: String,
    },
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
fn remote(argv: &[String]) -> Option<i32> {
    let i = argv.iter().position(|a| a == "--host")?;
    let host = argv.get(i + 1).filter(|h| !h.starts_with('-'));
    let Some(host) = host else {
        eprintln!("{}", json!({"schema_version":1,"ok":false,"error":"--host needs an ssh target","error_code":"operation_failed"}));
        return Some(2);
    };
    let bin = std::env::var("PCTL_REMOTE_BIN").unwrap_or_else(|_| "pctl".into());
    let rest = argv[1..i].iter().chain(&argv[i + 2..]);
    let line = std::iter::once(&bin)
        .chain(rest)
        .map(|a| pctl::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let status = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "--", host, &line])
        .status();
    Some(match status {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            eprintln!("{}", json!({"schema_version":1,"ok":false,"error":format!("ssh: {e}"),"error_code":"operation_failed"}));
            2
        }
    })
}
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if let Some(code) = remote(&argv) {
        std::process::exit(code)
    }
    if let Err(e) = run() {
        eprintln!(
            "{}",
            json!({"schema_version":1,"ok":false,"error":format!("{e:#}"),"error_code":pctl::error_code(&format!("{e:#}"))})
        );
        std::process::exit(2)
    }
}
fn run() -> Result<()> {
    let args = Args::parse();
    if let Cmd::Worker { label, tick_ms } = &args.cmd {
        return pctl::worker(label, *tick_ms);
    }
    if let Cmd::Team { cmd } = &args.cmd {
        pctl::orchestration::preflight(cmd)?;
    }
    let bound = matches!(&args.cmd, Cmd::Msg { .. })
        || match &args.cmd {
            Cmd::Team { cmd } => pctl::orchestration::requires_context(cmd),
            _ => false,
        };
    let core = if bound {
        anyhow::ensure!(
            args.theme.is_none(),
            "unsupported: --theme is not permitted on context-bound workflow commands; use the explicit operator theme command"
        );
        pctl::collaboration::workflow::open_bound_core(args.state, args.tmux)?
    } else {
        Core::new(args.state, args.tmux)?
    };
    if let Some(theme) = args.theme {
        core.set_theme(theme)?;
    }
    match args.cmd {
        Cmd::Theme { name } => {
            if let Some(theme) = name {
                core.set_theme(theme)?;
            }
            print(&json!({"theme": core.theme()?, "config": core.state.join("appearance.json")}))?;
        }
        Cmd::Doctor => print(&core.doctor())?,
        Cmd::Team { cmd } => print(&pctl::orchestration::run(&core, cmd)?)?,
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
            } => print(&core.update_agent(&agent, &name, &cwd, argv, &adapter)?)?,
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
        Cmd::Sidebar { cmd } => match cmd {
            SidebarCmd::Manager { project } => {
                core.sidebar_manager(&project)?;
                print(&json!({"ok":true,"result":"verified native manager focused"}))?;
            }
            SidebarCmd::Open {
                agent,
                expected_run,
                width,
            } => print(&core.open_sidebar(&agent, &expected_run, width)?)?,
            SidebarCmd::Inspect => print(&core.inspect_sidebar()?)?,
            SidebarCmd::Close { id } => {
                core.close_sidebar(&id)?;
                print(&json!({"ok":true,"result":"only sidebar pane removed; agents retained"}))?;
            }
            SidebarCmd::Ui { id } => pctl::sidebar::run(&core, &id)?,
        },
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
        Cmd::Attach { project } => core.attach(&project)?,
        Cmd::Ui { project } => pctl::ui::run(&core, project)?,
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
