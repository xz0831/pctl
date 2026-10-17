//! `pctl tell <to> <text>`: one agent speaks to another, wherever it lives.
//!
//! - a pctl session on this computer (`<name>`), typed into with the sender line in front;
//! - a session on another computer (`<machine>:<name>`), by running `pctl tell` there over ssh;
//! - an A2A agent listed in `config.toml` (`[tell.a2a.<name>]`), through `curl`;
//! - `codex:<prefix>`, the session whose conversation starts with that prefix.
//!
//! Everything specific to a person's computers comes from `[tell]` in `config.toml`; nothing is
//! named here. The A2A token is read when it is sent and goes to `curl` through its stdin.
use crate::Core;
use crate::app::keys::{config_path, section, section_names};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// How long a session may take to start working before the text counts as typed, not confirmed.
const REACH_WITHIN: Duration = Duration::from_millis(8000);
/// `--wait` for a session: how long to wait for it to finish.
const WAIT_SESSION: Duration = Duration::from_millis(1_800_000);
/// The A2A first frame, and a whole `--wait` answer (curl's own limits are 60 s and 330 s).
const FRAME_WITHIN: Duration = Duration::from_secs(65);
const REPLY_WITHIN: Duration = Duration::from_secs(340);
const REPLY_LINES: usize = 40;
const MAX_FROM_LINE: usize = 400;

/// `[tell]` and `[tell.a2a.<name>]` from `config.toml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// The name of this computer in sender lines; else `hostname -s`.
    pub host: Option<String>,
    /// `XPC_SERVICE_NAME` values that mean "this computer's front door": the sender is the host.
    pub door_services: Vec<String>,
    /// Stripped from the start of any other `XPC_SERVICE_NAME`; the sender is `job:<rest>`.
    pub job_prefix: String,
    /// launchd job labels (macOS) of this computer's front door: a caller with no other marker
    /// whose parent processes include that job's process is the front door (its code tool).
    pub door_parents: Vec<String>,
    /// How an A2A agent not waited on is told to answer; default "reply with tell".
    pub reply_hint: Option<String>,
    pub a2a: BTreeMap<String, A2a>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A2a {
    pub url: String,
    pub token_file: String,
    pub token_key: String,
}

impl Config {
    pub fn from_text(text: &str) -> Config {
        let tell = section(text, "tell");
        let get = |k: &str| {
            tell.iter()
                .rev()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let a2a = section_names(text, "tell.a2a.")
            .into_iter()
            .map(|name| {
                let s = section(text, &format!("tell.a2a.{name}"));
                let get = |k: &str| {
                    s.iter()
                        .rev()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                };
                let a = A2a {
                    url: get("url"),
                    token_file: get("token_file"),
                    token_key: get("token_key"),
                };
                (name, a)
            })
            .collect();
        Config {
            host: get("host"),
            door_services: get("door_services")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            job_prefix: get("job_prefix").unwrap_or_default(),
            door_parents: get("door_parents")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            reply_hint: get("reply_hint"),
            a2a,
        }
    }
    pub fn load() -> Config {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Config::from_text(&t))
            .unwrap_or_default()
    }
}

/// Where a message goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Local(String),
    /// A session on another computer: the machine as typed, and the name there.
    Remote(String, String),
    A2a(String),
    /// The session whose conversation starts with this.
    Codex(String),
}

pub fn route(to: &str, cfg: &Config, host: &str) -> Route {
    if let Some(prefix) = to.strip_prefix("codex:") {
        return Route::Codex(prefix.to_string());
    }
    if cfg.a2a.contains_key(to) {
        return Route::A2a(to.to_string());
    }
    match to.split_once(':') {
        Some((m, name)) if m == host => route(name, cfg, host),
        Some((m, name)) if !m.is_empty() && !name.is_empty() => {
            Route::Remote(m.to_string(), name.to_string())
        }
        _ => Route::Local(to.to_string()),
    }
}

/// What the environment says about the caller.
pub struct Caller<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Whether one of these launchd jobs is among the caller's parent processes.
    pub under: &'a dyn Fn(&[String]) -> bool,
    pub host: String,
    pub cwd: String,
    pub home: String,
}

/// Who is sending, first match wins. `whoami` is the name of the caller's own session;
/// `by_conversation` finds the session that holds a Codex conversation.
/// A front door's code tool has no marker in its environment: `door_parents` finds it by its
/// parent processes.
pub fn sender(
    c: &Caller,
    cfg: &Config,
    whoami: &dyn Fn() -> Option<String>,
    by_conversation: &dyn Fn(&str) -> Option<String>,
) -> String {
    let var = |k: &str| (c.env)(k).filter(|v| !v.is_empty());
    if var("PCTL_AGENT_ID").is_some()
        && let Some(me) = whoami().filter(|m| !m.is_empty())
    {
        return me;
    }
    if let Some(thread) = var("CODEX_THREAD_ID") {
        return by_conversation(&thread)
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| format!("codex:{}", thread.chars().take(8).collect::<String>()));
    }
    if let Some(svc) = var("XPC_SERVICE_NAME").filter(|s| s != "0") {
        if cfg.door_services.contains(&svc) {
            return c.host.clone();
        }
        let rest = if cfg.job_prefix.is_empty() {
            svc.as_str()
        } else {
            svc.strip_prefix(&cfg.job_prefix).unwrap_or(&svc)
        };
        return format!("job:{rest}");
    }
    if !cfg.door_parents.is_empty() && (c.under)(&cfg.door_parents) {
        return c.host.clone();
    }
    if var("SSH_CONNECTION").is_some() {
        return "ssh".into();
    }
    "?".into()
}

/// Whether the process of one of these launchd jobs is a parent (at any depth) of this process.
/// Not on macOS, or with no such job running, it is false.
pub fn under_launchd_jobs(labels: &[String]) -> bool {
    let out = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(uid) = out("id", &["-u"]).map(|u| u.trim().to_string()) else {
        return false;
    };
    let jobs: Vec<u32> = labels
        .iter()
        .filter_map(|l| out("launchctl", &["print", &format!("gui/{uid}/{l}")]))
        .filter_map(|t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("pid = ")?.trim().parse().ok())
        })
        .collect();
    if jobs.is_empty() {
        return false;
    }
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..64 {
        if jobs.contains(&pid) {
            return true;
        }
        match out("ps", &["-o", "ppid=", "-p", &pid.to_string()])
            .and_then(|p| p.trim().parse().ok())
        {
            Some(parent) if parent > 1 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

/// `$PWD` with `$HOME` written `~`.
pub fn short_cwd(cwd: &str, home: &str) -> String {
    match cwd.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => cwd.to_string(),
    }
}

pub fn sender_line(me: &str, host: &str, cwd: &str) -> String {
    format!("[from {me} {host} {cwd}]")
}

/// An A2A agent that is not waited on is told how to answer (`[tell] reply_hint`).
pub fn with_hint(line: &str, hint: Option<&str>) -> String {
    let hint = hint.unwrap_or("reply with tell");
    format!("{} · {hint}]", line.trim_end_matches(']'))
}

/// The sender and host in `[from <me> <host> <cwd>]`.
fn parse_line(line: &str) -> Option<(String, String)> {
    let inner = line.strip_prefix("[from ")?.strip_suffix(']')?;
    let mut parts = inner.split(' ');
    Some((parts.next()?.to_string(), parts.next()?.to_string()))
}

/// A line from the other end of ssh is one line of printable text in the sender-line shape.
pub fn valid_line(line: &str) -> bool {
    line.len() <= MAX_FROM_LINE && !line.chars().any(char::is_control) && parse_line(line).is_some()
}

pub fn is_self(to: &str, me: &str, host: &str) -> bool {
    me != "?" && (to == me || to == format!("{host}:{me}"))
}

/// Letters, digits, `_` and `-` only.
pub fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// One conversation per sender per day, unless the caller names one.
pub fn context_base(given: Option<&str>, host: &str, me: &str, date: &str) -> String {
    match given.filter(|g| !g.is_empty()) {
        Some(g) => safe(g),
        None => format!("msg-{}-{}-{date}", safe(host), safe(me)),
    }
}

/// The key after an anti-loop refusal: `<base>-r2`, then `-r3`, …
pub fn next_context(base: &str, current: &str) -> String {
    let n = current
        .strip_prefix(base)
        .and_then(|rest| rest.strip_prefix("-r"))
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(1);
    format!("{base}-r{}", n.saturating_add(1))
}

pub fn rpc_body(wait: bool, message_id: &str, context: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1,
        "method": if wait { "SendMessage" } else { "SendStreamingMessage" },
        "params": {"message": {
            "messageId": message_id, "contextId": context, "role": "ROLE_USER",
            "parts": [{"text": text, "mediaType": "text/plain"}],
        }},
    })
}

/// The first event of an SSE answer, or a plain JSON line (a refusal that ended at once).
pub fn first_frame(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("data:") {
        return Some(rest.trim().to_string());
    }
    line.starts_with('{').then(|| line.to_string())
}

/// What an A2A answer says.
#[derive(Debug, Default, PartialEq)]
pub struct Reply {
    pub state: String,
    pub said: String,
    pub anti_loop: bool,
}

pub fn parse_reply(out: &str) -> Reply {
    let anti_loop = out.contains("Anti-loop protection");
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        let head: String = out.chars().take(200).collect();
        return Reply {
            said: format!("unknown answer: {head}"),
            anti_loop,
            ..Reply::default()
        };
    };
    if let Some(e) = v.get("error") {
        return Reply {
            said: format!(
                "error {} {}",
                e["code"],
                e["message"].as_str().unwrap_or_default()
            ),
            anti_loop,
            ..Reply::default()
        };
    }
    let r = &v["result"];
    let body = ["task", "statusUpdate", "artifactUpdate"]
        .iter()
        .map(|k| &r[*k])
        .find(|b| !b.is_null())
        .unwrap_or(r);
    let mut texts: Vec<String> = vec![];
    let mut add = |p: &Value| {
        for part in p.as_array().into_iter().flatten() {
            if let Some(t) = part["text"].as_str()
                && !texts.iter().any(|x| x == t)
            {
                texts.push(t.to_string());
            }
        }
    };
    add(&body["status"]["message"]["parts"]);
    for a in body["artifacts"].as_array().into_iter().flatten() {
        add(&a["parts"]);
    }
    add(&body["artifact"]["parts"]);
    let state: String = body["status"]["state"].as_str().unwrap_or_default().into();
    // An answer that went through may talk about the rule; only a refusal counts.
    let answered = matches!(
        state.as_str(),
        "TASK_STATE_COMPLETED" | "TASK_STATE_INPUT_REQUIRED"
    );
    Reply {
        anti_loop: anti_loop && !answered,
        said: texts.join("\n"),
        state,
    }
}

/// How a send ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Reached,
    Queued,
    TypedUnconfirmed,
    Accepted,
    Replied,
    SelfSend,
    /// The other side refused (A2A reasons).
    Refused,
    NoToken,
    NoConnection,
    /// Could not send: stopped, lost, exited, unknown, failed to type.
    NotSent,
    Blocked,
}
impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Reached => "reached",
            Verdict::Queued => "queued",
            Verdict::TypedUnconfirmed => "typed_unconfirmed",
            Verdict::Accepted => "accepted",
            Verdict::Replied => "replied",
            Verdict::SelfSend => "self",
            Verdict::Refused => "refused",
            Verdict::NoToken => "no_token",
            Verdict::NoConnection => "no_connection",
            Verdict::NotSent => "not_sent",
            Verdict::Blocked => "blocked",
        }
    }
    /// 0 sent, 1 refused, 3 no token, 4 connection, 5 could not send, 6 blocked.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Reached
            | Verdict::Queued
            | Verdict::TypedUnconfirmed
            | Verdict::Accepted
            | Verdict::Replied
            | Verdict::SelfSend => 0,
            Verdict::Refused => 1,
            Verdict::NoToken => 3,
            Verdict::NoConnection => 4,
            Verdict::NotSent => 5,
            Verdict::Blocked => 6,
        }
    }
    fn from_name(name: &str) -> Option<Verdict> {
        [
            Verdict::Reached,
            Verdict::Queued,
            Verdict::TypedUnconfirmed,
            Verdict::Accepted,
            Verdict::Replied,
            Verdict::SelfSend,
            Verdict::Refused,
            Verdict::NoToken,
            Verdict::NoConnection,
            Verdict::NotSent,
            Verdict::Blocked,
        ]
        .into_iter()
        .find(|r| r.name() == name)
    }
}

#[derive(Debug)]
pub struct Request {
    pub to: String,
    pub text: String,
    pub wait: bool,
    pub context: Option<String>,
    /// The sender line a computer that ssh'd here sends for its own caller.
    pub from_line: Option<String>,
}

/// What `tell` printed and how it ends.
#[derive(Debug)]
pub struct Outcome {
    pub result: Verdict,
    pub to: String,
    pub context: Option<String>,
    pub reply: Option<String>,
    /// Why it was not sent or refused, or a note about a sent one.
    pub note: Option<String>,
}
impl Outcome {
    fn new(result: Verdict, to: &str) -> Outcome {
        Outcome {
            result,
            to: to.into(),
            context: None,
            reply: None,
            note: None,
        }
    }
    fn because(result: Verdict, to: &str, note: impl Into<String>) -> Outcome {
        Outcome {
            note: Some(note.into()),
            ..Outcome::new(result, to)
        }
    }
    pub fn json(&self) -> Value {
        let mut v = json!({"to": self.to, "result": self.result.name()});
        if let Some(c) = &self.context {
            v["context"] = json!(c);
        }
        if let Some(r) = &self.reply {
            v["reply"] = json!(r);
        }
        if let Some(n) = &self.note {
            v[if self.result.exit_code() == 0 {
                "note"
            } else {
                "error"
            }] = json!(n);
        }
        v
    }
    /// One human line, then the reply if there is one.
    pub fn plain(&self) -> String {
        let mut s = format!("{}: {}", self.result.name().replace('_', " "), self.to);
        match (&self.note, self.result) {
            (Some(n), _) => s.push_str(&format!(" ({n})")),
            (None, Verdict::Queued) => s.push_str(" (it was working; it reads this next)"),
            (None, Verdict::Reached) => s.push_str(" (it started reading)"),
            (None, Verdict::TypedUnconfirmed) => {
                s.push_str(" (typed in; not seen to start; do not send again)")
            }
            (None, Verdict::Accepted) => s.push_str(" (the answer comes by tell)"),
            _ => {}
        }
        if let Some(c) = &self.context {
            s.push_str(&format!(" [conversation {c}]"));
        }
        if let Some(r) = &self.reply {
            s.push('\n');
            s.push_str(r);
        }
        s
    }
}

fn today() -> String {
    date("+%Y%m%d").unwrap_or_else(|| "00000000".into())
}
fn date(format: &str) -> Option<String> {
    Command::new("date")
        .arg(format)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// One line per send, never the text: time, sender, receiver, result, length of the text.
fn log(core: &Core, me: &str, to: &str, result: &str, len: usize) {
    let line = format!(
        "{}\t{}\t{}\t{result}\t{len}\n",
        date("+%FT%T%z").unwrap_or_default(),
        safe_field(me),
        safe_field(to)
    );
    let _ = (|| -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(core.state.join("tell.log"))?;
        f.write_all(line.as_bytes())
    })();
}
fn safe_field(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `KEY=value` lines; the last one for the key wins. Never printed.
pub fn read_token(file: &str, key: &str) -> Option<String> {
    let path = match file.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => file.into(),
    };
    let text = std::fs::read_to_string(path).ok()?;
    let prefix = format!("{key}=");
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
        .filter(|v| !v.is_empty())
}

pub fn run(core: &Core, req: &Request) -> Outcome {
    let cfg = Config::load();
    let host = cfg.host.clone().unwrap_or_else(crate::bridge::hostname);
    let home = std::env::var("HOME").unwrap_or_default();
    let me = match &req.from_line {
        Some(line) => parse_line(line).map(|(m, _)| m).unwrap_or_default(),
        None => {
            let env = |k: &str| std::env::var(k).ok();
            let caller = Caller {
                env: &env,
                under: &under_launchd_jobs,
                host: host.clone(),
                cwd: short_cwd(&working_dir(), &home),
                home: home.clone(),
            };
            sender(
                &caller,
                &cfg,
                &|| {
                    core.session_whoami()
                        .ok()
                        .and_then(|v| v["name"].as_str().map(String::from))
                },
                &|thread| conversation_owner(core, |c| c == thread),
            )
        }
    };
    let line = match &req.from_line {
        Some(l) => l.clone(),
        None => sender_line(&me, &host, &short_cwd(&working_dir(), &home)),
    };
    let out = if req.from_line.is_none() && is_self(&req.to, &me, &host) {
        Outcome {
            note: Some("to yourself; not sent".into()),
            ..Outcome::new(Verdict::SelfSend, &req.to)
        }
    } else {
        match route(&req.to, &cfg, &host) {
            Route::Local(name) => local(core, &req.to, &name, &line, req),
            Route::Codex(prefix) => match codex_session(core, &prefix) {
                Some(name) => local(core, &req.to, &name, &line, req),
                None => Outcome::because(
                    Verdict::NotSent,
                    &req.to,
                    "no session holds a conversation that starts so",
                ),
            },
            Route::Remote(machine, name) => remote(&req.to, &machine, &name, &line, req),
            Route::A2a(name) => a2a(core, &cfg, &name, &me, &host, &line, req),
        }
    };
    log(core, &me, &req.to, out.result.name(), req.text.len());
    out
}

/// `$PWD` as a shell keeps it (symlinks kept) when it still names the working directory, else the
/// working directory itself: a caller that is no shell leaves a stale `PWD`.
fn working_dir() -> String {
    let real = std::env::current_dir().ok();
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).ok();
    match std::env::var("PWD") {
        Ok(pwd)
            if !pwd.is_empty()
                && canon(std::path::Path::new(&pwd)).is_some()
                && canon(std::path::Path::new(&pwd)) == real.as_deref().and_then(canon) =>
        {
            pwd
        }
        _ => real.map(|p| p.display().to_string()).unwrap_or_default(),
    }
}

/// The name of the session whose conversation `matches`.
fn conversation_owner(core: &Core, matches: impl Fn(&str) -> bool) -> Option<String> {
    core.session_list().ok()?.into_iter().find_map(|s| {
        matches(s["conversation"].as_str().unwrap_or_default())
            .then(|| s["name"].as_str().map(String::from))
            .flatten()
    })
}
fn codex_session(core: &Core, prefix: &str) -> Option<String> {
    (prefix.len() >= 4)
        .then(|| conversation_owner(core, |c| c.starts_with(prefix)))
        .flatten()
}

/// A session on this computer.
fn local(core: &Core, to: &str, name: &str, line: &str, req: &Request) -> Outcome {
    let before = match core.session_get(name) {
        Ok(v) => v["status"].as_str().unwrap_or_default().to_string(),
        Err(e) => return Outcome::because(Verdict::NotSent, to, format!("{e:#}")),
    };
    match before.as_str() {
        "blocked" => {
            return Outcome::because(
                Verdict::Blocked,
                to,
                "it waits for a human decision; nothing was typed",
            );
        }
        "stopped" | "lost" | "exited" => {
            return Outcome::because(Verdict::NotSent, to, format!("the session is {before}"));
        }
        _ => {}
    }
    if let Err(e) = core.session_prompt(name, &format!("{line} {}", req.text)) {
        let why = format!("{e:#}");
        let result = if why.starts_with("blocked:") {
            Verdict::Blocked
        } else {
            Verdict::NotSent
        };
        return Outcome::because(result, to, why);
    }
    let result = if before == "working" {
        Verdict::Queued
    } else if core
        .session_wait(
            name,
            &["working".into(), "blocked".into()],
            REACH_WITHIN,
            None,
        )
        .is_ok()
    {
        Verdict::Reached
    } else {
        Verdict::TypedUnconfirmed
    };
    let mut out = Outcome::new(result, to);
    if req.wait {
        if core.session_wait(name, &[], WAIT_SESSION, None).is_err() {
            out.note = Some("gave up waiting; check its state".into());
        }
        out.reply = core.session_read(name, REPLY_LINES).ok();
    }
    out
}

/// A session on another computer: `pctl tell` runs there with this caller's sender line.
fn remote(to: &str, machine: &str, name: &str, line: &str, req: &Request) -> Outcome {
    let target = match crate::machine::resolve(machine) {
        Ok(t) => t,
        Err(e) => {
            // A saved label that is switched off is not an ssh host name.
            let saved = crate::machine::load()
                .unwrap_or_default()
                .iter()
                .any(|m| m.label == machine);
            if saved {
                return Outcome::because(Verdict::NotSent, to, format!("{e:#}"));
            }
            machine.to_string()
        }
    };
    if target.starts_with('-') || target.is_empty() {
        return Outcome::because(Verdict::NotSent, to, "not an ssh target");
    }
    let bin = std::env::var("PCTL_REMOTE_BIN").unwrap_or_else(|_| "pctl".into());
    let mut words = vec![
        bin,
        "tell".into(),
        name.into(),
        req.text.clone(),
        "--from-line".into(),
        line.into(),
    ];
    if req.wait {
        words.push("--wait".into());
    }
    if let Some(c) = &req.context {
        words.extend(["--context".into(), c.clone()]);
    }
    let command = words
        .iter()
        .map(|w| crate::shell_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    let ssh = std::env::var("PCTL_SSH").unwrap_or_else(|_| "ssh".into());
    let output = Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            &target,
        ])
        .arg(&command)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    let Ok(output) = output else {
        return Outcome::because(Verdict::NoConnection, to, "ssh did not start");
    };
    let code = output.status.code().unwrap_or(255);
    let text = String::from_utf8_lossy(&output.stdout);
    let answer: Option<Value> = serde_json::from_str(text.trim()).ok();
    let result = answer
        .as_ref()
        .and_then(|v| v["result"].as_str())
        .and_then(Verdict::from_name);
    match (result, code) {
        (Some(result), _) => Outcome {
            result,
            to: to.into(),
            context: answer
                .as_ref()
                .and_then(|v| v["context"].as_str().map(String::from)),
            reply: answer
                .as_ref()
                .and_then(|v| v["reply"].as_str().map(String::from)),
            note: answer.as_ref().and_then(|v| {
                v["note"]
                    .as_str()
                    .or_else(|| v["error"].as_str())
                    .map(String::from)
            }),
        },
        // ssh itself failed (no route, refused, key).
        (None, 255) => Outcome::because(
            Verdict::NoConnection,
            to,
            format!("cannot reach {machine} over ssh"),
        ),
        (None, _) => Outcome::because(
            Verdict::NotSent,
            to,
            format!(
                "{machine} answered with exit {code} and no result (is pctl there new enough to have `tell`?)"
            ),
        ),
    }
}

/// An A2A agent from the config.
fn a2a(
    core: &Core,
    cfg: &Config,
    name: &str,
    me: &str,
    host: &str,
    line: &str,
    req: &Request,
) -> Outcome {
    let to = &req.to;
    let Some(a) = cfg.a2a.get(name) else {
        return Outcome::because(Verdict::NotSent, to, "not configured");
    };
    if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
        return Outcome::because(
            Verdict::NotSent,
            to,
            format!("[tell.a2a.{name}] needs a url that starts with http:// or https://"),
        );
    }
    let Some(token) = read_token(&a.token_file, &a.token_key) else {
        return Outcome::because(
            Verdict::NoToken,
            to,
            format!(
                "no {} in {} (set token_file and token_key in [tell.a2a.{name}])",
                a.token_key, a.token_file
            ),
        );
    };
    let base = context_base(req.context.as_deref(), host, me, &today());
    let file = core
        .state
        .join("tell")
        .join(format!("ctx-{}-{base}", safe(name)));
    let mut context = std::fs::read_to_string(&file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with(&base))
        .unwrap_or_else(|| base.clone());
    let text = format!(
        "{} {}",
        if req.wait {
            line.to_string()
        } else {
            with_hint(line, cfg.reply_hint.as_deref())
        },
        req.text
    );
    // One retry after an anti-loop refusal, on the next key.
    let mut reply = Reply::default();
    for attempt in 0..2 {
        let body = rpc_body(req.wait, &uuid::Uuid::new_v4().to_string(), &context, &text);
        match curl(&a.url, &body.to_string(), &token, !req.wait) {
            Ok(out) => reply = parse_reply(&out),
            Err(why) => return Outcome::because(Verdict::NoConnection, to, why),
        }
        if reply.anti_loop && attempt == 0 {
            context = next_context(&base, &context);
            remember(&file, &context);
            continue;
        }
        break;
    }
    let mut out = Outcome::new(Verdict::Refused, to);
    out.context = Some(context);
    match (req.wait, reply.state.as_str()) {
        (false, "TASK_STATE_SUBMITTED") => out.result = Verdict::Accepted,
        (true, "TASK_STATE_COMPLETED" | "TASK_STATE_INPUT_REQUIRED") => {
            out.result = Verdict::Replied;
            out.reply = Some(reply.said);
        }
        _ if reply.anti_loop => {
            out.note = Some("refused twice (anti-loop protection)".into());
        }
        _ => {
            let state = if reply.state.is_empty() {
                "error"
            } else {
                &reply.state
            };
            out.note = Some(if reply.said.is_empty() {
                state.to_string()
            } else {
                format!("{state}: {}", reply.said)
            });
        }
    }
    out
}

/// Remember the conversation key after an anti-loop refusal.
fn remember(file: &std::path::Path, context: &str) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let _ = (|| -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(file)?;
        writeln!(f, "{context}")
    })();
}

/// POST the body with `curl`. The token travels on curl's stdin as a header file (`-H @-`), so it
/// is in no argument list. With `first_frame` the answer is the first event of the stream and the
/// connection is dropped after it; the other side's work goes on. Err is why nothing arrived.
fn curl(url: &str, body: &str, token: &str, first: bool) -> Result<String, String> {
    let mut args: Vec<&str> = vec!["-sS"];
    if first {
        args.extend(["-N", "--max-time", "60", "-H", "Accept: text/event-stream"]);
    } else {
        args.extend(["--max-time", "330"]);
    }
    args.extend([
        "-H",
        "A2A-Version: 1.0",
        "-H",
        "Content-Type: application/json",
        "-H",
        "@-",
        "--data-binary",
        body,
        url,
    ]);
    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl did not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "Authorization: Bearer {token}");
    }
    let (tx, rx) = mpsc::channel::<Option<String>>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            if first {
                let mut line = String::new();
                while stdout.read_line(&mut line).unwrap_or(0) > 0 {
                    if let Some(frame) = first_frame(line.trim_end()) {
                        let _ = tx.send(Some(frame));
                        return;
                    }
                    line.clear();
                }
                let _ = tx.send(None);
            } else {
                let mut all = String::new();
                let _ = stdout.read_to_string(&mut all);
                let _ = tx.send(Some(all));
            }
        });
    }
    let (etx, erx) = mpsc::channel::<String>();
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.take(2000).read_to_string(&mut s);
            let _ = etx.send(s);
        });
    }
    let got = rx.recv_timeout(if first { FRAME_WITHIN } else { REPLY_WITHIN });
    // Stop curl in any case: after the first frame, after a timeout, after a failure.
    let _ = child.kill();
    let status = child.wait();
    let why = erx
        .recv_timeout(Duration::from_millis(300))
        .unwrap_or_default();
    let why = why.lines().next().unwrap_or_default().trim().to_string();
    match got {
        Ok(Some(out)) if first => Ok(out),
        Ok(Some(out)) if status.is_ok_and(|s| s.success()) && !out.trim().is_empty() => Ok(out),
        _ => Err(if why.is_empty() {
            "no answer".into()
        } else {
            why
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }
    fn cfg() -> Config {
        Config::from_text(
            "[tell]\nhost = \"alpha\"\ndoor_services = \"svc.door, svc.other\"\njob_prefix = \"local.jobs.\"\n\n[tell.a2a.front]\nurl = http://127.0.0.1:9/\ntoken_file = \"~/t.env\"\ntoken_key = FRONT_TOKEN\n",
        )
    }
    fn who(env: &[(&str, &str)], names: Option<&str>, conv: Option<&str>) -> String {
        let env = env_of(env);
        let c = Caller {
            env: &env,
            under: &|_| false,
            host: "alpha".into(),
            cwd: "/w".into(),
            home: "/h".into(),
        };
        sender(&c, &cfg(), &|| names.map(String::from), &|_| {
            conv.map(String::from)
        })
    }
    #[test]
    fn a_caller_under_a_front_door_job_is_the_host() {
        let cfg =
            Config::from_text("[tell]\nhost = \"alpha\"\ndoor_parents = \"door.job, other\"\n");
        assert_eq!(cfg.door_parents, ["door.job", "other"]);
        let env = env_of(&[("SSH_CONNECTION", "x")]);
        let mk = |under: &dyn Fn(&[String]) -> bool| {
            let c = Caller {
                env: &env,
                under,
                host: "alpha".into(),
                cwd: "/w".into(),
                home: "/h".into(),
            };
            sender(&c, &cfg, &|| None, &|_| None)
        };
        assert_eq!(
            mk(&|labels| labels.iter().any(|l| l == "door.job")),
            "alpha"
        );
        assert_eq!(mk(&|_| false), "ssh", "not under the job: the usual rules");
        let none = Config::from_text("[tell]\nhost = \"alpha\"\n");
        let c = Caller {
            env: &env,
            under: &|_| panic!("no door_parents: never asked"),
            host: "alpha".into(),
            cwd: "/w".into(),
            home: "/h".into(),
        };
        assert_eq!(sender(&c, &none, &|| None, &|_| None), "ssh");
    }

    #[test]
    fn the_sender_is_chosen_by_the_first_match() {
        // 1. A pctl session beats everything, and a failed whoami falls through.
        let both = [("PCTL_AGENT_ID", "a1"), ("CODEX_THREAD_ID", "abcdefgh12")];
        assert_eq!(who(&both, Some("panel-main"), None), "panel-main");
        assert_eq!(who(&both, None, None), "codex:abcdefgh");
        // 2. A Codex thread: its session, else codex:<first 8>.
        let codex = [("CODEX_THREAD_ID", "0123456789abcdef")];
        assert_eq!(who(&codex, None, Some("worker")), "worker");
        assert_eq!(who(&codex, None, None), "codex:01234567");
        // 3. A listed service is this computer's front door: the host name.
        assert_eq!(
            who(&[("XPC_SERVICE_NAME", "svc.door")], None, None),
            "alpha"
        );
        assert_eq!(
            who(&[("XPC_SERVICE_NAME", "svc.other")], None, None),
            "alpha"
        );
        // 4. Another service is a job; the configured prefix is stripped.
        assert_eq!(
            who(&[("XPC_SERVICE_NAME", "local.jobs.backup")], None, None),
            "job:backup"
        );
        assert_eq!(
            who(&[("XPC_SERVICE_NAME", "other.thing")], None, None),
            "job:other.thing"
        );
        // "0" is what a plain terminal reports: not a job.
        assert_eq!(who(&[("XPC_SERVICE_NAME", "0")], None, None), "?");
        // 5. ssh, 6. nothing.
        assert_eq!(
            who(&[("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")], None, None),
            "ssh"
        );
        assert_eq!(who(&[], None, None), "?");
        // Empty values do not count.
        assert_eq!(
            who(
                &[("PCTL_AGENT_ID", ""), ("XPC_SERVICE_NAME", "")],
                Some("x"),
                None
            ),
            "?"
        );
        // The service wins over ssh, as in the script.
        assert_eq!(
            who(
                &[("XPC_SERVICE_NAME", "svc.door"), ("SSH_CONNECTION", "x")],
                None,
                None
            ),
            "alpha"
        );
    }
    #[test]
    fn the_sender_line_has_name_host_and_a_short_path() {
        assert_eq!(short_cwd("/h/proj/x", "/h"), "~/proj/x");
        assert_eq!(short_cwd("/h", "/h"), "~");
        assert_eq!(
            short_cwd("/home2/x", "/h"),
            "/home2/x",
            "not a prefix of the path, only of the text"
        );
        assert_eq!(short_cwd("/x", ""), "/x");
        let line = sender_line("me", "alpha", "~/p");
        assert_eq!(line, "[from me alpha ~/p]");
        assert_eq!(
            with_hint(&line, None),
            "[from me alpha ~/p · reply with tell]"
        );
        assert_eq!(
            with_hint(&line, Some("reply by msg")),
            "[from me alpha ~/p · reply by msg]"
        );
        assert_eq!(parse_line(&line), Some(("me".into(), "alpha".into())));
        assert!(valid_line(&line));
        for bad in [
            "from me alpha ~",
            "[from me]",
            "[from a b c]\n",
            "[from a b \u{1b}c]",
            &format!("[from a b {}]", "x".repeat(500)),
        ] {
            assert!(!valid_line(bad), "{bad:?}");
        }
    }
    #[test]
    fn receivers_are_routed_by_where_they_live() {
        let c = cfg();
        assert_eq!(route("worker", &c, "alpha"), Route::Local("worker".into()));
        assert_eq!(route("front", &c, "alpha"), Route::A2a("front".into()));
        assert_eq!(
            route("beta:worker", &c, "alpha"),
            Route::Remote("beta".into(), "worker".into())
        );
        assert_eq!(
            route("alpha:worker", &c, "alpha"),
            Route::Local("worker".into()),
            "this computer's own name"
        );
        assert_eq!(
            route("alpha:front", &c, "alpha"),
            Route::A2a("front".into())
        );
        assert_eq!(
            route("codex:0123abcd", &c, "alpha"),
            Route::Codex("0123abcd".into())
        );
        assert_eq!(
            route("beta:codex:0123abcd", &c, "alpha"),
            Route::Remote("beta".into(), "codex:0123abcd".into())
        );
        assert_eq!(route(":x", &c, "alpha"), Route::Local(":x".into()));
        assert!(is_self("me", "me", "alpha") && is_self("alpha:me", "me", "alpha"));
        assert!(
            !is_self("beta:me", "me", "alpha")
                && !is_self("x", "?", "alpha")
                && !is_self("?", "?", "alpha")
        );
    }
    #[test]
    fn the_config_is_read_without_a_real_parser() {
        let c = cfg();
        assert_eq!(c.host.as_deref(), Some("alpha"));
        assert_eq!(c.door_services, ["svc.door", "svc.other"]);
        assert_eq!(c.job_prefix, "local.jobs.");
        assert_eq!(
            c.a2a["front"],
            A2a {
                url: "http://127.0.0.1:9/".into(),
                token_file: "~/t.env".into(),
                token_key: "FRONT_TOKEN".into()
            }
        );
        assert_eq!(Config::from_text(""), Config::default());
        assert_eq!(
            Config::from_text("[keys]\nhost = \"x\"\n").host,
            None,
            "only [tell]"
        );
    }
    #[test]
    fn the_conversation_key_is_one_per_sender_per_day() {
        assert_eq!(
            context_base(None, "alpha", "panel-main", "20261017"),
            "msg-alpha-panel-main-20261017"
        );
        assert_eq!(
            context_base(None, "alpha", "job:a b/c", "20261017"),
            "msg-alpha-job-a-b-c-20261017"
        );
        assert_eq!(
            context_base(Some("card 7"), "alpha", "me", "20261017"),
            "card-7"
        );
        assert_eq!(
            context_base(Some(""), "alpha", "me", "20261017"),
            "msg-alpha-me-20261017"
        );
        assert_eq!(safe("a.b;c é"), "a-b-c--");
        assert_eq!(safe("ok_1-2"), "ok_1-2");
    }
    #[test]
    fn an_anti_loop_refusal_moves_to_the_next_key() {
        let b = "msg-alpha-me-20261017";
        assert_eq!(next_context(b, b), format!("{b}-r2"));
        assert_eq!(next_context(b, &format!("{b}-r2")), format!("{b}-r3"));
        assert_eq!(next_context(b, &format!("{b}-r9")), format!("{b}-r10"));
        // A remembered key of another day or another name is not a suffix of this base.
        assert_eq!(next_context(b, "other-r4"), format!("{b}-r2"));
        assert_eq!(next_context("x-r2", "x-r2"), "x-r2-r2");
    }
    #[test]
    fn the_request_is_a_json_rpc_message() {
        let v = rpc_body(false, "m-1", "ctx", "[from a b c] hi \"there\"");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "SendStreamingMessage");
        let m = &v["params"]["message"];
        assert_eq!(
            (
                m["messageId"].as_str(),
                m["contextId"].as_str(),
                m["role"].as_str()
            ),
            (Some("m-1"), Some("ctx"), Some("ROLE_USER"))
        );
        assert_eq!(
            m["parts"][0],
            json!({"text": "[from a b c] hi \"there\"", "mediaType": "text/plain"})
        );
        assert_eq!(rpc_body(true, "m", "c", "t")["method"], "SendMessage");
    }
    #[test]
    fn only_the_first_event_of_a_stream_is_read() {
        assert_eq!(first_frame("data: {\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(first_frame("data:{\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(
            first_frame("{\"error\":1}").as_deref(),
            Some("{\"error\":1}")
        );
        for skip in ["", ": keep-alive", "event: message", "id: 4"] {
            assert_eq!(first_frame(skip), None, "{skip:?}");
        }
    }
    #[test]
    fn answers_are_read_for_state_text_and_refusals() {
        let submitted = r#"{"jsonrpc":"2.0","id":1,"result":{"task":{"id":"t","contextId":"c","status":{"state":"TASK_STATE_SUBMITTED"}}}}"#;
        let r = parse_reply(submitted);
        assert_eq!(
            (r.state.as_str(), r.said.as_str(), r.anti_loop),
            ("TASK_STATE_SUBMITTED", "", false)
        );
        let done = r#"{"result":{"task":{"status":{"state":"TASK_STATE_COMPLETED","message":{"parts":[{"text":"pong"}]}},"artifacts":[{"parts":[{"text":"pong"},{"text":"more"}]}]}}}"#;
        let r = parse_reply(done);
        assert_eq!(
            (r.state.as_str(), r.said.as_str()),
            ("TASK_STATE_COMPLETED", "pong\nmore"),
            "no repeats"
        );
        let update = r#"{"result":{"statusUpdate":{"status":{"state":"TASK_STATE_REJECTED","message":{"parts":[{"text":"Anti-loop protection: context c exceeded 5 turns."}]}}}}}"#;
        let r = parse_reply(update);
        assert!(r.anti_loop && r.state == "TASK_STATE_REJECTED", "{r:?}");
        let error = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"nope"}}"#;
        let r = parse_reply(error);
        assert_eq!(
            (r.state.as_str(), r.said.as_str()),
            ("", "error -32001 nope")
        );
        let talk = done.replace("pong", "see Anti-loop protection in the docs");
        let r = parse_reply(&talk);
        assert!(
            !r.anti_loop && r.state == "TASK_STATE_COMPLETED",
            "a reply that mentions the rule is no refusal"
        );
        let r = parse_reply("<html>bad gateway</html>");
        assert!(r.said.starts_with("unknown answer: <html>") && r.state.is_empty());
    }
    #[test]
    fn every_result_has_its_exit_code() {
        let table = [
            (Verdict::Reached, 0),
            (Verdict::Queued, 0),
            (Verdict::TypedUnconfirmed, 0),
            (Verdict::Accepted, 0),
            (Verdict::Replied, 0),
            (Verdict::SelfSend, 0),
            (Verdict::Refused, 1),
            (Verdict::NoToken, 3),
            (Verdict::NoConnection, 4),
            (Verdict::NotSent, 5),
            (Verdict::Blocked, 6),
        ];
        for (r, code) in table {
            assert_eq!(r.exit_code(), code, "{r:?}");
            assert_eq!(
                Verdict::from_name(r.name()),
                Some(r),
                "{r:?} round-trips by name"
            );
        }
        // Names are what scripts read: they must not drift.
        let names: Vec<&str> = table.iter().map(|(r, _)| r.name()).collect();
        assert_eq!(
            names,
            [
                "reached",
                "queued",
                "typed_unconfirmed",
                "accepted",
                "replied",
                "self",
                "refused",
                "no_token",
                "no_connection",
                "not_sent",
                "blocked"
            ]
        );
    }
    #[test]
    fn the_output_is_json_or_one_plain_line() {
        let mut o = Outcome::new(Verdict::Reached, "worker");
        assert_eq!(o.json(), json!({"to": "worker", "result": "reached"}));
        assert_eq!(o.plain(), "reached: worker (it started reading)");
        o.context = Some("c1".into());
        o.reply = Some("pong".into());
        assert_eq!(o.json()["context"], "c1");
        assert_eq!(o.json()["reply"], "pong");
        assert_eq!(
            o.plain(),
            "reached: worker (it started reading) [conversation c1]\npong"
        );
        let b = Outcome::because(Verdict::Blocked, "w", "it waits");
        assert_eq!(
            b.json(),
            json!({"to": "w", "result": "blocked", "error": "it waits"})
        );
        assert_eq!(b.plain(), "blocked: w (it waits)");
        let n = Outcome::because(Verdict::NotSent, "w", "the session is stopped");
        assert_eq!(n.plain(), "not sent: w (the session is stopped)");
        assert_eq!(
            Outcome::new(Verdict::Queued, "w").plain(),
            "queued: w (it was working; it reads this next)"
        );
    }
    #[test]
    fn the_token_is_the_last_line_for_its_key_and_is_found_only_by_key() {
        let d = tempfile::tempdir().expect("tempdir");
        let f = d.path().join("api.env");
        std::fs::write(&f, "OTHER=x\nTOKEN=old\nTOKEN_EXTRA=nope\nTOKEN=\"new\"\n").expect("write");
        let file = f.to_str().expect("utf8");
        assert_eq!(read_token(file, "TOKEN").as_deref(), Some("new"));
        assert_eq!(read_token(file, "OTHER").as_deref(), Some("x"));
        assert_eq!(read_token(file, "MISSING"), None);
        assert_eq!(read_token("/nonexistent/file", "TOKEN"), None);
        std::fs::write(&f, "TOKEN=\n").expect("write");
        assert_eq!(read_token(file, "TOKEN"), None, "empty is missing");
    }
}
