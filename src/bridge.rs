//! The remote bridge and its client. `pctl bridge` serves JSON lines on stdin and stdout; `Remote`
//! is the client the app will use to drive the bridges of other computers. Nothing here starts a
//! tmux server or the pctl app. The tmux logic is a copy of the app's for now; part 2 of the
//! "other computers" brief makes the app call these functions and deletes the copies.
use crate::{Core, human::literal};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// One request line, and one answer line, are each at most this long.
const MAX_LINE: usize = 1 << 20;
/// The hello must arrive this quickly, and one answer within `ANSWER`.
const HELLO_WITHIN: Duration = Duration::from_secs(10);
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
const CURSOR_FORMAT: &str =
    "#{cursor_x} #{cursor_y} #{cursor_flag} #{pane_width} #{pane_height} #{history_size}";

/// `pctl bridge` (hidden): answer requests on stdin with one JSON object per line on stdout, until
/// stdin closes. The hello goes out first, unasked, so a client knows who answered.
pub fn run(core: &Core) -> Result<()> {
    // Same rule as the app: it types into any pane. Over ssh the caller's variables do not arrive,
    // so this only stops local agents; ssh access already reaches tmux directly anyway.
    anyhow::ensure!(
        !crate::app::agent_caller(core),
        "agent_caller: the bridge serves the app of a person on another computer; agents use `pctl session` and `msg`"
    );
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", hello())?;
    out.flush()?;
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    // The size this bridge last asked each pane for. Resizing only on a change keeps a second
    // viewer (the app on the other computer) from being resized back and forth.
    let mut asked: HashMap<String, (u16, u16)> = HashMap::new();
    while let Some((line, over)) = next_line(&mut input)? {
        let (id, result) = if over {
            (None, Err(anyhow!("line over 1 MiB")))
        } else {
            match serde_json::from_str::<Value>(&line) {
                Err(_) => (None, Err(anyhow!("invalid json"))),
                Ok(req) => match req.get("id").and_then(Value::as_u64) {
                    None => (None, Err(anyhow!("request needs a numeric id"))),
                    Some(id) => (Some(id), op(core, &req, &mut asked)),
                },
            }
        };
        writeln!(out, "{}", answer(id, result))?;
        out.flush()?;
    }
    Ok(())
}

/// One request. The run check happens before anything is touched.
fn op(core: &Core, req: &Value, asked: &mut HashMap<String, (u16, u16)>) -> Result<Value> {
    let text = |name: &str| -> Result<&str> {
        req.get(name)
            .and_then(Value::as_str)
            .with_context(|| format!("missing {name}"))
    };
    match req
        .get("op")
        .and_then(Value::as_str)
        .context("missing op")?
    {
        "list" => list(core),
        "screen" => screen(core, text("agent")?, text("run")?, req, asked),
        "keys" => send_keys(core, text("agent")?, text("run")?, req),
        "paste" => paste(core, text("agent")?, text("run")?, req),
        other => bail!("unknown op {other}"),
    }
}

/// What the observer thread in `App::run` collects: every session, the projects, and whether a
/// server owns this state.
// shortcut: copy of App::run's observer body; part 2 makes the app use this
pub fn list(core: &Core) -> Result<Value> {
    let projects: Vec<Value> = core
        .registry()
        .map(|r| {
            r.projects
                .iter()
                .map(|p| json!({"name": p.name, "root": p.root}))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "sessions": core.session_list()?,
        "projects": projects,
        "connected": core.server().is_ok_and(|m| !m.is_empty()),
    }))
}

fn screen(
    core: &Core,
    agent: &str,
    run: &str,
    req: &Value,
    asked: &mut HashMap<String, (u16, u16)>,
) -> Result<Value> {
    let (w, h) = (small(req, "width")?, small(req, "height")?);
    let scroll = usize::try_from(number(req, "scroll")?).context("scroll is too large")?;
    let pane = pane_of(core, agent, run)?;
    verified(core, &pane, run)?;
    if w >= 2 && h >= 2 && asked.get(&pane) != Some(&(w, h)) {
        asked.insert(pane.clone(), (w, h));
        resize(core, &pane, w, h)?;
    }
    capture(core, &pane, h, scroll)
}

/// The pane a run is on, from the registry. The run must exist and belong to the agent asked for.
fn pane_of(core: &Core, agent: &str, run: &str) -> Result<String> {
    let r = core.registry()?;
    let row = r
        .runs
        .iter()
        .find(|x| x.id == run)
        .with_context(|| format!("changed: no run {run}"))?;
    let owner = r
        .agents
        .iter()
        .find(|a| a.id == row.agent_id)
        .with_context(|| format!("changed: run {run} has no agent"))?;
    ensure!(
        owner.id == agent || owner.name == agent,
        "changed: run {run} belongs to {}, not {agent}",
        owner.name
    );
    Ok(row.pane.clone())
}

/// The pane still holds this run. Panes are cached from the last list; after a restart or a new
/// tmux server the same pane id can belong to another process, and nothing must be typed into it.
// shortcut: copy of App::verified; part 2 makes the app use this
pub fn verified(core: &Core, pane: &str, run: &str) -> Result<()> {
    match core.tm(&["display-message", "-p", "-t", pane, "#{@pctl_run}"]) {
        Ok(found) if found.trim() == run => Ok(()),
        Ok(_) => bail!("changed: pane {pane} no longer holds run {run}; touch nothing"),
        Err(e) => bail!("changed: pane {pane} is gone ({e:#})"),
    }
}

/// Resize the pane's window to the viewer, then hand the size back to tmux when nobody is attached
/// to it. `resize-window` sets `window-size` to manual; with no client attached, unset it at once.
// shortcut: copy of App::capture's resize step; part 2 makes the app use this
pub fn resize(core: &Core, pane: &str, w: u16, h: u16) -> Result<()> {
    let unset = format!("set-option -wu -t {pane} window-size");
    core.tm(&[
        "resize-window",
        "-t",
        pane,
        "-x",
        &w.to_string(),
        "-y",
        &h.to_string(),
        ";",
        "if-shell",
        "-F",
        "-t",
        pane,
        "#{==:#{session_attached},0}",
        &unset,
    ])?;
    Ok(())
}

/// The raw screen text, the cursor when it is visible, the pane size and the history size.
// shortcut: copy of App::capture; part 2 makes the app use this
pub fn capture(core: &Core, pane: &str, h: u16, scroll: usize) -> Result<Value> {
    let (start, end) = (
        format!("-{scroll}"),
        format!("{}", h as i64 - 1 - scroll as i64),
    );
    let mut args = vec!["capture-pane", "-p", "-e", "-t", pane];
    if scroll > 0 {
        args.extend(["-S", &start, "-E", &end]);
    }
    args.extend([";", "display-message", "-p", "-t", pane, CURSOR_FORMAT]);
    let out = core.tm(&args)?;
    let (body, last) = out.rsplit_once('\n').unwrap_or(("", out.as_str()));
    let m: Vec<u64> = last.split(' ').filter_map(|v| v.parse().ok()).collect();
    let (cursor, size, history) = if m.len() == 6 {
        (
            (m[2] == 1).then(|| json!([m[0], m[1]])),
            json!([m[3], m[4]]),
            json!(m[5]),
        )
    } else {
        (None, json!([0, 0]), json!(0))
    };
    Ok(json!({"rows": body, "cursor": cursor, "size": size, "history": history}))
}

// shortcut: copy of App::send_key; part 2 makes the app use this
pub fn send_keys(core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
    let asked = req
        .get("keys")
        .and_then(Value::as_array)
        .context("missing keys")?;
    let keys: Vec<(bool, String)> = asked
        .iter()
        .map(|k| {
            let lit = k
                .get("literal")
                .and_then(Value::as_bool)
                .context("key needs literal")?;
            let key = k
                .get("key")
                .and_then(Value::as_str)
                .context("key needs key")?;
            Ok((lit, literal(key)))
        })
        .collect::<Result<_>>()?;
    let pane = pane_of(core, agent, run)?;
    verified(core, &pane, run)?;
    for (lit, key) in keys {
        if lit {
            core.tm(&["send-keys", "-t", &pane, "-l", "--", &key])?;
        } else {
            core.tm(&["send-keys", "-t", &pane, "--", &key])?;
        }
    }
    Ok(json!({}))
}

// shortcut: copy of App::paste; part 2 makes the app use this
pub fn paste(core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
    let text = req
        .get("text")
        .and_then(Value::as_str)
        .context("missing text")?;
    let pane = pane_of(core, agent, run)?;
    verified(core, &pane, run)?;
    // One tmux command carries about 16 KB; a longer paste goes in 8 KB pieces into one buffer,
    // then out as one bracketed paste. One buffer per bridge: two clients must not mix their text.
    let buffer = format!("pctl-bridge-{}", std::process::id());
    let mut r = Ok(String::new());
    for (i, piece) in chunks(text, 8192).into_iter().enumerate() {
        let append = if i == 0 { "-b" } else { "-ab" };
        r = r.and_then(|_| core.tm(&["set-buffer", append, &buffer, "--", &literal(piece)]));
    }
    r.and_then(|_| core.tm(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &pane]))?;
    Ok(json!({}))
}

// shortcut: copy of app::chunks; part 2 makes the app use this
fn chunks(s: &str, max: usize) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = s;
    while rest.len() > max {
        let cut = (1..=max)
            .rev()
            .find(|&i| rest.is_char_boundary(i))
            .unwrap_or(rest.len());
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out.push(rest);
    out
}

/// The hello line, sent as soon as the bridge starts.
fn hello() -> Value {
    json!({"ev": "hello", "protocol": 1, "host": hostname(), "version": env!("CARGO_PKG_VERSION")})
}

fn hostname() -> String {
    Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()))
}

/// One input line, at most `MAX_LINE` bytes. `true` marks a longer line, whose tail is dropped so
/// the line after it still reads clean.
fn next_line<R: BufRead>(r: &mut R) -> std::io::Result<Option<(String, bool)>> {
    let mut buf = Vec::new();
    let n = std::io::Read::take(&mut *r, MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > MAX_LINE {
        loop {
            let mut rest = Vec::new();
            let n = std::io::Read::take(&mut *r, 8192).read_until(b'\n', &mut rest)?;
            if n == 0 || rest.ends_with(b"\n") {
                break;
            }
        }
        return Ok(Some((String::new(), true)));
    }
    Ok(Some((
        String::from_utf8_lossy(&buf).trim_end().to_string(),
        false,
    )))
}

fn number(req: &Value, name: &str) -> Result<u64> {
    req.get(name)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing {name}"))
}

fn small(req: &Value, name: &str) -> Result<u16> {
    let n = number(req, name)?;
    u16::try_from(n).with_context(|| format!("{name} is too large"))
}

/// One request line: the op object with its id.
fn request_line(id: u64, op: &Value) -> String {
    let mut map = match op {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.insert("id".into(), json!(id));
    Value::Object(map).to_string()
}

/// One answer line: the id and `ok`, plus what the op returned or why it failed.
fn answer(id: Option<u64>, result: Result<Value>) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(id));
    match result {
        Ok(extra) => {
            map.insert("ok".into(), json!(true));
            if let Value::Object(fields) = extra {
                for (k, v) in fields {
                    map.insert(k, v);
                }
            }
        }
        Err(e) => {
            map.insert("ok".into(), json!(false));
            map.insert("error".into(), json!(format!("{e:#}")));
        }
    }
    Value::Object(map)
}

/// Read a hello line: the event name, and protocol 1.
fn hello_ok(line: &str) -> Result<Hello> {
    let v: Value = serde_json::from_str(line).context("not json")?;
    ensure!(
        v.get("ev").and_then(Value::as_str) == Some("hello"),
        "not a hello"
    );
    ensure!(
        v.get("protocol").and_then(Value::as_u64) == Some(1),
        "protocol is not 1"
    );
    Ok(Hello {
        host: v
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        version: v
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// Where a `Remote` stands. `down` keeps the reason so a caller can show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteState {
    Connecting,
    Up,
    Down(String),
}

/// What the bridge said about itself in the hello.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub host: String,
    pub version: String,
}

/// A connection to one `pctl bridge` on another computer. Everything that can block runs on its
/// own threads: `request` returns at once and never waits for the answer, so the app's drawing
/// thread can call it.
pub struct Remote {
    target: String,
    /// The current connection. Only `reconnect` swaps it, and the old one goes down first.
    inner: Mutex<Arc<Inner>>,
}

struct Inner {
    state: Mutex<RemoteState>,
    hello: Mutex<Option<Hello>>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    /// Requests go to the writer thread, so a full pipe never blocks the caller.
    lines: Mutex<Option<mpsc::Sender<String>>>,
    child: Mutex<Option<Child>>,
    started: Instant,
}

struct Pending {
    tx: mpsc::Sender<Result<Value>>,
    started: Instant,
}

/// A mutex we own: a thread that panicked while holding it cannot have left our simple values in a
/// state that needs guarding, so take the lock back instead of panicking again.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Remote {
    /// Start a bridge on `target` over ssh. Returns as soon as ssh exists; the hello is read on a
    /// background thread within 10 s, or the connection goes down.
    pub fn connect(target: &str) -> Result<Remote> {
        Remote::connect_with(&env_ssh(), &env_bin(), target)
    }

    fn connect_with(ssh: &str, bin: &str, target: &str) -> Result<Remote> {
        Ok(Remote {
            target: target.to_string(),
            inner: Mutex::new(Inner::spawn_with(ssh, bin, target)?),
        })
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn state(&self) -> RemoteState {
        lock(&current(&self.inner).state).clone()
    }

    pub fn hello(&self) -> Option<Hello> {
        lock(&current(&self.inner).hello).clone()
    }

    /// Wait for the hello or for the connection to fail, at most `within`.
    pub fn wait_ready(&self, within: Duration) -> RemoteState {
        let deadline = Instant::now() + within;
        loop {
            match self.state() {
                RemoteState::Connecting if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25))
                }
                state => return state,
            }
        }
    }

    /// Send one request. `op` is a JSON object holding an `"op"`; the id is added here. The answer
    /// (whose `ok` may be false) arrives on the returned channel within 5 s, else the request fails
    /// with `timeout`. A connection already down fails at once.
    pub fn request(&self, op: Value) -> mpsc::Receiver<Result<Value>> {
        current(&self.inner).request(op)
    }

    /// Start a new connection over the same target. Pending requests on the old one fail.
    pub fn reconnect(&self) -> Result<()> {
        self.replace(Inner::spawn_with(&env_ssh(), &env_bin(), &self.target)?);
        Ok(())
    }

    #[cfg(test)]
    fn reconnect_with(&self, ssh: &str, bin: &str) -> Result<()> {
        self.replace(Inner::spawn_with(ssh, bin, &self.target)?);
        Ok(())
    }

    fn replace(&self, next: Arc<Inner>) -> Arc<Inner> {
        let old = std::mem::replace(&mut *lock(&self.inner), next);
        old.down("reconnected");
        old
    }
}

impl Drop for Remote {
    /// The bridge sees its stdin close and exits; the ssh child is killed and reaped so nothing is
    /// left behind.
    fn drop(&mut self) {
        lock(&self.inner).down("closed");
    }
}

fn current(inner: &Mutex<Arc<Inner>>) -> Arc<Inner> {
    lock(inner).clone()
}

/// The ssh program to run: `$PCTL_SSH`, else `ssh` on PATH. Tests point this at a fake ssh.
fn env_ssh() -> String {
    std::env::var("PCTL_SSH").unwrap_or_else(|_| "ssh".into())
}

/// The remote program name, like `--host`: `$PCTL_REMOTE_BIN`, else `pctl`.
fn env_bin() -> String {
    std::env::var("PCTL_REMOTE_BIN").unwrap_or_else(|_| "pctl".into())
}

impl Inner {
    fn spawn_with(ssh: &str, bin: &str, target: &str) -> Result<Arc<Inner>> {
        let line = format!("{} bridge", crate::shell_quote(bin));
        let mut child = Command::new(ssh)
            .args([
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                target,
                &line,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Never read: a pipe nobody drains would block ssh. The reason for a failure is the
            // state, not ssh's message.
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("cannot run {ssh}"))?;
        let stdin = child.stdin.take().context("ssh child has no stdin")?;
        let stdout = child.stdout.take().context("ssh child has no stdout")?;
        let inner = Arc::new(Inner {
            state: Mutex::new(RemoteState::Connecting),
            hello: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            lines: Mutex::new(None),
            child: Mutex::new(Some(child)),
            started: Instant::now(),
        });
        let (tx, rx) = mpsc::channel::<String>();
        *lock(&inner.lines) = Some(tx);
        let _ = thread::spawn({
            let inner = inner.clone();
            move || writer(inner, stdin, rx)
        });
        let _ = thread::spawn({
            let inner = inner.clone();
            move || reader(inner, stdout)
        });
        let _ = thread::spawn({
            let inner = inner.clone();
            move || sweeper(inner)
        });
        Ok(inner)
    }

    fn is_down(&self) -> bool {
        matches!(*lock(&self.state), RemoteState::Down(_))
    }

    fn request(&self, op: Value) -> mpsc::Receiver<Result<Value>> {
        let (tx, rx) = mpsc::channel();
        if let RemoteState::Down(reason) = lock(&self.state).clone() {
            let _ = tx.send(Err(anyhow!("down: {reason}")));
            return rx;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.pending).insert(
            id,
            Pending {
                tx,
                started: Instant::now(),
            },
        );
        if !self.write(id, op) {
            self.fail(id, "the ssh connection is closed");
        }
        rx
    }

    fn write(&self, id: u64, op: Value) -> bool {
        let line = request_line(id, &op);
        lock(&self.lines)
            .as_ref()
            .is_some_and(|tx| tx.send(line).is_ok())
    }

    fn fail(&self, id: u64, reason: &str) {
        if let Some(e) = lock(&self.pending).remove(&id) {
            let _ = e.tx.send(Err(anyhow!("{reason}")));
        }
    }

    /// Match one answer line to its request. An answer nobody waits for is dropped.
    fn route(&self, line: &str) {
        let Ok(answer) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let Some(id) = answer.get("id").and_then(Value::as_u64) else {
            return;
        };
        if let Some(e) = lock(&self.pending).remove(&id) {
            let _ = e.tx.send(Ok(answer));
        }
    }

    fn expire(&self, after: Duration) {
        let now = Instant::now();
        let old: Vec<u64> = lock(&self.pending)
            .iter()
            .filter(|(_, e)| now.duration_since(e.started) >= after)
            .map(|(id, _)| *id)
            .collect();
        for id in old {
            self.fail(id, &format!("timeout: no answer in {} s", after.as_secs()));
        }
    }

    /// One ending, so the reader, the writer and a drop cannot race into two: fail everything,
    /// stop the writer, then kill and reap the ssh child.
    fn down(&self, reason: &str) {
        {
            let mut state = lock(&self.state);
            if matches!(*state, RemoteState::Down(_)) {
                return;
            }
            *state = RemoteState::Down(reason.to_string());
        }
        let pending: Vec<Pending> = lock(&self.pending).drain().map(|(_, e)| e).collect();
        for e in pending {
            let _ = e.tx.send(Err(anyhow!("down: {reason}")));
        }
        lock(&self.lines).take();
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn set_up(&self, hello: Hello) {
        *lock(&self.hello) = Some(hello);
        *lock(&self.state) = RemoteState::Up;
    }
}

fn reader(inner: Arc<Inner>, stdout: ChildStdout) {
    let mut out = BufReader::new(stdout);
    let mut first = String::new();
    match out.read_line(&mut first) {
        Ok(0) => return inner.down("eof before the hello"),
        Ok(_) => {}
        Err(e) => return inner.down(&format!("read: {e}")),
    }
    match hello_ok(&first) {
        Ok(hello) => inner.set_up(hello),
        Err(e) => return inner.down(&format!("hello: {e}")),
    }
    let mut line = String::new();
    loop {
        line.clear();
        match out.read_line(&mut line) {
            Ok(0) => return inner.down("eof"),
            Ok(_) => inner.route(&line),
            Err(e) => return inner.down(&format!("read: {e}")),
        }
    }
}

fn writer(inner: Arc<Inner>, mut stdin: ChildStdin, rx: mpsc::Receiver<String>) {
    for line in rx {
        if writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            inner.down("write: the ssh connection is closed");
            return;
        }
    }
}

fn sweeper(inner: Arc<Inner>) {
    loop {
        thread::sleep(Duration::from_millis(100));
        if inner.is_down() {
            return;
        }
        if matches!(*lock(&inner.state), RemoteState::Connecting)
            && inner.started.elapsed() >= HELLO_WITHIN
        {
            return inner.down("hello: no answer within 10 s");
        }
        inner.expire(ANSWER_WITHIN);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Inner with no ssh behind it: the writer queue is ours to read, the child is absent.
    fn bare() -> (Arc<Inner>, mpsc::Receiver<String>) {
        bare_in(RemoteState::Up, Instant::now())
    }

    fn bare_in(state: RemoteState, started: Instant) -> (Arc<Inner>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(Inner {
                state: Mutex::new(state),
                hello: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                lines: Mutex::new(Some(tx)),
                child: Mutex::new(None),
                started,
            }),
            rx,
        )
    }

    /// A stand-in for ssh: a script that ignores its options, target and command and runs `body`.
    fn fake_ssh(dir: &tempfile::TempDir, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the fake ssh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        path.to_string_lossy().into_owned()
    }

    /// A hello line, as the bridge sends it.
    fn hello_line() -> String {
        json!({"ev": "hello", "protocol": 1, "host": "other", "version": "9.9.9"}).to_string()
    }

    fn wait_for_state(remote: &Remote, want: impl Fn(&RemoteState) -> bool) -> RemoteState {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = remote.state();
            if want(&state) || Instant::now() >= deadline {
                return state;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A pid that has been reaped is no longer visible to ps.
    fn is_gone(pid: &str) -> bool {
        !Command::new("/bin/ps")
            .args(["-p", pid])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn a_hello_puts_the_connection_up_and_the_drop_leaves_no_child() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pid_file = dir.path().join("pid");
        let ssh = fake_ssh(
            &dir,
            &format!(
                "echo $$ > {}\nprintf '%s\\n' '{}'\nexec sleep 30",
                pid_file.display(),
                hello_line()
            ),
        );
        let remote = Remote::connect_with(&ssh, "pctl", "somewhere").expect("start the fake ssh");
        assert_eq!(
            wait_for_state(&remote, |s| *s == RemoteState::Up),
            RemoteState::Up
        );
        assert_eq!(
            remote.hello(),
            Some(Hello {
                host: "other".into(),
                version: "9.9.9".into()
            })
        );
        let pid = std::fs::read_to_string(&pid_file).expect("the child's pid");
        let pid = pid.trim().to_string();
        assert!(!is_gone(&pid), "the fake ssh should still be running");
        drop(remote);
        assert!(is_gone(&pid), "the ssh child must be killed and reaped");
    }

    #[test]
    fn a_reconnect_starts_again_and_the_old_child_is_gone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ssh = fake_ssh(
            &dir,
            &format!(
                "echo $$ >> {}/pids\nprintf '%s\\n' '{}'\nexec sleep 30",
                dir.path().display(),
                hello_line()
            ),
        );
        let remote = Remote::connect_with(&ssh, "pctl", "somewhere").expect("start the fake ssh");
        assert_eq!(
            wait_for_state(&remote, |s| *s == RemoteState::Up),
            RemoteState::Up
        );
        remote.reconnect_with(&ssh, "pctl").expect("reconnect");
        assert_eq!(
            wait_for_state(&remote, |s| *s == RemoteState::Up),
            RemoteState::Up
        );
        let pids: Vec<String> = std::fs::read_to_string(dir.path().join("pids"))
            .expect("pids")
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(pids.len(), 2, "two connections were started");
        assert!(is_gone(&pids[0]), "the first child must be gone");
        assert!(!is_gone(&pids[1]), "the second child is the live one");
    }

    #[test]
    fn a_bad_hello_goes_down_with_its_reason() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ssh = fake_ssh(&dir, "printf 'not json\\n'\nexec sleep 30");
        let remote = Remote::connect_with(&ssh, "pctl", "somewhere").expect("start");
        let state = wait_for_state(&remote, |s| matches!(s, RemoteState::Down(_)));
        assert!(
            matches!(&state, RemoteState::Down(r) if r.starts_with("hello:")),
            "{state:?}"
        );
        assert!(
            remote
                .request(json!({"op": "list"}))
                .try_recv()
                .expect("failed")
                .is_err()
        );
    }

    #[test]
    fn an_eof_before_the_hello_goes_down() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ssh = fake_ssh(&dir, "exit 0");
        let remote = Remote::connect_with(&ssh, "pctl", "somewhere").expect("start");
        let state = wait_for_state(&remote, |s| matches!(s, RemoteState::Down(_)));
        assert!(
            matches!(&state, RemoteState::Down(r) if r.contains("eof before the hello")),
            "{state:?}"
        );
    }

    #[test]
    fn killing_the_bridge_goes_down_and_fails_what_waits() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pid_file = dir.path().join("pid");
        let ssh = fake_ssh(
            &dir,
            &format!(
                "echo $$ > {}\nprintf '%s\\n' '{}'\nexec sleep 30",
                pid_file.display(),
                hello_line()
            ),
        );
        let remote = Remote::connect_with(&ssh, "pctl", "somewhere").expect("start");
        assert_eq!(
            wait_for_state(&remote, |s| *s == RemoteState::Up),
            RemoteState::Up
        );
        let waiting = remote.request(json!({"op": "list"}));
        let pid = std::fs::read_to_string(&pid_file).expect("the child's pid");
        assert!(
            Command::new("/bin/kill")
                .args(["-9", pid.trim()])
                .status()
                .expect("kill")
                .success()
        );
        let state = wait_for_state(&remote, |s| matches!(s, RemoteState::Down(_)));
        assert!(
            matches!(&state, RemoteState::Down(r) if r == "eof"),
            "{state:?}"
        );
        assert!(
            waiting
                .recv_timeout(Duration::from_secs(6))
                .expect("an answer")
                .is_err()
        );
        remote.reconnect_with(&ssh, "pctl").expect("reconnect");
        assert_eq!(
            wait_for_state(&remote, |s| *s == RemoteState::Up),
            RemoteState::Up
        );
    }

    #[test]
    fn a_hello_that_never_arrives_times_out() {
        let (inner, _sent) = bare_in(
            RemoteState::Connecting,
            Instant::now() - Duration::from_secs(11),
        );
        let _ = thread::spawn({
            let inner = inner.clone();
            move || sweeper(inner)
        });
        for _ in 0..100 {
            if inner.is_down() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            matches!(&*lock(&inner.state), RemoteState::Down(r) if r.starts_with("hello:")),
            "{:?}",
            lock(&inner.state)
        );
    }

    #[test]
    fn a_request_line_is_one_object_with_its_id() {
        let line = request_line(7, &json!({"op": "list"}));
        let v: Value = serde_json::from_str(&line).expect("a request line is json");
        assert_eq!(v["id"], 7);
        assert_eq!(v["op"], "list");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn an_answer_carries_fields_or_the_error() {
        let ok: Value =
            serde_json::from_str(&answer(Some(3), Ok(json!({"rows": "hi"}))).to_string())
                .expect("json");
        assert_eq!(ok, json!({"id": 3, "ok": true, "rows": "hi"}));
        let bad: Value =
            serde_json::from_str(&answer(None, Err(anyhow!("changed: nope"))).to_string())
                .expect("json");
        assert_eq!(
            bad,
            json!({"id": null, "ok": false, "error": "changed: nope"})
        );
    }

    #[test]
    fn answers_match_their_ids_in_any_order() {
        let (inner, sent) = bare();
        let first = inner.request(json!({"op": "list"}));
        let second = inner.request(json!({"op": "paste", "text": "hi"}));
        let lines: Vec<String> = sent.try_iter().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"id\":1"), "{}", lines[0]);
        assert!(lines[1].contains("\"id\":2"), "{}", lines[1]);
        inner.route(&json!({"id": 2, "ok": true}).to_string());
        inner.route(&json!({"id": 1, "ok": false, "error": "changed: no run"}).to_string());
        let two = second.try_recv().expect("answered").expect("ok");
        assert_eq!(two["id"], 2);
        let one = first.try_recv().expect("answered").expect("ok");
        assert_eq!(one["ok"], false);
        assert_eq!(one["error"], "changed: no run");
        // A second answer for the same id, or an answer nobody waits for, changes nothing.
        inner.route(&json!({"id": 1, "ok": true}).to_string());
        assert!(first.try_recv().is_err());
    }

    #[test]
    fn an_unanswered_request_times_out_and_nothing_waits_forever() {
        let (inner, _sent) = bare();
        let waiting = inner.request(json!({"op": "list"}));
        {
            let mut pending = lock(&inner.pending);
            for e in pending.values_mut() {
                e.started = Instant::now() - Duration::from_secs(6);
            }
        }
        inner.expire(ANSWER_WITHIN);
        let error = waiting.try_recv().expect("failed").expect_err("timed out");
        assert!(error.to_string().contains("timeout"), "{error}");
        assert!(lock(&inner.pending).is_empty());
    }

    #[test]
    fn a_down_connection_fails_pending_and_new_requests() {
        let (inner, _sent) = bare();
        let waiting = inner.request(json!({"op": "list"}));
        inner.down("eof");
        assert!(waiting.try_recv().expect("failed").is_err());
        assert!(
            inner
                .request(json!({"op": "list"}))
                .try_recv()
                .expect("failed")
                .is_err()
        );
        assert!(matches!(lock(&inner.state).clone(), RemoteState::Down(_)));
    }

    #[test]
    fn a_reconnect_goes_down_a_fresh_connection_and_fails_the_old_one() {
        let (old, _sent) = bare();
        let waiting = old.request(json!({"op": "list"}));
        let remote = Remote {
            target: "somewhere".into(),
            inner: Mutex::new(old.clone()),
        };
        let (next, _next_sent) = bare_in(RemoteState::Connecting, Instant::now());
        let gone = remote.replace(next);
        assert!(std::ptr::eq(&*gone, &*old));
        assert_eq!(remote.state(), RemoteState::Connecting);
        assert!(waiting.try_recv().expect("failed").is_err());
        assert!(matches!(lock(&old.state).clone(), RemoteState::Down(_)));
    }

    #[test]
    fn a_line_over_1_mib_is_marked_and_its_tail_dropped() {
        let mut input =
            std::io::Cursor::new(format!("{}\n{{\"id\":1}}\n", "x".repeat(MAX_LINE + 10)));
        let (line, over) = next_line(&mut input).expect("read").expect("a line");
        assert!(over && line.is_empty());
        let (line, over) = next_line(&mut input).expect("read").expect("a line");
        assert!(!over);
        assert_eq!(line, "{\"id\":1}");
        assert!(next_line(&mut input).expect("read").is_none());
    }

    #[test]
    fn paste_pieces_stay_on_character_boundaries() {
        let text = "한".repeat(5000);
        let pieces = chunks(&text, 8192);
        assert!(pieces.iter().all(|p| p.len() <= 8192));
        assert!(pieces.iter().all(|p| !p.is_empty()));
        assert_eq!(pieces.concat(), text);
    }
}
