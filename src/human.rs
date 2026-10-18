//! Advanced raw tmux entry. Project and agent names are static tmux user options;
//! every attach targets a verified registered agent pane and never creates a UI or server.
use crate::Core;
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{fs, path::Path};

impl Core {
    /// A project ID from its ID or its unique name.
    pub fn resolve_project(&self, name_or_id: &str) -> Result<String> {
        let r = self.registry()?;
        if r.projects.iter().any(|p| p.id == name_or_id) {
            return Ok(name_or_id.into());
        }
        let found: Vec<_> = r.projects.iter().filter(|p| p.name == name_or_id).collect();
        match found[..] {
            [p] => Ok(p.id.clone()),
            [] => bail!("project not found: {name_or_id}"),
            _ => bail!(
                "ambiguous: {} projects are named {name_or_id}; use the ID",
                found.len()
            ),
        }
    }

    /// Raw tmux entry: only a registered, verified agent pane, never a legacy control window.
    pub fn attach(&self, project: &str) -> Result<()> {
        self.attach_agent(Some(project))
    }
    pub fn open_default(&self) -> Result<()> {
        self.attach_agent(None)
    }
    fn attach_agent(&self, project: Option<&str>) -> Result<()> {
        let outer = std::env::var("TMUX").ok();
        if let Some(outer) = &outer {
            let socket = outer.split(',').next().unwrap_or_default();
            ensure!(
                fs::canonicalize(socket)
                    .ok()
                    .zip(fs::canonicalize(self.state.join("tmux.sock")).ok())
                    .is_some_and(|(a, b)| a == b),
                "already inside another tmux; detach from it first (tmux would nest)"
            );
        }
        let pane = {
            let _lock = self.lock()?;
            let r = self.registry()?;
            ensure!(
                self.server().map_err(|_| anyhow!(
                    "no PCTL server is running; open pctl and start an agent"
                ))? == r.server_instance,
                "this tmux server does not belong to this PCTL state; controls locked"
            );
            self.retire_old_panel()?;
            let panes = self.panes()?;
            let eligible: Vec<_> = r
                .runs
                .iter()
                .filter_map(|run| {
                    let a = r.agents.iter().find(|a| a.id == run.agent_id)?;
                    let p = panes.iter().find(|p| {
                        p.pane == run.pane
                            && p.run == run.id
                            && p.agent == a.id
                            && p.project == a.project_id
                    })?;
                    self.guard(&r, &a.id, &run.id, false)
                        .ok()
                        .map(|_| (a.project_id.as_str(), p))
                })
                .collect();
            let rows = self.tm(&[
                "list-sessions",
                "-F",
                "#{session_last_attached}\t#{session_activity}\t#{session_id}\t#{@pctl_project}",
            ])?;
            let target = match project {
                Some(id) => id.to_string(),
                None => rows
                    .lines()
                    .filter_map(|l| {
                        let f: Vec<_> = l.split('\t').collect();
                        (f.len() == 4 && eligible.iter().any(|(id, _)| *id == f[3])).then(|| {
                            let n = |s: &str| s.parse::<u64>().unwrap_or(0);
                            ((n(f[0]), n(f[1])), f[3].to_string())
                        })
                    })
                    .max()
                    .map(|(_, id)| id)
                    .context("no agent window to attach; open pctl and start an agent")?,
            };
            let candidates: Vec<_> = eligible
                .into_iter()
                .filter(|(id, _)| *id == target)
                .map(|(_, p)| p)
                .collect();
            let first = candidates
                .first()
                .context("no agent window to attach; open pctl and start an agent")?;
            let active = self.tm(&[
                "display-message",
                "-p",
                "-t",
                &first.session,
                "#{window_id}\t#{pane_id}",
            ])?;
            let (window, pane) = active.split_once('\t').unwrap_or_default();
            let picked = candidates
                .iter()
                .find(|p| p.window == window && p.pane == pane)
                .or_else(|| candidates.iter().find(|p| p.window == window))
                .unwrap_or(first);
            (**picked).clone()
        };
        let mut cmd = self.tmux_cmd();
        if let Some(outer) = outer {
            // Without -c tmux takes "the best client" of the session, which can be a control
            // client of ours; the person's is the non-control client that was active last.
            let person = self
                .tm(&[
                    "list-clients",
                    "-f",
                    crate::bridge::NOT_CONTROL,
                    "-F",
                    "#{client_activity} #{client_name}",
                ])
                .ok()
                .and_then(|rows| {
                    rows.lines()
                        .filter_map(|l| l.split_once(' '))
                        .max_by_key(|(t, _)| t.parse::<u64>().unwrap_or(0))
                        .map(|(_, name)| name.to_string())
                });
            cmd.env("TMUX", outer).arg("switch-client");
            if let Some(name) = &person {
                cmd.args(["-c", name]);
            }
            cmd.args(["-t", &pane.pane]);
        } else {
            self.tm(&["select-window", "-t", &pane.window])?;
            self.tm(&["select-pane", "-t", &pane.pane])?;
            cmd.args(["attach-session", "-t", &pane.session]);
        }
        ensure!(cmd.status()?.success(), "tmux attach/switch failed");
        Ok(())
    }
    /// Retire only exact old PCTL routes on this verified server; leave every existing pane alone.
    pub(crate) fn retire_old_panel(&self) -> Result<()> {
        let Ok(marker) = self.server() else {
            return Ok(());
        };
        ensure!(
            marker == self.registry()?.server_instance,
            "server identity changed; controls locked"
        );
        self.tm(&["set-option", "-s", "exit-empty", "off"])?;
        if let Ok(key) = self.tm(&["list-keys", "-T", "prefix", "m"])
            && key.split_whitespace().eq([
                "bind-key",
                "-T",
                "prefix",
                "m",
                "select-window",
                "-t",
                ":manager",
            ])
        {
            self.tm(&["unbind-key", "-T", "prefix", "m"])?;
        }
        let status = self.tm(&["show-options", "-gqv", "status-right"])?;
        let cleaned = without_marks_job(&status, &self.state, &self.tmux);
        if cleaned != status {
            self.tm(&["set-option", "-g", "status-right", &literal(&cleaned)])?;
        }
        Ok(())
    }
    pub(crate) fn legacy_sidebar(&self, r: &crate::Registry, pane: &crate::Pane) -> bool {
        let Some(v) = fs::read(self.state.join("sidebar.json"))
            .ok()
            .and_then(|s| serde_json::from_slice::<serde_json::Value>(&s).ok())
        else {
            return false;
        };
        let Some(id) = v["id"].as_str().filter(|id| !id.is_empty()) else {
            return false;
        };
        if v["server_instance"] != r.server_instance
            || v["pane"] != pane.pane
            || !pane.agent.is_empty()
            || (!pane.dead
                && (v["pid"] != pane.pid
                    || crate::process_start(&pane.pid).ok().as_deref()
                        != v["process_start"].as_str()))
        {
            return false;
        }
        self.tm(&[
            "display-message",
            "-p",
            "-t",
            &pane.pane,
            "#{@pctl_sidebar}\t#{@pctl_sidebar_instance}\t#{@pctl_agent}END",
        ])
        .is_ok_and(|tags| tags == format!("{id}\t{}\tEND", r.server_instance))
    }
}

fn without_marks_job(value: &str, state: &Path, tmux: &Path) -> String {
    let tail = format!(
        " {} {} {} {} {} {})",
        crate::shell_quote("--state"),
        crate::shell_quote(&state.to_string_lossy()),
        crate::shell_quote("--tmux"),
        crate::shell_quote(&tmux.to_string_lossy()),
        crate::shell_quote("session"),
        crate::shell_quote("marks")
    );
    let mut out = value.to_string();
    let mut scan = 0;
    while let Some(offset) = out[scan..].find(&tail) {
        let end = scan + offset;
        scan = end + tail.len();
        let start = out[..end].rmatch_indices("#(").find_map(|(start, _)| {
            let head = &out[start + 2..end];
            let exe = head
                .strip_prefix('\'')?
                .strip_suffix('\'')?
                .replace("'\\''", "'");
            (Path::new(&exe)
                .file_name()
                .is_some_and(|name| name == "pctl")
                && crate::shell_quote(&exe) == head)
                .then_some(start)
        });
        let Some(start) = start else {
            continue;
        };
        out.replace_range(start..end + tail.len(), "");
        scan = start;
    }
    if out != value {
        out = out.replace("^b s tree  m manager  q detach", "^b s tree  q detach");
    }
    out
}

/// tmux reads an argument that ends in ";" as a command separator; "\;" keeps it literal.
pub(crate) fn literal(v: &str) -> String {
    match v.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retires_only_generated_jobs_for_this_state_and_tmux() {
        let state = Path::new("/tmp/one 'state");
        let tmux = Path::new("/bin/tmux");
        let job = |state: &Path, extra: &str| {
            format!(
                "#({} {} {} {} {} {} {}{extra})",
                crate::shell_quote("/old's directory/pctl"),
                crate::shell_quote("--state"),
                crate::shell_quote(&state.to_string_lossy()),
                crate::shell_quote("--tmux"),
                crate::shell_quote(&tmux.to_string_lossy()),
                crate::shell_quote("session"),
                crate::shell_quote("marks")
            )
        };
        let value = format!(
            "user #(date) {} │ ^b s tree  m manager  q detach",
            job(state, "")
        );
        assert_eq!(
            without_marks_job(&value, state, tmux),
            "user #(date)  │ ^b s tree  q detach"
        );
        let nested_path = job(state, "").replace("/old", "/nested#(old");
        assert_eq!(without_marks_job(&nested_path, state, tmux), "");
        let custom = job(state, "")
            .replace("/old", "/custom")
            .replace("/pctl'", "/echo'");
        let mixed = format!("{custom} {}", job(state, ""));
        assert_eq!(without_marks_job(&mixed, state, tmux), format!("{custom} "));
        for other in [
            job(Path::new("/other"), ""),
            job(state, " --other"),
            "#(echo 'session' 'marks')".into(),
        ] {
            assert_eq!(without_marks_job(&other, state, tmux), other);
        }
    }
    #[test]
    fn a_trailing_semicolon_stays_literal() {
        assert_eq!(super::literal("a;"), "a\\;");
        assert_eq!(super::literal("a;b"), "a;b");
        assert_eq!(super::literal("x"), "x");
    }
}
