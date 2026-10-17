//! The remote bridge and its client. `pctl bridge` serves JSON lines on stdin and stdout; `Remote`
//! is the client the app uses to drive the bridges of other computers. The tmux-level pieces the
//! app shows an agent with (read a screen, type, paste, act, list) live here once: the app calls
//! them for its own computer and a bridge calls them for the app of another. Nothing here starts a
//! tmux server or the pctl app.
use crate::{
    Core,
    app::term::unbase64,
    clipboard::{self, MAX_IMAGE},
    human::literal,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
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
const CURSOR_FORMAT: &str = "#{cursor_x} #{cursor_y} #{cursor_flag} #{pane_width} #{pane_height} #{history_size} #{alternate_on} #{mouse_any_flag} #{mouse_sgr_flag} #{@pctl_run}";

/// `pctl bridge` (hidden): answer requests on stdin with one JSON object per line on stdout, until
/// stdin closes. The hello goes out first, unasked, so a client knows who answered.
pub fn run(core: &Core) -> Result<()> {
    // Same rule as the app: it types into any pane. Over ssh the caller's variables do not arrive,
    // so this only stops local agents; ssh access already reaches tmux directly anyway.
    anyhow::ensure!(
        !crate::app::agent_caller(core),
        "agent_caller: the bridge serves the app of a person on another computer; agents use `pctl session` and `msg`"
    );
    // One locked write per line: a list answers from its own thread, and lines must not interleave.
    let send = |line: Value| -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")?;
        out.flush()
    };
    send(hello())?;
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    // The size this bridge last asked each pane for. Resizing only on a change keeps a second
    // viewer (the app on the other computer) from being resized back and forth.
    let mut asked: HashMap<String, (u16, u16)> = HashMap::new();
    let mut clip = Clip::new(core);
    let mut lists: Vec<thread::JoinHandle<std::io::Result<()>>> = vec![];
    while let Some((line, over)) = next_line(&mut input)? {
        let (id, result) = if over {
            (None, Err(anyhow!("line over 1 MiB")))
        } else {
            match serde_json::from_str::<Value>(&line) {
                Err(_) => (None, Err(anyhow!("invalid json"))),
                Ok(req) => match req.get("id").and_then(Value::as_u64) {
                    None => (None, Err(anyhow!("request needs a numeric id"))),
                    // A list checks every agent's process (~0.1 s with 30 agents); screens and keys
                    // must not wait behind it. The client keeps one list in flight.
                    Some(id) if req["op"] == "list" => {
                        let core = core.clone();
                        lists.retain(|h| !h.is_finished());
                        lists.push(thread::spawn(move || send(answer(Some(id), list(&core)))));
                        continue;
                    }
                    Some(id) => (Some(id), op(core, &req, &mut asked, &mut clip)),
                },
            }
        };
        send(answer(id, result))?;
    }
    // A client that closes right after asking still gets its list.
    for h in lists {
        let _ = h.join();
    }
    core.set_app_viewed(&[])?;
    Ok(())
}

/// One request. The run check happens before anything is touched.
fn op(
    core: &Core,
    req: &Value,
    asked: &mut HashMap<String, (u16, u16)>,
    clip: &mut Clip,
) -> Result<Value> {
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
        "view" => {
            let runs: Vec<String> = serde_json::from_value(req["runs"].clone())?;
            ensure!(runs.len() <= 256, "too many viewed runs");
            core.set_app_viewed(&runs)?;
            Ok(json!({"viewed":runs.len()}))
        }
        "list" => list(core),
        "doc_read" => core.doc_read(text("doc")?),
        "screen" => screen(core, text("agent")?, text("run")?, req, asked),
        "keys" => send_keys(core, text("agent")?, text("run")?, req),
        "paste" => paste(core, text("agent")?, text("run")?, req),
        "clip_put" => clip.put(core, text("agent")?, text("run")?, req),
        "clip_set" => clip.set(core, text("agent")?, text("run")?, req),
        "act" => act(core, req),
        other => bail!("unknown op {other}"),
    }
}

/// What the app's observer collects: every session, the projects, and whether a server owns this
/// state. The app calls this for its own computer and asks it of the others over the bridge. A
/// failing session read is `error`, with the projects and the server still listed.
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
    let sessions = core.session_list();
    Ok(json!({
        "error": sessions.as_ref().err().map(|e| format!("{e:#}")),
        "sessions": sessions.unwrap_or_default(),
        "projects": projects,
        "connected": core.server().is_ok_and(|m| !m.is_empty()),
        "docs": core.doc_list(None),
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
    if w >= 2 && h >= 2 && asked.get(&pane) != Some(&(w, h)) {
        verified(core, &pane, run)?;
        asked.insert(pane.clone(), (w, h));
        resize(core, &pane, w, h)?;
    }
    let Some(s) = read_screen(core, &pane, run, h, scroll)? else {
        bail!("changed: pane {pane} no longer holds run {run}; touch nothing");
    };
    Ok(json!({
        "rows": s.text, "history": s.history,
        "cursor": s.cursor.map(|(x, y)| json!([x, y])), "size": [s.size.0, s.size.1],
        "alt": s.alt, "mouse": s.mouse, "sgr": s.sgr,
    }))
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
/// tmux server the same pane id can belong to another process, and nothing must be touched in it.
/// Used before a resize; typing, pasting and reading check inside the call that does them.
pub fn verified(core: &Core, pane: &str, run: &str) -> Result<()> {
    match core.tm(&["display-message", "-p", "-t", pane, "#{@pctl_run}"]) {
        Ok(found) if found.trim() == run => Ok(()),
        Ok(_) => bail!("changed: pane {pane} no longer holds run {run}; touch nothing"),
        Err(e) => bail!("changed: pane {pane} is gone ({e:#})"),
    }
}

/// Resize the pane's window to the viewer, then hand the size back to tmux when nobody is attached
/// to it. `resize-window` sets `window-size` to manual; with no client attached, unset it at once:
/// tmux 3.8 keeps the size and nothing is left behind however the viewer ends. With a client
/// attached, unsetting would hand the window straight back to that client's size, so it stays
/// manual until the viewer leaves.
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

/// A pane's screen as the viewer needs it: the raw text, the cursor when it is visible, the pane
/// size and the history size, and whether the program there is full-screen (`alt`) and reads the
/// mouse (`mouse`, `sgr` for the SGR encoding): then the wheel belongs to it, not to tmux history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    pub text: String,
    pub cursor: Option<(u16, u16)>,
    pub size: (u16, u16),
    pub history: usize,
    pub alt: bool,
    pub mouse: bool,
    pub sgr: bool,
}

impl Screen {
    /// A screen from a `screen` answer.
    pub fn from_answer(v: &Value) -> Screen {
        let pair = |k: &str| {
            let a = v.get(k).and_then(Value::as_array);
            let n = |i: usize| a.and_then(|a| a.get(i)).and_then(Value::as_u64);
            Some((n(0)? as u16, n(1)? as u16))
        };
        Screen {
            text: v["rows"].as_str().unwrap_or_default().to_string(),
            cursor: pair("cursor"),
            size: pair("size").unwrap_or_default(),
            history: v["history"].as_u64().unwrap_or(0) as usize,
            alt: v["alt"].as_bool().unwrap_or(false),
            mouse: v["mouse"].as_bool().unwrap_or(false),
            sgr: v["sgr"].as_bool().unwrap_or(false),
        }
    }
}

/// Read the pane's screen, `h` rows ending `scroll` lines above the bottom. The same call reads
/// the pane's run: None when it is not `run` (or the pane is gone), and what was read is dropped.
pub fn read_screen(
    core: &Core,
    pane: &str,
    run: &str,
    h: u16,
    scroll: usize,
) -> Result<Option<Screen>> {
    // A pane taller than the view shows its bottom rows live; scrolled back it keeps that anchor,
    // so the window is `scroll` lines above the live one, not above the pane's top.
    let anchor = if scroll > 0 {
        core.tm(&["display-message", "-p", "-t", pane, "#{pane_height}"])
            .ok()
            .and_then(|t| t.trim().parse::<i64>().ok())
            .map_or(0, |ph| (ph - i64::from(h)).max(0))
    } else {
        0
    };
    let (start, end) = (
        format!("{}", anchor - scroll as i64),
        format!("{}", anchor + h as i64 - 1 - scroll as i64),
    );
    // -N keeps trailing blanks: a coloured bar (a message box, a selected row) reaches the edge.
    let mut args = vec!["capture-pane", "-p", "-e", "-N", "-t", pane];
    if scroll > 0 {
        args.extend(["-S", &start, "-E", &end]);
    }
    args.extend([";", "display-message", "-p", "-t", pane, CURSOR_FORMAT]);
    let out = match core.tm(&args) {
        Ok(out) => out,
        Err(e) if e.to_string().contains("can't find") => return Ok(None),
        Err(e) => return Err(e),
    };
    let (body, last) = out.rsplit_once('\n').unwrap_or(("", out.as_str()));
    let mut fields = last.splitn(10, ' ');
    let m: Vec<u64> = fields
        .by_ref()
        .take(9)
        .filter_map(|v| v.parse().ok())
        .collect();
    if fields.next() != Some(run) {
        return Ok(None);
    }
    let mut screen = Screen {
        text: body.to_string(),
        ..Screen::default()
    };
    if m.len() == 9 {
        screen.cursor = (m[2] == 1).then_some((m[0] as u16, m[1] as u16));
        screen.size = (m[3] as u16, m[4] as u16);
        screen.history = m[5] as usize;
        (screen.alt, screen.mouse, screen.sgr) = (m[6] == 1, m[7] == 1, m[8] == 1);
    }
    Ok(Some(screen))
}

/// `s` as one tmux command argument, for a command inside another command.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// What `while_run` prints when the run changed.
const RUN_CHANGED: &str = "pctl-run-changed";

/// Run the tmux command `then` only while `pane` still holds `run`, else `otherwise`. Check and
/// act are one tmux call, so a restart cannot slip in between. Ok(false): the run changed.
/// `otherwise` is a command with its own leading " ; ", or empty.
fn while_run(core: &Core, pane: &str, run: &str, then: &str, otherwise: &str) -> Result<bool> {
    // The run goes into a format; an id with a format character is not one of ours.
    if !run
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Ok(false);
    }
    let out = core.tm(&[
        "if-shell",
        "-F",
        "-t",
        pane,
        &format!("#{{==:#{{@pctl_run}},{run}}}"),
        then,
        &format!("display-message -p {RUN_CHANGED}{otherwise}"),
    ])?;
    Ok(!out.contains(RUN_CHANGED))
}

/// Type the keys (text to send with `-l`, or key names) into the pane, in one tmux call that also
/// checks the run. Ok(false): the run changed and nothing was typed.
pub fn type_keys(core: &Core, pane: &str, run: &str, keys: &[(bool, String)]) -> Result<bool> {
    if !core.refresh_pane_input(pane, run)? {
        return Ok(false);
    }
    let send: Vec<String> = keys
        .iter()
        .map(|(lit, key)| {
            format!(
                "send-keys -t {pane} {}-- {}",
                if *lit { "-l " } else { "" },
                quote(key)
            )
        })
        .collect();
    while_run(core, pane, run, &send.join(" ; "), "")
}

fn send_keys(core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
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
            Ok((lit, key.to_string()))
        })
        .collect::<Result<_>>()?;
    let pane = pane_of(core, agent, run)?;
    ensure!(
        type_keys(core, &pane, run, &keys)?,
        "changed: pane {pane} no longer holds run {run}; touch nothing"
    );
    Ok(json!({}))
}

/// Paste `text` into the pane as one bracketed paste, checking the run in the call that pastes.
/// Ok(false): the run changed and nothing was pasted.
pub fn paste_text(core: &Core, pane: &str, run: &str, text: &str) -> Result<bool> {
    if !core.refresh_pane_input(pane, run)? {
        return Ok(false);
    }
    // One tmux command carries about 16 KB; a longer paste goes in 8 KB pieces into one buffer,
    // then out as one bracketed paste. One buffer per process: two viewers must not mix their text.
    let buffer = format!("pctl-app-{}", std::process::id());
    let mut r = Ok(String::new());
    for (i, piece) in chunks(text, 8192).into_iter().enumerate() {
        let append = if i == 0 { "-b" } else { "-ab" };
        r = r.and_then(|_| core.tm(&["set-buffer", append, &buffer, "--", &literal(piece)]));
    }
    r?;
    while_run(
        core,
        pane,
        run,
        &format!("paste-buffer -p -d -b {buffer} -t {pane}"),
        &format!(" ; delete-buffer -b {buffer}"),
    )
}

fn paste(core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
    let text = req
        .get("text")
        .and_then(Value::as_str)
        .context("missing text")?;
    let pane = pane_of(core, agent, run)?;
    ensure!(
        paste_text(core, &pane, run, text)?,
        "changed: pane {pane} no longer holds run {run}; touch nothing"
    );
    Ok(json!({}))
}

/// The largest piece of an image one `clip_put` carries, before base64.
pub const CLIP_PIECE: usize = 512 << 10;

/// An image on its way to this computer's clipboard: `clip_put` pieces are appended to a private
/// file, `clip_set` checks it and puts it on the clipboard. One folder per bridge process, made on
/// the first piece and removed with the bridge.
struct Clip {
    dir: PathBuf,
    /// The pieces and bytes received so far; 0 when no image is under way.
    pieces: u64,
    size: usize,
}

impl Clip {
    fn new(core: &Core) -> Self {
        Self {
            dir: core.state.join(format!("clip-{}", std::process::id())),
            pieces: 0,
            size: 0,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("image.png")
    }

    /// Forget the image and delete its file.
    fn drop_image(&mut self) {
        (self.pieces, self.size) = (0, 0);
        let _ = fs::remove_file(self.file());
    }

    /// The pane is the agent's and still holds the run, so the clipboard is not touched for another.
    fn check(core: &Core, agent: &str, run: &str) -> Result<()> {
        let pane = pane_of(core, agent, run)?;
        verified(core, &pane, run)?;
        ensure!(
            core.refresh_pane_input(&pane, run)?,
            "changed: image target run changed"
        );
        Ok(())
    }

    fn put(&mut self, core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
        Self::check(core, agent, run)?;
        let seq = number(req, "seq")?;
        // Piece 0 starts an image over; the others must follow on.
        ensure!(
            seq == 0 || seq == self.pieces,
            "clip_put: piece {seq} came after {} pieces",
            self.pieces
        );
        let data = unbase64(
            req.get("data")
                .and_then(Value::as_str)
                .context("missing data")?,
        )
        .context("clip_put: data is not base64")?;
        ensure!(
            data.len() <= CLIP_PIECE,
            "clip_put: a piece is at most {} KB",
            CLIP_PIECE >> 10
        );
        let size = if seq == 0 { 0 } else { self.size } + data.len();
        ensure!(
            size <= MAX_IMAGE,
            "clip_put: the image is over {} MB",
            MAX_IMAGE >> 20
        );
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let mut file = OpenOptions::new();
        file.create(true).mode(0o600);
        if seq == 0 {
            file.write(true).truncate(true);
        } else {
            file.append(true);
        }
        file.open(self.file())?.write_all(&data)?;
        (self.pieces, self.size) = (seq + 1, size);
        Ok(json!({}))
    }

    fn set(&mut self, core: &Core, agent: &str, run: &str, req: &Value) -> Result<Value> {
        let r = self.set_image(core, agent, run, req);
        self.drop_image();
        r.map(|()| json!({}))
    }

    fn set_image(&self, core: &Core, agent: &str, run: &str, req: &Value) -> Result<()> {
        Self::check(core, agent, run)?;
        ensure!(
            req.get("kind").and_then(Value::as_str) == Some("png"),
            "clip_set: only png"
        );
        let bytes = number(req, "bytes")?;
        ensure!(
            self.pieces > 0 && bytes <= MAX_IMAGE as u64 && bytes == self.size as u64,
            "clip_set: {} bytes arrived, {bytes} expected",
            self.size
        );
        let mut head = [0; 8];
        fs::File::open(self.file())?.read_exact(&mut head)?;
        ensure!(clipboard::is_png(&head), "clip_set: not a png");
        clipboard::write_image(&self.file())
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `s` in pieces of at most `max` bytes, cut on character boundaries.
pub(crate) fn chunks(s: &str, max: usize) -> Vec<&str> {
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

/// bash, as Ghostty starts it here: `$SHELL` when that is bash, else a Homebrew or system bash.
pub fn bash() -> String {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if std::path::Path::new(&shell)
        .file_name()
        .is_some_and(|n| n == "bash")
    {
        return shell;
    }
    ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or("bash")
        .to_string()
}

/// The agent kinds of the new-agent form.
pub const KINDS: [&str; 5] = ["bash", "Claude Code", "Codex", "OpenCode", "Command"];

/// The command line (and the OpenCode model) of a new agent of `kind`, built on the computer that
/// runs it: `bash` is that computer's bash.
pub fn agent_argv(kind: &str, model: &str, command: &str) -> Result<(Vec<String>, Option<String>)> {
    let model = (!model.is_empty()).then(|| model.to_string());
    let with = |bin: &str, flag: &str| -> Vec<String> {
        std::iter::once(bin.to_string())
            .chain(model.iter().flat_map(|m| [flag.to_string(), m.clone()]))
            .collect()
    };
    let argv = match kind {
        "Claude Code" => with("claude", "--model"),
        "Codex" => with("codex", "-m"),
        "OpenCode" => vec!["opencode".into()],
        "bash" => vec![bash(), "-l".into()],
        _ => crate::parse_command(command)?,
    };
    Ok((argv, model.filter(|_| kind == "OpenCode")))
}

/// One action on an agent or project: the calls the app makes for its own computer, the same
/// ones for another computer's app over the bridge. `action` is restart, stop (with the `run` that
/// was shown), start (`new` for a new conversation), remove, new_tab (a bash tab named `name` in
/// `project`) or new_agent (the new-agent form's fields).
pub fn act(core: &Core, req: &Value) -> Result<Value> {
    let text = |name: &str| -> Result<&str> {
        req.get(name)
            .and_then(Value::as_str)
            .with_context(|| format!("missing {name}"))
    };
    let optional = |name: &str| {
        req.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
    };
    match text("action")? {
        "restart" => {
            core.session_restart(text("agent")?, false, false)?;
        }
        "start" => {
            let new = req.get("new").and_then(Value::as_bool).unwrap_or(false);
            core.session_start(text("agent")?, new)?;
        }
        "remove" => {
            core.session_remove(text("agent")?)?;
        }
        "stop" => {
            let s = core.session_get(text("agent")?)?;
            let id = s["agent_id"].as_str().unwrap_or_default();
            let run = req
                .get("run")
                .and_then(Value::as_str)
                .or(s["run_id"].as_str())
                .unwrap_or_default();
            core.terminate(id, run)?;
        }
        "doc_open" => {
            return Ok(json!({"name": core.doc_open(text("doc")?)?}));
        }
        "doc_close" => core.doc_close(text("agent")?)?,
        "new_tab" => {
            let project = text("project")?;
            let root = core
                .registry()?
                .projects
                .iter()
                .find(|p| p.name == project)
                .map(|p| p.root.clone())
                .unwrap_or_default();
            core.session_add(
                text("name")?,
                std::path::Path::new(&root),
                Some(project),
                vec![bash(), "-l".into()],
                None,
                None,
                None,
            )?;
        }
        "new_agent" => {
            let (argv, opencode_model) =
                agent_argv(optional("kind"), optional("model"), optional("command"))?;
            let branch = optional("branch");
            core.session_add(
                text("name")?,
                std::path::Path::new(text("folder")?),
                Some(text("project")?),
                argv,
                None,
                (!branch.is_empty()).then_some((branch, None)),
                opencode_model.as_deref(),
            )?;
        }
        other => bail!("unknown action {other}"),
    }
    Ok(json!({}))
}

/// The hello line, sent as soon as the bridge starts.
fn hello() -> Value {
    json!({"ev": "hello", "protocol": 1, "host": hostname(), "version": env!("CARGO_PKG_VERSION")})
}

pub(crate) fn hostname() -> String {
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
    /// How long this request may wait; None: the usual `ANSWER_WITHIN`.
    within: Option<Duration>,
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
        current(&self.inner).request(op, None)
    }

    /// Like `request`, for an op that may take longer than 5 s (restarting an agent waits for it).
    pub fn request_within(&self, op: Value, within: Duration) -> mpsc::Receiver<Result<Value>> {
        current(&self.inner).request(op, Some(within))
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

    fn request(&self, op: Value, within: Option<Duration>) -> mpsc::Receiver<Result<Value>> {
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
                within,
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
            .filter(|(_, e)| now.duration_since(e.started) >= e.within.unwrap_or(after))
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
        let first = inner.request(json!({"op": "list"}), None);
        let second = inner.request(json!({"op": "paste", "text": "hi"}), None);
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
        let waiting = inner.request(json!({"op": "list"}), None);
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
        let waiting = inner.request(json!({"op": "list"}), None);
        inner.down("eof");
        assert!(waiting.try_recv().expect("failed").is_err());
        assert!(
            inner
                .request(json!({"op": "list"}), None)
                .try_recv()
                .expect("failed")
                .is_err()
        );
        assert!(matches!(lock(&inner.state).clone(), RemoteState::Down(_)));
    }

    #[test]
    fn a_reconnect_goes_down_a_fresh_connection_and_fails_the_old_one() {
        let (old, _sent) = bare();
        let waiting = old.request(json!({"op": "list"}), None);
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

    #[test]
    fn a_new_agent_command_is_built_where_it_runs() {
        let (argv, model) = agent_argv("Claude Code", "claude-x", "").expect("claude");
        assert_eq!(
            (argv, model),
            (
                vec!["claude".into(), "--model".into(), "claude-x".into()],
                None
            )
        );
        let (argv, _) = agent_argv("Codex", "", "").expect("codex");
        assert_eq!(argv, ["codex"]);
        let (argv, model) = agent_argv("OpenCode", "m1", "").expect("opencode");
        assert_eq!(
            (argv, model.as_deref()),
            (vec!["opencode".into()], Some("m1"))
        );
        let (argv, _) = agent_argv("bash", "", "").expect("bash");
        assert_eq!(argv.last().map(String::as_str), Some("-l"));
        let (argv, model) = agent_argv("Command", "m", "sh -c 'echo hi'").expect("command");
        assert_eq!(
            (argv, model),
            (vec!["sh".into(), "-c".into(), "echo hi".into()], None)
        );
        assert!(
            agent_argv("Command", "", "").is_err(),
            "an empty command is refused"
        );
    }

    #[test]
    fn a_screen_answer_round_trips_into_a_screen() {
        let s = Screen {
            text: "a\nb".into(),
            cursor: Some((3, 1)),
            size: (80, 24),
            history: 7,
            alt: true,
            mouse: true,
            sgr: true,
        };
        let answer = json!({
            "rows": s.text, "history": s.history,
            "cursor": [3, 1], "size": [80, 24], "alt": true, "mouse": true, "sgr": true,
        });
        assert_eq!(Screen::from_answer(&answer), s);
        let hidden = json!({"rows": "", "history": 0, "cursor": null, "size": [1, 1]});
        assert_eq!(Screen::from_answer(&hidden).cursor, None);
    }
}
