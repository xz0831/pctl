//! Name-addressed session commands:
//! list/get/prompt/wait/read/whoami, harness conversation IDs kept across restarts,
//! and restore after the tmux server is gone (reboot).
use crate::{Agent, Core, Registry, Run, ensure_terminal_input, now};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};
use uuid::Uuid;

/// The harness a profile runs, from its executable name.
pub fn harness(argv: &[String]) -> &'static str {
    match argv
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|n| n.to_str())
    {
        Some("claude") => "claude",
        Some("codex") => "codex",
        _ => "",
    }
}
/// Command line for a new run. Claude gets a fixed session ID on first start and resumes it
/// afterwards; Codex resumes its discovered thread. Profiles that already pick a conversation
/// themselves are left alone.
pub(crate) fn launch_argv(r: &mut Registry, agent: &str) -> Result<Vec<String>> {
    let a = r
        .agents
        .iter_mut()
        .find(|a| a.id == agent)
        .context("agent not found")?;
    let mut argv = a.argv.clone();
    let chooses = |flags: &[&str]| argv.iter().any(|x| flags.contains(&x.as_str()));
    match harness(&argv) {
        "claude" if !chooses(&["-r", "--resume", "-c", "--continue", "--session-id"]) => {
            match &a.conversation {
                Some(c) => argv.extend(["--resume".into(), c.clone()]),
                None => {
                    let c = Uuid::new_v4().to_string();
                    argv.extend(["--session-id".into(), c.clone()]);
                    a.conversation = Some(c);
                }
            }
        }
        "codex" if argv.get(1).map(String::as_str) != Some("resume") => {
            if let Some(c) = &a.conversation {
                argv.splice(1..1, ["resume".to_string(), c.clone()]);
            }
        }
        _ => {}
    }
    Ok(argv)
}
/// Whether starting this profile again would land in a fresh conversation by accident: a
/// Claude/Codex profile whose conversation PCTL does not know and that does not pick one itself.
fn conversation_unknown(a: &Agent) -> bool {
    let picks = a.argv.iter().any(|x| {
        [
            "-r",
            "--resume",
            "-c",
            "--continue",
            "--session-id",
            "resume",
        ]
        .contains(&x.as_str())
    });
    !harness(&a.argv).is_empty() && a.conversation.is_none() && !picks
}
/// Fallback until the SessionStart hook reports: the rollout Codex created for this run (same
/// cwd, created after the run started, not owned by another profile). Only an unambiguous
/// single candidate counts; otherwise the conversation stays unknown.
fn discover_codex(r: &Registry, a: &Agent, run: &Run, root: &Path) -> Option<String> {
    let since = run.started_at.saturating_sub(5);
    let taken: Vec<_> = r
        .agents
        .iter()
        .filter(|o| o.id != a.id)
        .filter_map(|o| o.conversation.as_deref())
        .collect();
    let mut found = vec![];
    for path in recent_files(root, since, 3) {
        let created = fs::metadata(&path)
            .and_then(|m| m.created())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if created < since {
            continue;
        }
        let mut line = String::new();
        let Ok(f) = fs::File::open(&path) else {
            continue;
        };
        // session_meta carries instructions and can be large; the first line is enough.
        if BufReader::new(f.take(4 << 20))
            .read_line(&mut line)
            .is_err()
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(cwd)) = (v["payload"]["id"].as_str(), v["payload"]["cwd"].as_str())
        else {
            continue;
        };
        if v["type"] == "session_meta" && cwd == a.cwd && !taken.contains(&id) {
            found.push(id.to_string());
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}
/// Files under `root` (YYYY/MM/DD layout) modified at or after `since`.
fn recent_files(root: &Path, since: u64, depth: usize) -> Vec<PathBuf> {
    let modified = |p: &Path| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let Ok(entries) = fs::read_dir(root) else {
        return vec![];
    };
    let mut out = vec![];
    for e in entries.flatten() {
        let p = e.path();
        // Only a file's own mtime counts: adding a day folder does not touch the year folder.
        if depth > 0 && p.is_dir() {
            out.extend(recent_files(&p, since, depth - 1));
        } else if depth == 0 && p.extension().is_some_and(|x| x == "jsonl") && modified(&p) >= since
        {
            out.push(p);
        }
    }
    out
}
/// Drop ANSI CSI sequences (colors) from a captured line.
fn plain(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
/// Screen-derived activity from a colored capture (`capture-pane -e`).
/// Working signs seen on real sessions (2026-10-15): Codex "• Working (6s • esc to interrupt)"
/// and a braille spinner in the pane title; Claude Code 2.1 a spinner line ("✢ Vibing…") before
/// text streams, and a dimmed input prompt "❯" for the whole turn (default color when idle).
// shortcut: the dimmed-prompt cue is Claude Code's rendering, not an API; if a Claude update
// breaks it, report state from UserPromptSubmit/Stop hooks the way `session hook` does.
#[derive(serde::Serialize)]
struct Activity {
    status: &'static str,
    rule: String,
    row: Option<usize>,
    text: Option<String>,
    considered_rows: Vec<ScreenRow>,
    pane_title: String,
}
#[derive(serde::Serialize)]
struct ScreenRow {
    row: usize,
    text: String,
}
fn activity(raw: &str, title: &str) -> &'static str {
    activity_reason(raw, title).status
}
fn activity_reason(raw: &str, title: &str) -> Activity {
    let rows: Vec<_> = raw
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l, plain(l).trim().to_lowercase()))
        .filter(|(_, _, l)| !l.is_empty())
        .collect();
    let tail = &rows[rows.len().saturating_sub(12)..];
    let decide = |status, rule: String, matched: Option<usize>| Activity {
        status,
        rule,
        row: matched.map(|i| tail[i].0),
        text: matched.map(|i| plain(tail[i].1)),
        considered_rows: tail
            .iter()
            .map(|(row, raw, _)| ScreenRow {
                row: *row,
                text: plain(raw),
            })
            .collect(),
        pane_title: title.into(),
    };
    if title
        .chars()
        .next()
        .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c) || "◐◓◑◒".contains(c))
    {
        return decide("working", "spinner in pane title".into(), None);
    }
    for (i, (_, _, l)) in tail.iter().enumerate() {
        if l.contains("esc to interrupt") {
            return decide("working", "footer \"esc to interrupt\"".into(), Some(i));
        }
        if l.starts_with(['✢', '✳', '✶', '✻', '✽', '*', '·']) && l.contains('…') {
            return decide("working", "spinner".into(), Some(i));
        }
    }
    let input = tail
        .iter()
        .rposition(|(_, _, l)| l.starts_with(['❯', '›']) && !menu_row(l));
    let below = input.map_or(0, |i| i + 1);
    // Without an input row, prose needs dialog controls before question words count.
    let dialog = tail[below..].iter().any(|(_, _, l)| {
        menu_row(l)
            || l.contains("enter to confirm")
            || l.contains("esc to cancel")
            || l.contains("enter confirm")
            || (l.contains("allow once") && l.contains("reject"))
    });
    for (i, (_, _, l)) in tail.iter().enumerate().skip(below) {
        if menu_row(l) {
            return decide("blocked", "numbered menu".into(), Some(i));
        }
        if (input.is_some() || dialog)
            && let Some(word) = [
                "do you want to",
                "would you like to",
                "allow command",
                "approve",
                "enter to confirm",
                "esc to cancel",
                "trust this folder",
                "permission required",
                "enter confirm",
            ]
            .iter()
            .find(|word| l.contains(**word))
        {
            let scope = if input.is_some() {
                "below the last input row"
            } else {
                "in dialog"
            };
            return decide(
                "blocked",
                format!("question word {word:?} {scope}"),
                Some(i),
            );
        }
    }
    for (i, (_, _, l)) in tail.iter().enumerate().skip(tail.len().saturating_sub(4)) {
        if l.contains("esc interrupt") && (l.contains(['⬝', '■']) || l.contains("ctrl+p commands"))
        {
            return decide("working", "opencode footer".into(), Some(i));
        }
    }
    if let Some(i) = tail
        .iter()
        .rposition(|(_, _, l)| l.starts_with('❯') && !menu_row(l))
    {
        let raw = tail[i].1;
        if raw.split('❯').next().is_some_and(|before| {
            before
                .rfind("\x1b[")
                .is_some_and(|j| !before[j..].starts_with("\x1b[39m"))
        }) {
            return decide("working", "dim prompt".into(), Some(i));
        }
    }
    decide("idle", "idle: no rule matched".into(), None)
}
/// A numbered choice under the cursor ("› 1. Update now", "❯ 2. No") waits for a person: Codex
/// update and trust menus carry none of the question words, and Enter would pick the choice.
fn menu_row(l: &str) -> bool {
    l.strip_prefix(['›', '❯']).is_some_and(|r| {
        let r = r.trim_start();
        let digits = r.len() - r.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        digits > 0 && r[digits..].starts_with('.')
    })
}
fn codex_root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex/sessions")
}
/// The named keys `session keys` may press. A closed set: free text is `session prompt`'s job.
const KEYS: [&str; 10] = [
    "Escape", "Enter", "Tab", "BTab", "Up", "Down", "Left", "Right", "BSpace", "Space",
];
/// The tmux key name for an allowed key, or None for anything else. tmux's own names for these
/// keys are the names above; a single digit, `y` or `n` is sent as that one character.
fn key_name(key: &str) -> Option<&str> {
    let single = key.len() == 1 && (key.as_bytes()[0].is_ascii_digit() || key == "y" || key == "n");
    (KEYS.contains(&key) || single).then_some(key)
}
fn allowed_keys() -> String {
    format!("{}, 0-9, y, n", KEYS.join(", "))
}

impl Core {
    pub(crate) fn by_name<'a>(&self, r: &'a Registry, name: &str) -> Result<&'a Agent> {
        let found: Vec<_> = r.agents.iter().filter(|a| a.name == name).collect();
        match found[..] {
            [a] => Ok(a),
            [] => bail!("not_found: no session named {name}"),
            _ => bail!(
                "ambiguous: {} sessions are named {name}; use agent IDs",
                found.len()
            ),
        }
    }
    /// The live pane of an agent's current run.
    fn pane_of<'a>(
        r: &Registry,
        a: &Agent,
        panes: Option<&'a [crate::Pane]>,
    ) -> Option<&'a crate::Pane> {
        let run = r.runs.iter().find(|x| x.agent_id == a.id)?;
        panes?
            .iter()
            .find(|p| p.pane == run.pane && p.run == run.id)
    }
    /// What each of `panes` is doing, read with one tmux call per 16 panes instead of two per pane.
    /// tmux stops a command list at its first error, so a pane that is gone makes the call fail or
    /// come back short; those panes are left out and the caller asks for them one by one.
    fn activities(&self, panes: &[&str]) -> HashMap<String, &'static str> {
        let mut known = HashMap::new();
        for chunk in panes.chunks(16) {
            // Each pane's screen is followed by a line only this call can write: a screen that
            // prints something alike cannot end its own part early or start another's.
            let mark = Uuid::new_v4().simple().to_string();
            // The pane comes from tmux's own format: a literal "%1" would be read as a strftime code.
            let line = format!("{mark} #{{pane_id}} #{{pane_title}} {mark}");
            let mut args: Vec<String> = vec![];
            for p in chunk {
                let p = p.to_string();
                args.extend(["capture-pane", "-p", "-e", "-t", &p, ";"].map(String::from));
                args.extend(["display-message", "-p", "-t", &p, &line, ";"].map(String::from));
            }
            args.pop();
            let Ok(out) = self.tm(&args.iter().map(String::as_str).collect::<Vec<_>>()) else {
                continue;
            };
            let (mut got, mut screen) = (vec![], String::new());
            for l in out.split('\n') {
                let Some(rest) = l.strip_prefix(&format!("{mark} ")) else {
                    screen.push_str(l);
                    screen.push('\n');
                    continue;
                };
                let Some((pane, title)) = rest
                    .strip_suffix(&format!(" {mark}"))
                    .and_then(|m| m.split_once(' '))
                else {
                    break;
                };
                got.push((pane, activity(screen.trim_end_matches('\n'), title)));
                screen.clear();
            }
            if got.len() == chunk.len() && got.iter().zip(chunk).all(|((p, _), c)| p == c) {
                known.extend(got.into_iter().map(|(p, a)| (p.to_string(), a)));
            }
        }
        known
    }
    fn view(
        &self,
        r: &Registry,
        a: &Agent,
        panes: Option<&[crate::Pane]>,
        known: &HashMap<String, &'static str>,
    ) -> Value {
        let run = r.runs.iter().find(|x| x.agent_id == a.id);
        let pane = Self::pane_of(r, a, panes);
        let (process, status) = match (run, pane) {
            (None, _) => ("stopped", "stopped"),
            (Some(_), None) => ("lost", "lost"),
            (Some(_), Some(p)) if p.dead => ("exited", "exited"),
            (Some(_), Some(p)) => (
                "running",
                known.get(&p.pane).copied().unwrap_or_else(|| {
                    self.tm(&["capture-pane", "-p", "-e", "-t", &p.pane])
                        .map_or("unknown", |s| {
                            let title = self
                                .tm(&["display-message", "-p", "-t", &p.pane, "#{pane_title}"])
                                .unwrap_or_default();
                            activity(&s, &title)
                        })
                }),
            ),
        };
        let project = r.projects.iter().find(|p| p.id == a.project_id);
        json!({
            "name": a.name, "agent_id": a.id, "project": project.map(|p| &p.name),
            "cwd": a.cwd, "harness": harness(&a.argv), "conversation": a.conversation,
            "run_id": run.map(|x| &x.id), "pane": run.map(|x| &x.pane),
            "branch": a.worktree.as_ref().map(|w| &w.branch),
            "process": process, "status": status,
        })
    }
    /// Live panes, or None when no server owns this state (after a reboot).
    fn live_panes(&self, r: &Registry) -> Option<Vec<crate::Pane>> {
        (self.server().ok()? == r.server_instance)
            .then(|| self.panes().ok())
            .flatten()
    }
    /// Fill in Codex thread IDs for running profiles that have none yet.
    fn discover(&self) -> Result<Registry> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        let root = codex_root();
        let mut changed = false;
        for i in 0..r.agents.len() {
            let a = &r.agents[i];
            if a.conversation.is_some() || harness(&a.argv) != "codex" {
                continue;
            }
            let Some(run) = r.runs.iter().find(|x| x.agent_id == a.id) else {
                continue;
            };
            if let Some(id) = discover_codex(&r, a, run, &root) {
                r.agents[i].conversation = Some(id);
                changed = true;
            }
        }
        if changed {
            self.save(&r)?;
        }
        Ok(r)
    }
    pub fn session_list(&self) -> Result<Vec<Value>> {
        let r = self.discover()?;
        let panes = self.live_panes(&r);
        let reading: Vec<&str> = r
            .agents
            .iter()
            .filter_map(|a| Self::pane_of(&r, a, panes.as_deref()))
            .filter(|p| !p.dead)
            .map(|p| p.pane.as_str())
            .collect();
        let known = self.activities(&reading);
        Ok(r.agents
            .iter()
            .map(|a| self.view(&r, a, panes.as_deref(), &known))
            .collect())
    }
    pub fn session_get(&self, name: &str) -> Result<Value> {
        let r = self.discover()?;
        let a = self.by_name(&r, name)?;
        Ok(self.view(&r, a, self.live_panes(&r).as_deref(), &HashMap::new()))
    }
    pub fn session_explain(&self, name: &str) -> Result<Value> {
        let r = self.discover()?;
        let a = self.by_name(&r, name)?;
        let panes = self.live_panes(&r);
        let pane = Self::pane_of(&r, a, panes.as_deref());
        let reason = match (r.runs.iter().any(|x| x.agent_id == a.id), pane) {
            (false, _) => "stopped: no run",
            (true, None) => "lost: no matching live pane",
            (_, Some(p)) if p.dead => "exited: pane is dead",
            (_, Some(p)) => {
                let title = self
                    .tm(&["display-message", "-p", "-t", &p.pane, "#{pane_title}"])
                    .unwrap_or_default();
                return match self.tm(&["capture-pane", "-p", "-e", "-t", &p.pane]) {
                    Ok(raw) => Ok(serde_json::to_value(activity_reason(&raw, &title))?),
                    Err(e) => Ok(
                        json!({"status": "unknown", "rule": format!("capture failed: {e}"),
                        "row": null, "text": null, "considered_rows": [], "pane_title": title}),
                    ),
                };
            }
        };
        Ok(json!({"status": reason.split(':').next(), "rule": reason,
            "row": null, "text": null, "considered_rows": [], "pane_title": null}))
    }
    fn current(&self, name: &str) -> Result<(Agent, Run)> {
        let r = self.registry()?;
        let a = self.by_name(&r, name)?.clone();
        let run = r
            .runs
            .iter()
            .find(|x| x.agent_id == a.id)
            .cloned()
            .with_context(|| format!("stopped: {name} has no run"))?;
        Ok((a, run))
    }
    /// Paste text (bracketed, so newlines stay inside one prompt) and press Enter.
    pub fn session_prompt(&self, name: &str, text: &str) -> Result<Value> {
        ensure!(!text.trim().is_empty(), "empty prompt");
        ensure!(
            text.len() <= 65536,
            "prompt exceeds 64 KiB; send a file path instead"
        );
        ensure!(!text.contains('\0'), "prompt contains NUL");
        let before = self.session_get(name)?["status"].clone();
        ensure!(
            before != "blocked",
            "blocked: {name} is waiting for a human decision; not typing into it"
        );
        let (a, run) = self.current(name)?;
        {
            let _lock = self.lock()?;
            let r = self.registry()?;
            ensure_terminal_input(&r, &a.id)?;
            self.deliver_text(&r, &a.id, &run.id, text.as_bytes(), "session_prompt", true)?;
        }
        // Codex treats an Enter right after a paste burst as a newline; wait it out.
        std::thread::sleep(Duration::from_millis(500));
        self.submit_line(&a.id, &run.id)?;
        Ok(
            json!({"name": name, "run_id": run.id, "status_before": before, "result": "delivered; acceptance unverified"}),
        )
    }
    /// Press named keys in the session's pane, in order. A blocked session is answered too: a
    /// deliberate key on a dialog is what this command is for, unlike pasted text.
    pub fn session_keys(&self, name: &str, keys: &[String]) -> Result<Value> {
        ensure!(!keys.is_empty(), "no keys given");
        ensure!(keys.len() <= 32, "at most 32 keys per call");
        let mut send = Vec::with_capacity(keys.len());
        for key in keys {
            let Some(named) = key_name(key) else {
                bail!("unsupported key {key:?}; allowed: {}", allowed_keys());
            };
            send.push(named);
        }
        let before = self.session_get(name)?["status"].clone();
        let (a, run) = self.current(name)?;
        {
            let _lock = self.lock()?;
            let r = self.registry()?;
            ensure_terminal_input(&r, &a.id)?;
            let pane = self.guard(&r, &a.id, &run.id, true)?;
            ensure!(!pane.in_mode, "pane is in copy/other mode");
            ensure!(
                !pane.synchronized,
                "synchronize-panes is enabled; input rejected"
            );
            let mut args = vec!["send-keys", "-t", &pane.pane];
            args.extend(send.iter().copied());
            match self.tm(&args) {
                Ok(_) => self
                    .audit(
                        "session_keys",
                        &run.id,
                        &format!("keys sent: {}", send.join(" ")),
                    )
                    .context("keys delivered but audit failed; do not retry"),
                Err(error) => bail!("outcome_unknown; do not retry automatically: {error}"),
            }?;
        }
        Ok(
            json!({"name": name, "run_id": run.id, "status_before": before, "was_blocked": before == "blocked", "keys": keys, "result": "delivered; acceptance unverified"}),
        )
    }
    /// Poll until the status is one of `until` (default: not working), or, with `matched`, until
    /// that text shows on the screen `session_read` returns.
    pub fn session_wait(
        &self,
        name: &str,
        until: &[String],
        timeout: Duration,
        matched: Option<&str>,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut v = self.session_get(name)?;
            if let Some(text) = matched {
                if self.session_read(name, 200)?.contains(text) {
                    v["matched"] = json!(text);
                    return Ok(v);
                }
                ensure!(
                    Instant::now() < deadline,
                    "timeout: {name} never showed {text:?}"
                );
            } else {
                let s = v["status"].as_str().unwrap_or("");
                let done = if until.is_empty() {
                    s != "working"
                } else {
                    until.iter().any(|u| u == s)
                };
                if done {
                    return Ok(v);
                }
                ensure!(Instant::now() < deadline, "timeout: {name} is still {s}");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    /// The last `lines` lines of the session's screen and scrollback, wrapped lines joined.
    pub fn session_read(&self, name: &str, lines: usize) -> Result<String> {
        let (_, run) = self.current(name)?;
        let r = self.registry()?;
        let panes = self
            .live_panes(&r)
            .context("lost: no server owns this state")?;
        let p = panes
            .iter()
            .find(|p| p.pane == run.pane && p.run == run.id)
            .context("lost: the run's pane is gone")?;
        let start = format!("-{}", lines.clamp(1, 2000));
        let text = self.tm(&["capture-pane", "-p", "-J", "-S", &start, "-t", &p.pane])?;
        // Harness UIs pad the screen with empty rows; they carry nothing.
        let trimmed: Vec<_> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        Ok(trimmed[trimmed.len().saturating_sub(lines)..].join("\n"))
    }
    /// Register (project by name, default the cwd's folder name), opt in to terminal input
    /// and the mailbox, and start. Names are unique across the whole state: they are msg addresses.
    /// With `worktree` (branch, base) the session runs in a new git worktree of the cwd's repo
    /// (project default: the repo's folder name); `model` sets an OpenCode session's model.
    #[allow(clippy::too_many_arguments)]
    pub fn session_add(
        &self,
        name: &str,
        cwd: &Path,
        project: Option<&str>,
        argv: Vec<String>,
        conversation: Option<&str>,
        worktree: Option<(&str, Option<&str>)>,
        model: Option<&str>,
    ) -> Result<Value> {
        // Moving a session from another tool: start it already resuming its conversation.
        if let Some(c) = conversation {
            ensure!(
                !harness(&argv).is_empty(),
                "--conversation needs a claude or codex command"
            );
            ensure!(
                !c.is_empty()
                    && c.len() <= 128
                    && c.chars().all(|x| x.is_ascii_alphanumeric() || x == '-'),
                "invalid conversation ID"
            );
        }
        let cwd = fs::canonicalize(cwd).context("cwd")?;
        ensure!(
            !self.registry()?.agents.iter().any(|a| a.name == name),
            "conflict: a session named {name} already exists"
        );
        // Refused before registering, so a refused add leaves no stopped profile behind.
        if self.server().is_err() {
            crate::server_start_allowed()?;
        }
        let argv = match model {
            Some(m) => crate::worktree::with_model(argv, m)?,
            None => argv,
        };
        crate::validate_name(name)?;
        crate::validate_argv(&argv)?;
        // Last check-and-create step: everything that can refuse runs before it.
        let (cwd, wt) = match worktree {
            Some((branch, base)) => {
                let (path, w) = crate::worktree::create(&cwd, name, branch, base)?;
                (path, Some(w))
            }
            None => (cwd, None),
        };
        let folder = wt.as_ref().map_or(cwd.as_path(), |w| Path::new(&w.repo));
        let pname = match project {
            Some(p) => p.to_string(),
            None => folder
                .file_name()
                .and_then(|n| n.to_str())
                .context("cwd has no folder name; pass --project")?
                .to_string(),
        };
        let existing = self
            .registry()?
            .projects
            .into_iter()
            .find(|p| p.name == pname);
        let pid = match existing {
            Some(p) => p.id,
            None => self.register_project(&pname, &cwd)?.id,
        };
        let a = self.register_agent(&pid, name, &cwd, argv, "generic")?;
        if conversation.is_some() || wt.is_some() {
            let _lock = self.lock()?;
            let mut r = self.registry()?;
            if let Some(x) = r.agents.iter_mut().find(|x| x.id == a.id) {
                x.conversation = conversation.map(str::to_string);
                x.worktree = wt;
            }
            self.save(&r)?;
        }
        self.configure_input(&a.id, "terminal-line")?;
        self.configure_collaboration(&a.id, "mailbox")?;
        self.start(&a.id)?;
        self.session_get(name)
    }
    /// Restart in the same conversation. Refuses a session that is mid-turn unless forced.
    /// Start a stopped registration in its saved conversation (after `agent edit`, a terminate,
    /// or a skipped restore). Refused while it has a run, and when the conversation is unknown
    /// unless `new`.
    pub fn session_start(&self, name: &str, new: bool) -> Result<Value> {
        let r = self.registry()?;
        let a = self.by_name(&r, name)?.clone();
        ensure!(
            !r.runs.iter().any(|x| x.agent_id == a.id),
            "has_run: {name} already has a run; use restart"
        );
        self.fresh_or_known(&a, new)?;
        self.start(&a.id)?;
        self.session_get(name)
    }
    pub fn session_restart(&self, name: &str, force: bool, new: bool) -> Result<Value> {
        let v = self.session_get(name)?;
        let s = v["status"].as_str().unwrap_or("");
        ensure!(
            s != "blocked",
            "blocked: {name} is waiting for a human decision; not restarting it"
        );
        ensure!(
            force || s != "working",
            "busy: {name} is working; wait or pass --force"
        );
        let (a, run) = self.current(name)?;
        self.fresh_or_known(&a, new)?;
        self.restart(&a.id, &run.id)?;
        self.session_get(name)
    }
    /// Refuse an accidental fresh conversation (conversation_unknown); `new` asks for one
    /// explicitly and forgets the old ID so Claude gets a new fixed session ID.
    fn fresh_or_known(&self, a: &Agent, new: bool) -> Result<()> {
        if new {
            let _lock = self.lock()?;
            let mut r = self.registry()?;
            if let Some(x) = r.agents.iter_mut().find(|x| x.id == a.id) {
                x.conversation = None;
            }
            return self.save(&r);
        }
        ensure!(
            !conversation_unknown(a),
            "conversation_unknown: {} has no known conversation ID; pass --new to start a fresh conversation",
            a.name
        );
        Ok(())
    }
    /// Start again, in the same conversation, every profile whose run was lost with its pane
    /// (reboot, killed server). Live and exited runs are left alone. Non-interactive: the login
    /// carrier calls it from a pane allowed to start the server, which it also does.
    pub fn session_restore(&self, new: bool) -> Result<Value> {
        let lost: Vec<Run> = {
            let _lock = self.lock()?;
            let mut r = self.registry()?;
            let panes = match self.server() {
                Err(_) => {
                    // Checked before touching the registry, so a refused restore loses nothing.
                    crate::server_start_allowed()?;
                    vec![]
                }
                Ok(m) if m.is_empty() || m == r.server_instance => self.panes()?,
                Ok(_) => bail!("server identity changed; restore refused"),
            };
            let alive = |x: &Run| panes.iter().any(|p| p.pane == x.pane && p.run == x.id);
            let lost: Vec<Run> = r.runs.iter().filter(|x| !alive(x)).cloned().collect();
            if !lost.is_empty() {
                r.runs.retain(|x| alive(x));
                self.save(&r)?;
                self.audit(
                    "restore",
                    "-",
                    &format!("cleared {} lost runs at {}", lost.len(), now()),
                )?;
            }
            lost
        };
        let mut out = vec![];
        let mut skipped = vec![];
        for run in lost {
            let r = self.registry()?;
            let a = r
                .agents
                .iter()
                .find(|a| a.id == run.agent_id)
                .context("agent not found")?;
            let result = self
                .fresh_or_known(a, new)
                .and_then(|_| self.start(&a.id))
                .map(|run| run.id);
            if let Err(e) = &result
                && format!("{e:#}").contains("conversation_unknown")
            {
                skipped.push(run.clone());
            }
            out.push(json!({"name": a.name, "agent_id": a.id,
                "run_id": result.as_ref().ok(), "error": result.as_ref().err().map(|e| format!("{e:#}"))}));
        }
        // Keep skipped sessions visible as lost, so a later `restore --new` (a human's choice)
        // still finds them. Done after the starts: a server-less registry must have no runs.
        if !skipped.is_empty() {
            let _lock = self.lock()?;
            let mut r = self.registry()?;
            r.runs.extend(skipped);
            self.save(&r)?;
        }
        Ok(json!({"restored": out}))
    }
    /// SessionStart hook (Claude Code and Codex): record the harness's session ID for the
    /// caller's current run. Outside PCTL, or for a stale run, it does nothing and succeeds.
    pub fn session_hook(&self, input: &str) -> Result<Value> {
        let (Ok(agent), Ok(run)) = (std::env::var("PCTL_AGENT_ID"), std::env::var("PCTL_RUN_ID"))
        else {
            return Ok(json!({"recorded": false, "reason": "not inside a PCTL session"}));
        };
        let v: Value = serde_json::from_str(input).context("hook input is not JSON")?;
        let id = v["session_id"].as_str().filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
        let Some(id) = id else {
            return Ok(json!({"recorded": false, "reason": "no session_id"}));
        };
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        if !r.runs.iter().any(|x| x.agent_id == agent && x.id == run) {
            return Ok(json!({"recorded": false, "reason": "stale run"}));
        }
        let a = r
            .agents
            .iter_mut()
            .find(|a| a.id == agent)
            .context("unknown agent")?;
        if a.conversation.as_deref() != Some(id) {
            a.conversation = Some(id.to_string());
            self.save(&r)?;
        }
        Ok(json!({"recorded": true, "conversation": id}))
    }
    /// Unregister a session that has no run (test names, retired workers), so it stops being
    /// a msg address. A session with a run, even an exited or lost one, is refused.
    pub fn session_remove(&self, name: &str) -> Result<Value> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        let id = self.by_name(&r, name)?.id.clone();
        ensure!(
            !r.runs.iter().any(|x| x.agent_id == id),
            "has_run: {name} still has a run; terminate it first"
        );
        r.agents.retain(|a| a.id != id);
        self.save(&r)?;
        self.audit("remove", "-", &format!("unregistered {name}"))?;
        Ok(json!({"removed": name, "agent_id": id}))
    }
    /// The session name of the caller's own run (PCTL_AGENT_ID + PCTL_RUN_ID).
    pub fn session_whoami(&self) -> Result<Value> {
        let agent = std::env::var("PCTL_AGENT_ID").context("not inside a PCTL session")?;
        let run = std::env::var("PCTL_RUN_ID").unwrap_or_default();
        let r = self.registry()?;
        let a = r
            .agents
            .iter()
            .find(|a| a.id == agent)
            .context("unknown PCTL_AGENT_ID")?;
        let current = r.runs.iter().any(|x| x.agent_id == agent && x.id == run);
        Ok(json!({"name": a.name, "agent_id": a.id, "current_run": current}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_explains_and_ignores_quoted_questions_without_input() {
        for raw in [
            "The command reports permission required.\n\n  Build  model\n  tab agents  ctrl+p commands\n",
            "$ echo approve\napprove\n$ ",
        ] {
            assert_eq!(activity(raw, ""), "idle");
        }
        let raw = "heading\n\n• Working (5s • Esc to interrupt)\n› \n";
        let v = activity_reason(raw, "title");
        assert_eq!(v.status, activity(raw, "title"));
        assert_eq!(v.rule, "footer \"esc to interrupt\"");
        assert_eq!(v.row, Some(3));
        assert_eq!(v.text.as_deref(), Some("• Working (5s • Esc to interrupt)"));
        assert_eq!(
            v.considered_rows.iter().map(|r| r.row).collect::<Vec<_>>(),
            [1, 3, 4]
        );
        assert_eq!(v.pane_title, "title");
        for (screen, rule, row) in [
            ("✢ Vibing…\n❯ ", "spinner", 1),
            ("⬝■ esc interrupt ctrl+p commands", "opencode footer", 1),
            ("\x1b[90m❯ ", "dim prompt", 1),
            ("❯ 2. Skip", "numbered menu", 1),
            (
                "❯ \npermission required",
                "question word \"permission required\" below the last input row",
                2,
            ),
            (
                "△ Permission required\nAllow once Always allow Reject",
                "question word \"permission required\" in dialog",
                1,
            ),
        ] {
            let v = activity_reason(screen, "");
            assert_eq!(v.rule, rule);
            assert_eq!(v.row, Some(row));
        }
        let title = activity_reason("› ", "⠧ task");
        assert_eq!(title.rule, "spinner in pane title");
        assert_eq!(title.row, None);
        let tail = activity_reason(&format!("{}› \n", "old\n".repeat(15)), "");
        assert_eq!(tail.considered_rows.len(), 12);
        assert_eq!(tail.considered_rows[0].row, 5);
    }
    #[test]
    fn keys_are_the_allowed_names_only() {
        for key in [
            "Escape", "Enter", "Tab", "BTab", "Up", "Down", "Left", "Right", "BSpace", "Space",
            "0", "9", "y", "n",
        ] {
            assert_eq!(key_name(key), Some(key), "{key} maps to its own tmux name");
        }
        for key in [
            "", "esc", "escape", "Y", "N", "yy", "10", "space", "Ctrl-C", "F5", "hello", "Escape ",
        ] {
            assert_eq!(key_name(key), None, "{key} is refused");
        }
    }
    #[test]
    fn opencode_footer_reads_working() {
        let head = "  ┃  Build · DeepSeek V4.1 Flash Free OpenCode\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n";
        for footer in [
            "   ⬝⬝⬝⬝⬝⬝■■ esc interrupt                                                 35.2K (18%)  ctrl+p commands\n",
            "   ■■■■■⬝⬝⬝ esc interrupt   123.5K (62%)  ctrl+p commands\n",
            "   ⬝⬝⬝⬝⬝⬝⬝⬝ esc interrupt ↓ 1 shell · 139.8K (70%)  ctrl+p commands\n",
        ] {
            assert_eq!(
                activity(&format!("{head}{footer}"), ""),
                "working",
                "{footer}"
            );
            // Coloured, as the terminal gives it.
            let colored = format!("{head}\x1b[38;5;240m{footer}\x1b[0m");
            assert_eq!(activity(&colored, ""), "working");
        }
        // Idle: no "esc interrupt" anywhere.
        let idle = "                       ┃  Build · DeepSeek V4.1 Flash Free OpenCode\n                       ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n                       …/pctl-gh-wt-panel-copy:panel/app-copy    shift+tab agents  ctrl+p commands\n  ⊙ 7 MCP /mcps                                                                         2.0.28\n";
        assert_eq!(activity(idle, ""), "idle");
        // Prose is not a footer: an answer that says it, in the last rows or above them.
        let prose =
            "  ┃ To stop it, press esc interrupt twice.\n  ┃ You can also use ctrl+p commands.\n";
        assert_eq!(activity(&format!("{idle}{prose}"), ""), "idle");
        assert_eq!(activity(&format!("{prose}{idle}"), ""), "idle");
        let old_prose = format!("  press esc interrupt to stop\n{}", "\n  line\n".repeat(6));
        assert_eq!(activity(&old_prose, ""), "idle");
        // A footer scrolled far up the screen is old.
        let stale = format!(
            "   ⬝⬝⬝■■⬝⬝⬝ esc interrupt  ctrl+p commands\n{}{idle}",
            "  more text\n".repeat(8)
        );
        assert_eq!(activity(&stale, ""), "idle");
    }
    #[test]
    fn activity_reads_the_footer() {
        assert_eq!(
            activity("> hi\n\n✻ Thinking… (3s · esc to interrupt)\n", ""),
            "working"
        );
        assert_eq!(
            activity("• Working (5s • Esc to interrupt)\n› \n", ""),
            "working"
        );
        assert_eq!(
            activity("⏺\n✢ Vibing…\n  ⎿  Tip: x\n❯ \n", "✳ t"),
            "working"
        );
        assert_eq!(
            activity("⏺ OK\n✻ Cogitated for 2s · done 11:53 PM\n❯ \n", "✳ t"),
            "idle"
        );
        assert_eq!(activity("› Ask Codex\n", "⠧ Remember | codex"), "working");
        assert_eq!(
            activity("Do you want to proceed?\n❯ 1. Yes\n", ""),
            "blocked"
        );
        assert_eq!(
            activity("> \n? for shortcuts\n", "Remember | codex"),
            "idle"
        );
        let update = "Update available · 0.162.0 → 0.163.0\n› 1. Update now (runs `sh -c ...`)\n  2. Skip\n  enter continue · esc skip\n";
        assert_eq!(activity(update, ""), "blocked");
        assert_eq!(activity("  1. Update now\n› 2. Skip\n", ""), "blocked");
        assert_eq!(activity("› Ask Codex to do anything\n", ""), "idle");
        assert_eq!(activity("❯ 10 items left\n", ""), "idle");
        // A coloured menu row must not read as Claude's dim (working) prompt.
        assert_eq!(
            activity(
                "Do you want to proceed?\n\x1b[36m❯ 1. Yes\x1b[0m\n  2. No\n",
                ""
            ),
            "blocked"
        );
        assert_eq!(activity("\x1b[38;5;153m❯ 2. Skip\x1b[39m\n", ""), "blocked");
        // Claude Code's folder-trust dialog (2.1.x, captured): unnumbered, coloured choices.
        let trust = " Quick safety check: Is this a project you created or one you trust?\n \x1b[38;5;153m❯\x1b[39m \x1b[38;5;153mNo,\x1b[39m \x1b[38;5;153mexit\x1b[39m\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel\n";
        assert_eq!(activity(trust, ""), "blocked");
        let no_footer = trust.replace(" Enter to confirm · Esc to cancel\n", "");
        assert_eq!(
            activity(&no_footer, ""),
            "blocked",
            "the trust question alone"
        );
        // OpenCode's permission dialog (strings from the installed binary): Enter picks Allow once.
        let opencode = "  △ Permission required\n  ┃ bash: rm -rf build\n  Allow once   Always allow   Reject\n  enter confirm  esc reject\n";
        assert_eq!(activity(opencode, ""), "blocked");
        // ... also when its footer still says "esc interrupt": the dialog waits for a person.
        let asking =
            format!("{opencode}   ⬝⬝⬝■■⬝⬝⬝ esc interrupt   35.2K (18%)  ctrl+p commands\n");
        assert_eq!(activity(&asking, ""), "blocked");
        assert_eq!(
            activity(
                "  △ Permission required\n  Allow once   Always allow   Reject\n",
                ""
            ),
            "blocked"
        );
        // Prose above a free input row is not a question (an idle lead reporting "approves").
        let report = "⏺ I merge if the reviewer approves. Do you want to see the diff?\n────\n❯ \n────\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n";
        assert_eq!(activity(report, "✳ t"), "idle");
        assert_eq!(activity("❯ 1. Yes\n⏺ Ran it.\n❯ \n", ""), "idle");
        assert_eq!(
            activity(
                "• Would you like to approve it? I asked.\n› Ask Codex to do anything\n",
                ""
            ),
            "idle"
        );
    }
    #[test]
    fn launch_argv_assigns_then_resumes() {
        let mut r = Registry::default();
        let mk = |id: &str, argv: &[&str]| Agent {
            id: id.into(),
            project_id: "p".into(),
            name: id.into(),
            cwd: "/".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            adapter: "generic".into(),
            input_mode: "terminal-line".into(),
            collaboration_mode: "mailbox".into(),
            conversation: None,
            worktree: None,
        };
        r.agents.push(mk("c", &["/x/claude", "--model", "m"]));
        r.agents.push(mk("x", &["codex", "-m", "g"]));
        r.agents.push(mk("o", &["claude", "--resume", "fixed"]));
        let first = launch_argv(&mut r, "c").unwrap();
        let id = r.agents[0].conversation.clone().unwrap();
        assert_eq!(first[3..], ["--session-id".to_string(), id.clone()]);
        assert_eq!(
            launch_argv(&mut r, "c").unwrap()[3..],
            ["--resume".to_string(), id]
        );
        assert_eq!(launch_argv(&mut r, "x").unwrap(), ["codex", "-m", "g"]);
        r.agents[1].conversation = Some("t1".into());
        assert_eq!(
            launch_argv(&mut r, "x").unwrap(),
            ["codex", "resume", "t1", "-m", "g"]
        );
        assert_eq!(
            launch_argv(&mut r, "o").unwrap(),
            ["claude", "--resume", "fixed"]
        );
        assert!(r.agents[2].conversation.is_none());
    }
    #[test]
    fn discover_codex_matches_cwd_and_skips_taken() {
        let d = tempfile::tempdir().unwrap();
        let day = d.path().join("2026/10/15");
        fs::create_dir_all(&day).unwrap();
        let meta = |id: &str, cwd: &str| {
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\"}}}}\n"
            )
        };
        fs::write(day.join("rollout-a.jsonl"), meta("other-cwd", "/elsewhere")).unwrap();
        fs::write(day.join("rollout-b.jsonl"), meta("taken", "/w")).unwrap();
        fs::write(day.join("rollout-c.jsonl"), meta("mine", "/w")).unwrap();
        let mut r = Registry::default();
        let mut a = Agent {
            id: "a".into(),
            project_id: "p".into(),
            name: "a".into(),
            cwd: "/w".into(),
            argv: vec!["codex".into()],
            adapter: "generic".into(),
            input_mode: "native".into(),
            collaboration_mode: "disabled".into(),
            conversation: None,
            worktree: None,
        };
        let mut b = a.clone();
        b.id = "b".into();
        b.conversation = Some("taken".into());
        r.agents.push(b);
        r.agents.push(a.clone());
        let run = Run {
            id: "r".into(),
            agent_id: "a".into(),
            project_id: "p".into(),
            server_instance: "s".into(),
            session: "$1".into(),
            pane: "%1".into(),
            window: "@1".into(),
            pid: "1".into(),
            process_start: "x".into(),
            started_at: now(),
        };
        assert_eq!(
            discover_codex(&r, &a, &run, d.path()).as_deref(),
            Some("mine")
        );
        fs::write(day.join("rollout-d.jsonl"), meta("twin", "/w")).unwrap();
        assert_eq!(
            discover_codex(&r, &a, &run, d.path()),
            None,
            "two candidates: unknown"
        );
        a.cwd = "/nowhere".into();
        assert_eq!(discover_codex(&r, &a, &run, d.path()), None);
    }
}
