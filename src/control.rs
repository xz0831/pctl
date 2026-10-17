//! Screens over tmux control mode. One `tmux -C` client per tmux session (tmux sends a client the
//! output of the panes of its own session only) tells us when a pane wrote (`%output`), and the
//! same connection answers `capture-pane` without starting a process. The bytes of `%output` are
//! not looked at: a pane that wrote is paused (what it writes meanwhile is dropped, so nothing
//! piles up in tmux or here), and the next read continues it. Nothing here is a viewer: the client
//! is `ignore-size`, and `Core::clients`, the refresh checks and `bridge::resize` leave control
//! clients out. If a client cannot start or dies, callers read with a process per read as before
//! and this tries again later.
use crate::{
    Core,
    bridge::{CURSOR_FORMAT, Screen, screen_from},
};
use anyhow::{Context, Result};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// A reply that does not come in this long means the connection is stuck.
const REPLY_WITHIN: Duration = Duration::from_secs(2);
/// After a client fails, wait this long before starting another.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// A client whose session has had no read for this long is detached: clients stay as many as the
/// sessions with a cell on screen.
const IDLE_AFTER: Duration = Duration::from_secs(30);
const LOG_CAP: u64 = 1 << 16;

/// Seconds from the environment, for tests; else `default`.
fn env_secs(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(default, Duration::from_secs)
}
fn idle_after() -> Duration {
    env_secs("PCTL_CONTROL_IDLE_SECS", IDLE_AFTER)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the parser takes out of the stream.
#[derive(Debug, PartialEq)]
pub enum Item {
    /// A pane wrote.
    Output(String),
    /// The answer to one command of ours: whether it worked, and what it printed.
    Reply { ok: bool, body: String },
    /// The client is over.
    Exit,
}

/// Reads a control-mode stream line by line. A block between `%begin T N F` and the `%end`/`%error`
/// with the same `T N` is one reply, whatever its lines look like (a screen row can start with
/// `%`). Blocks with flags 0 are tmux's own (the attach), not answers to us.
#[derive(Default)]
pub struct Parser {
    block: Option<Block>,
}
struct Block {
    id: String,
    ours: bool,
    lines: Vec<String>,
}

impl Parser {
    pub fn feed(&mut self, line: &str) -> Option<Item> {
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some(b) = &mut self.block {
            let end = line
                .strip_prefix("%end ")
                .map(|r| (true, r))
                .or_else(|| line.strip_prefix("%error ").map(|r| (false, r)));
            if let Some((ok, rest)) = end
                && rest.split(' ').take(2).collect::<Vec<_>>().join(" ") == b.id
            {
                let b = self.block.take()?;
                return b.ours.then(|| Item::Reply {
                    ok,
                    body: b.lines.join("\n"),
                });
            }
            b.lines.push(line.to_string());
            return None;
        }
        if let Some(rest) = line.strip_prefix("%begin ") {
            let f: Vec<&str> = rest.split(' ').collect();
            self.block = Some(Block {
                id: f.iter().take(2).copied().collect::<Vec<_>>().join(" "),
                ours: f.get(2) == Some(&"1"),
                lines: vec![],
            });
            return None;
        }
        if let Some(rest) = line
            .strip_prefix("%output ")
            .or_else(|| line.strip_prefix("%extended-output "))
        {
            let pane = rest.split(' ').next().unwrap_or_default();
            return (!pane.is_empty()).then(|| Item::Output(pane.to_string()));
        }
        (line == "%exit" || line.starts_with("%exit ")).then_some(Item::Exit)
    }
}

/// A command waiting for its reply. A command that runs others (`if-shell`) is followed by a
/// `display-message` of `until`: tmux answers each command it runs, so the replies up to that one
/// are all this command's.
struct Waiting {
    tx: mpsc::Sender<(bool, String)>,
    until: Option<String>,
    ok: bool,
    body: Vec<String>,
}
/// What is told that a pane wrote.
type Dirty = Arc<dyn Fn(&str) + Send + Sync>;

/// One `tmux -C` process attached to one session.
struct Client {
    session: String,
    input: Mutex<(ChildStdin, VecDeque<Waiting>)>,
    child: Mutex<Option<Child>>,
    alive: AtomicBool,
    ready: AtomicBool,
    used: Mutex<Instant>,
}

impl Client {
    /// Send one command; its reply comes on the returned channel (which closes if the client dies).
    fn send(&self, command: &str) -> Option<mpsc::Receiver<(bool, String)>> {
        self.send_until(command, None)
    }

    /// Like `send`, for a command that runs others: with `until`, the replies come together, up
    /// to the one of the `display-message` that is sent after it.
    fn send_until(
        &self,
        command: &str,
        until: Option<String>,
    ) -> Option<mpsc::Receiver<(bool, String)>> {
        let (tx, rx) = mpsc::channel();
        let mut input = lock(&self.input);
        if !self.alive.load(Ordering::Relaxed) {
            return None;
        }
        let text = match &until {
            Some(mark) => format!("{command}\ndisplay-message -p {mark}"),
            None => command.to_string(),
        };
        if writeln!(input.0, "{text}")
            .and_then(|_| input.0.flush())
            .is_err()
        {
            drop(input);
            self.kill();
            return None;
        }
        input.1.push_back(Waiting {
            tx,
            until,
            ok: true,
            body: vec![],
        });
        Some(rx)
    }

    fn kill(&self) {
        self.alive.store(false, Ordering::Relaxed);
        lock(&self.input).1.clear();
        if let Some(mut c) = lock(&self.child).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The control connections of one tmux server, started when a pane is first asked about.
pub struct Control {
    core: Core,
    clients: Mutex<HashMap<String, Arc<Client>>>,
    /// Which session each pane is in, from `list-panes`; read again for a pane not in it.
    sessions: Mutex<HashMap<String, String>>,
    /// Panes that wrote and were paused, until the next read of them.
    paused: Mutex<HashSet<String>>,
    /// Panes somebody reads: only their output is reported.
    watched: Mutex<HashSet<String>>,
    on_dirty: Mutex<Option<Dirty>>,
    /// No new client before this.
    wait_until: Mutex<Option<Instant>>,
    marks: AtomicU64,
}

impl Control {
    pub fn new(core: &Core) -> Arc<Control> {
        Arc::new(Control {
            core: core.clone(),
            clients: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            paused: Mutex::new(HashSet::new()),
            watched: Mutex::new(HashSet::new()),
            on_dirty: Mutex::new(None),
            wait_until: Mutex::new(None),
            marks: AtomicU64::new(0),
        })
    }

    /// Call `f(pane)` (from the reader thread: keep it short) when a watched pane wrote.
    pub fn on_dirty(&self, f: Dirty) {
        *lock(&self.on_dirty) = Some(f);
    }

    /// The panes whose output is reported; the others are paused when they first write.
    pub fn watch(&self, panes: &[String]) {
        *lock(&self.watched) = panes.iter().cloned().collect();
    }

    /// Whether the output of this pane reaches us, so it need not be polled. Starts the client of
    /// its session when there is none.
    pub fn covers(self: &Arc<Self>, pane: &str) -> bool {
        self.client_for(pane).is_some()
    }

    fn session_of(&self, pane: &str) -> Option<String> {
        if let Some(s) = lock(&self.sessions).get(pane) {
            return Some(s.clone());
        }
        let rows = self
            .core
            .tm(&["list-panes", "-a", "-F", "#{pane_id} #{session_id}"])
            .ok()?;
        let map: HashMap<String, String> = rows
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .collect();
        let found = map.get(pane).cloned();
        *lock(&self.sessions) = map;
        found
    }

    /// The ready client of the pane's session. None while it starts, or when it cannot.
    fn client_for(self: &Arc<Self>, pane: &str) -> Option<Arc<Client>> {
        let session = self.session_of(pane)?;
        let mut clients = lock(&self.clients);
        if let Some(c) = clients.get(&session) {
            if c.alive.load(Ordering::Relaxed) {
                *lock(&c.used) = Instant::now();
                return c.ready.load(Ordering::Relaxed).then(|| c.clone());
            }
            clients.remove(&session);
        }
        if lock(&self.wait_until).is_some_and(|t| Instant::now() < t) {
            return None;
        }
        match self.start(&session) {
            Ok(c) => {
                clients.insert(session, c);
            }
            Err(e) => self.failed(&format!("cannot start for {session}: {e:#}")),
        }
        None
    }

    fn failed(&self, why: &str) {
        let wait = env_secs("PCTL_CONTROL_RETRY_SECS", RETRY_AFTER);
        *lock(&self.wait_until) = Some(Instant::now() + wait);
        crate::append_capped(
            &self.core.state.join("tmux-control.log"),
            &format!("{}\t{why}", crate::utc_now()),
            LOG_CAP,
        );
    }

    fn start(self: &Arc<Self>, session: &str) -> Result<Arc<Client>> {
        let mut cmd = self.core.tmux_cmd();
        let mut child = cmd
            .args(["-C", "attach", "-t", session, "-f", "ignore-size"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("cannot run tmux")?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let client = Arc::new(Client {
            session: session.to_string(),
            input: Mutex::new((stdin, VecDeque::new())),
            child: Mutex::new(Some(child)),
            alive: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            used: Mutex::new(Instant::now()),
        });
        let (me, c) = (self.clone(), client.clone());
        thread::spawn(move || me.reader(&c, stdout));
        // The first answer says the attach worked.
        let probe = client
            .send("display-message -p ok")
            .context("the connection closed at once")?;
        let (me, c) = (self.clone(), client.clone());
        thread::spawn(move || match probe.recv_timeout(REPLY_WITHIN) {
            Ok((true, _)) => c.ready.store(true, Ordering::Relaxed),
            _ => {
                c.kill();
                me.failed(&format!("no answer from the client of {}", c.session));
            }
        });
        Ok(client)
    }

    fn reader(&self, client: &Client, stdout: std::process::ChildStdout) {
        let mut input = BufReader::new(stdout);
        let mut parser = Parser::default();
        let mut raw = vec![];
        loop {
            raw.clear();
            match input.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            match parser.feed(&String::from_utf8_lossy(&raw)) {
                Some(Item::Output(pane)) => self.output(client, &pane),
                Some(Item::Reply { ok, body }) => {
                    let mut input = lock(&client.input);
                    let done = match input.1.front_mut() {
                        Some(w) if w.until.as_deref().is_some_and(|m| body.trim() != m) => {
                            w.ok &= ok;
                            w.body.push(body);
                            None
                        }
                        Some(w) => {
                            let ok = w.ok && ok;
                            let body = if w.until.is_some() {
                                w.body.join("\n")
                            } else {
                                body
                            };
                            Some((ok, body))
                        }
                        None => None,
                    };
                    if let Some(answer) = done
                        && let Some(w) = input.1.pop_front()
                    {
                        let _ = w.tx.send(answer);
                    }
                }
                Some(Item::Exit) => break,
                None => {}
            }
        }
        client.kill();
        // A session that was closed ends its client without anything being wrong.
        let gone = self
            .core
            .tm(&["list-sessions", "-F", "#{session_id}"])
            .is_ok_and(|rows| !rows.lines().any(|s| s == client.session));
        if !gone {
            self.failed(&format!("the client of {} ended", client.session));
        }
    }

    /// A pane wrote: pause it (what it writes until the next read is dropped), and tell the
    /// watcher if it reads this pane.
    fn output(&self, client: &Client, pane: &str) {
        if !lock(&self.paused).insert(pane.to_string()) {
            return;
        }
        let _ = client.send(&format!("refresh-client -A '{pane}:pause'"));
        if lock(&self.watched).contains(pane) {
            let f = lock(&self.on_dirty).clone();
            if let Some(f) = f {
                f(pane);
            }
        }
    }

    /// The screens of live panes, read over the connection: `(pane, run, height)` in, one answer
    /// each out, as `bridge::read_screens` does. None when the panes have no ready client; read
    /// them the other way then.
    pub fn read_screens(
        self: &Arc<Self>,
        cells: &[(&str, &str, u16)],
    ) -> Option<Vec<Result<Option<Screen>>>> {
        let client = self.client_for(cells.first()?.0)?;
        let mut waits = vec![];
        for (pane, ..) in cells {
            if lock(&self.paused).remove(*pane) {
                client.send(&format!("refresh-client -A '{pane}:continue'"))?;
            }
            let capture = client.send(&format!("capture-pane -p -e -N -t {pane}"))?;
            let fields = client.send(&format!(
                "display-message -p -t {pane} '{}'",
                CURSOR_FORMAT.replace('\'', "")
            ))?;
            waits.push((capture, fields));
        }
        let deadline = Instant::now() + REPLY_WITHIN;
        let wait = |rx: &mpsc::Receiver<(bool, String)>| {
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        };
        let mut out = vec![];
        for ((_, run, _), (capture, fields)) in cells.iter().zip(waits) {
            let (Ok(capture), Ok(fields)) = (wait(&capture), wait(&fields)) else {
                client.kill();
                self.failed("no reply to a read");
                return None;
            };
            out.push(match (capture, fields) {
                ((true, body), (true, last)) => Ok(screen_from(&body, &last, run)),
                ((true, _), (false, why)) | ((false, why), _) if why.contains("can't find") => {
                    Ok(None)
                }
                ((true, _), (false, why)) | ((false, why), _) => {
                    Err(anyhow::anyhow!("tmux: {why}"))
                }
            });
        }
        Some(out)
    }

    /// Run one tmux command line over the connection of the pane's session. None: there is no
    /// ready client, nothing was sent, and the caller runs it itself. After it was sent the answer
    /// is the command's own (an error when none came: whether it ran is then unknown).
    pub fn command(self: &Arc<Self>, pane: &str, line: &str) -> Option<Result<String>> {
        let client = self.client_for(pane)?;
        let mark = format!("pctl-end-{}", self.marks.fetch_add(1, Ordering::Relaxed));
        let rx = client.send_until(line, Some(mark))?;
        Some(match rx.recv_timeout(REPLY_WITHIN) {
            Ok((true, body)) => Ok(body),
            Ok((false, why)) => Err(anyhow::anyhow!("tmux: {why}")),
            Err(_) => {
                client.kill();
                self.failed("no reply to a command");
                Err(anyhow::anyhow!(
                    "outcome_unknown: no reply from tmux; do not repeat it"
                ))
            }
        })
    }

    /// Detach the clients of sessions nobody has read for a while.
    pub fn sweep(&self) {
        let mut clients = lock(&self.clients);
        clients.retain(|_, c| {
            let keep = lock(&c.used).elapsed() < idle_after();
            if !keep {
                c.kill();
            }
            keep
        });
    }

    /// How many clients are attached.
    pub fn clients(&self) -> usize {
        lock(&self.clients)
            .values()
            .filter(|c| c.alive.load(Ordering::Relaxed))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> Vec<Item> {
        let mut p = Parser::default();
        lines.iter().filter_map(|l| p.feed(l)).collect()
    }

    #[test]
    fn a_reply_block_is_one_reply_and_tmuxs_own_block_is_none() {
        let items = feed(&[
            "%begin 1700000000 1 0",
            "%end 1700000000 1 0",
            "%session-changed $0 s",
            "%begin 1700000000 2 1",
            "row one",
            "",
            "%end 1700000000 2 1",
        ]);
        assert_eq!(
            items,
            [Item::Reply {
                ok: true,
                body: "row one\n".into()
            }]
        );
    }

    #[test]
    fn an_error_block_is_a_failed_reply() {
        let items = feed(&["%begin 1 5 1", "can't find pane: %9", "%error 1 5 1"]);
        assert_eq!(
            items,
            [Item::Reply {
                ok: false,
                body: "can't find pane: %9".into()
            }]
        );
    }

    #[test]
    fn output_between_the_lines_of_a_block_is_text_and_after_it_a_notification() {
        // A screen row can look like anything, a notification or the end of another block.
        let items = feed(&[
            "%begin 1 7 1",
            "%output %1 not a notification",
            "%end 1 8 1",
            "%error 2 7 1",
            "%end 1 7 1",
            "%output %3 \\033[1mhi\\015\\012",
            "%extended-output %4 0 : x",
            "%exit",
        ]);
        assert_eq!(
            items,
            [
                Item::Reply {
                    ok: true,
                    body: "%output %1 not a notification\n%end 1 8 1\n%error 2 7 1".into()
                },
                Item::Output("%3".into()),
                Item::Output("%4".into()),
                Item::Exit,
            ]
        );
    }

    #[test]
    fn other_notifications_and_lines_are_ignored() {
        assert!(
            feed(&[
                "%window-renamed @0 bash",
                "%layout-change @0 b67d,100x24,0,0,0",
                "%pause %2",
                "stray text",
                "%continue %2",
            ])
            .is_empty()
        );
    }

    #[test]
    fn crlf_and_a_trailing_newline_do_not_matter() {
        let items = feed(&["%begin 1 9 1\r\n", "ok\r\n", "%end 1 9 1\r\n"]);
        assert_eq!(
            items,
            [Item::Reply {
                ok: true,
                body: "ok".into()
            }]
        );
    }
}
