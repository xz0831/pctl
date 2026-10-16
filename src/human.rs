//! The human entry, Herdr-like: `pctl` alone opens the running workspace, projects are found by
//! name, and tabs carry agent names and states. They are tmux user options: `@pctl_name` (project,
//! on the session), `@pctl_tab` and `@pctl_status` (agent, on its window; a distinct name because
//! formats look up window options before session ones). Window and session names stay as they
//! are, so no identity check changes. Nothing here starts a server.
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

    /// Write each running agent's name and state onto its tmux window, name each project
    /// session, and return a one-line summary for the status line. Run by the status line
    /// while someone is attached; never starts a server.
    pub fn session_marks(&self) -> Result<String> {
        let all = self.session_list()?;
        let r = self.registry()?;
        // One tmux call: commands joined by ";" arguments.
        let mut cmds: Vec<String> = Vec::new();
        let mut set = |args: &[&str]| {
            if !cmds.is_empty() {
                cmds.push(";".into());
            }
            cmds.push("set-option".into());
            cmds.extend(args.iter().map(|a| literal(a)));
        };
        let (mut blocked, mut working) = (0, 0);
        for s in &all {
            let status = s["status"].as_str().unwrap_or("unknown");
            let (Some(pane), Some(name)) = (s["pane"].as_str(), s["name"].as_str()) else {
                continue;
            };
            if matches!(status, "stopped" | "lost") {
                continue;
            }
            blocked += usize::from(status == "blocked");
            working += usize::from(status == "working");
            set(&["-w", "-t", pane, "@pctl_status", status]);
            set(&["-w", "-t", pane, "@pctl_tab", name]);
        }
        let sessions = self.tm(&["list-sessions", "-F", "#{session_id}\t#{@pctl_project}"])?;
        for line in sessions.lines() {
            let Some((sid, pid)) = line.split_once('\t') else {
                continue;
            };
            if let Some(p) = r.projects.iter().find(|p| p.id == pid) {
                set(&["-t", sid, "@pctl_name", &p.name]);
            }
        }
        if !cmds.is_empty() {
            self.tm(&cmds.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        Ok(match (blocked, working) {
            (0, 0) => "all idle".into(),
            (b, 0) => format!("! {b} blocked"),
            (0, w) => format!("* {w} working"),
            (b, w) => format!("! {b} blocked  * {w} working"),
        })
    }

    /// `pctl` alone on a terminal: attach to the running server at the most recently used
    /// project, in the window used last. Refuses when no server runs (it never starts one) and
    /// inside another tmux; inside this server it switches the client instead of nesting.
    pub fn open_default(&self) -> Result<()> {
        let r = self.registry()?;
        let marker = self.server().map_err(|_| {
            anyhow!(
                "no PCTL server is running. Start it from a Herdr pane (pctl-server-herdr); on a host without Herdr use PCTL_STANDALONE=1 pctl attach --project <name>"
            )
        })?;
        ensure!(
            marker == r.server_instance,
            "this tmux server does not belong to this PCTL state; controls locked"
        );
        let rows = self.tm(&[
            "list-sessions",
            "-F",
            "#{session_last_attached}\t#{session_activity}\t#{session_id}\t#{@pctl_project}",
        ])?;
        let session = rows
            .lines()
            .filter_map(|l| {
                let f: Vec<_> = l.split('\t').collect();
                let known = f.len() == 4 && r.projects.iter().any(|p| p.id == f[3]);
                known.then(|| {
                    let n = |s: &str| s.parse::<u64>().unwrap_or(0);
                    ((n(f[0]), n(f[1])), f[2].to_string())
                })
            })
            .max()
            .map(|(_, id)| id)
            .context("the PCTL server has no project yet; start an agent with pctl session add")?;
        self.apply_theme_chrome()?;
        let _ = self.session_marks();
        if let Ok(outer) = std::env::var("TMUX") {
            let socket = outer.split(',').next().unwrap_or_default();
            let same = |a: &Path, b: &Path| match (fs::canonicalize(a), fs::canonicalize(b)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            ensure!(
                same(Path::new(socket), &self.state.join("tmux.sock")),
                "already inside another tmux; detach from it first (tmux would nest)"
            );
            let status = self
                .tmux_cmd()
                .env("TMUX", &outer)
                .args(["switch-client", "-t", &session])
                .status()?;
            ensure!(status.success(), "tmux switch-client failed");
            return Ok(());
        }
        let status = self
            .tmux_cmd()
            .args(["attach-session", "-t", &session])
            .status()?;
        ensure!(status.success(), "tmux attach failed");
        Ok(())
    }
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
    #[test]
    fn a_trailing_semicolon_stays_literal() {
        assert_eq!(super::literal("a;"), "a\\;");
        assert_eq!(super::literal("a;b"), "a;b");
        assert_eq!(super::literal("x"), "x");
    }
}
