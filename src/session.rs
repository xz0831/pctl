//! Name-addressed session commands (the Herdr replacement surface):
//! list/get/prompt/wait/read/whoami, harness conversation IDs kept across restarts,
//! and restore after the tmux server is gone (reboot).
use crate::{Agent, Core, Registry, Run, ensure_terminal_input, now};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
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
/// Screen-derived activity, like Herdr's, from a colored capture (`capture-pane -e`).
/// Working signs seen on real sessions (2026-10-15): Codex "• Working (6s • esc to interrupt)"
/// and a braille spinner in the pane title; Claude Code 2.1 a spinner line ("✢ Vibing…") before
/// text streams, and a dimmed input prompt "❯" for the whole turn (default color when idle).
// shortcut: the dimmed-prompt cue is Claude Code's rendering, not an API; if a Claude update
// breaks it, report state from UserPromptSubmit/Stop hooks the way `session hook` does.
fn activity(raw: &str, title: &str) -> &'static str {
    let rows: Vec<(&str, String)> = raw
        .lines()
        .map(|l| (l, plain(l).trim().to_lowercase()))
        .filter(|(_, l)| !l.is_empty())
        .collect();
    let tail = &rows[rows.len().saturating_sub(12)..];
    let spinner_line =
        |l: &str| l.starts_with(['✢', '✳', '✶', '✻', '✽', '*', '·']) && l.contains('…');
    // The last prompt row: its color code is written right before the "❯".
    let dim_prompt = tail
        .iter()
        .rev()
        .find(|(_, l)| l.starts_with('❯'))
        .is_some_and(|(r, _)| {
            r.split('❯').next().is_some_and(|before| {
                before
                    .rfind("\x1b[")
                    .is_some_and(|i| !before[i..].starts_with("\x1b[39m"))
            })
        });
    let spinner_title = title
        .chars()
        .next()
        .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c) || "◐◓◑◒".contains(c));
    if spinner_title
        || dim_prompt
        || tail
            .iter()
            .any(|(_, l)| l.contains("esc to interrupt") || spinner_line(l))
    {
        "working"
    } else if tail.iter().any(|(_, l)| {
        [
            "do you want to",
            "would you like to",
            "allow command",
            "approve",
        ]
        .iter()
        .any(|k| l.contains(k))
    }) || tail.iter().any(|(_, l)| menu_row(l))
    {
        "blocked"
    } else {
        "idle"
    }
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
    fn view(&self, r: &Registry, a: &Agent, panes: Option<&[crate::Pane]>) -> Value {
        let run = r.runs.iter().find(|x| x.agent_id == a.id);
        let pane = run.and_then(|run| {
            panes?
                .iter()
                .find(|p| p.pane == run.pane && p.run == run.id)
        });
        let (process, status) = match (run, pane) {
            (None, _) => ("stopped", "stopped"),
            (Some(_), None) => ("lost", "lost"),
            (Some(_), Some(p)) if p.dead => ("exited", "exited"),
            (Some(_), Some(p)) => (
                "running",
                self.tm(&["capture-pane", "-p", "-e", "-t", &p.pane])
                    .map_or("unknown", |s| {
                        let title = self
                            .tm(&["display-message", "-p", "-t", &p.pane, "#{pane_title}"])
                            .unwrap_or_default();
                        activity(&s, &title)
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
        Ok(r.agents
            .iter()
            .map(|a| self.view(&r, a, panes.as_deref()))
            .collect())
    }
    pub fn session_get(&self, name: &str) -> Result<Value> {
        let r = self.discover()?;
        let a = self.by_name(&r, name)?;
        Ok(self.view(&r, a, self.live_panes(&r).as_deref()))
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
    /// Poll until the status is one of `until` (default: not working).
    pub fn session_wait(&self, name: &str, until: &[String], timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let v = self.session_get(name)?;
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
        // Moving a Herdr session: start it already resuming its conversation.
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
    /// carrier calls it from a Herdr pane, which also starts the server.
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
