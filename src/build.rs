//! Launch-time executable identity; this never runs a harness to ask its version.
use crate::{Agent, Core, Run};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Build {
    pub resolved_path: PathBuf,
    pub size: u64,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: i64,
    pub inode: u64,
    pub device: u64,
    #[serde(default)]
    pub interpreter: Option<Box<Build>>,
}
impl Build {
    fn read(path: &Path) -> Result<Self> {
        let resolved_path = fs::canonicalize(path)?;
        resolved_path
            .to_str()
            .context("executable path is not UTF-8")?;
        let m = fs::metadata(&resolved_path)?;
        ensure!(m.is_file() && m.mode() & 0o111 != 0, "not executable");
        Ok(Self {
            resolved_path,
            size: m.len(),
            mtime_seconds: m.mtime(),
            mtime_nanoseconds: m.mtime_nsec(),
            inode: m.ino(),
            device: m.dev(),
            interpreter: None,
        })
    }
}

// Only the env assignment form used by PCTL's model wrapper is unambiguous here.
fn command_index(argv: &[String]) -> Option<usize> {
    let first = argv.first()?;
    if Path::new(first).file_name()?.to_str()? != "env" {
        return Some(0);
    }
    argv.iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, s)| {
            if s.starts_with('-') {
                Some(None)
            } else if s.contains('=') {
                None
            } else {
                Some(Some(i))
            }
        })
        .flatten()
}
impl Core {
    pub(crate) fn resolve_build(&self, a: &Agent) -> Option<(usize, Build)> {
        self.resolve_build_in(a, &mut HashMap::new())
    }
    fn resolve_build_in(
        &self,
        a: &Agent,
        paths: &mut HashMap<String, Option<String>>,
    ) -> Option<(usize, Build)> {
        let i = command_index(&a.argv)?;
        let cwd = Path::new(&a.cwd);
        let mut search_path: Option<Option<String>> = None;
        let mut resolve = |command: &str| -> Option<Build> {
            if command.contains('/') {
                let p = Path::new(command);
                return Build::read(&if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    cwd.join(p)
                })
                .ok();
            }
            let path = search_path
                .get_or_insert_with(|| {
                    let mut path = paths
                        .entry(a.project_id.clone())
                        .or_insert_with(|| self.launcher_path(a))
                        .clone();
                    for setting in &a.argv[..i] {
                        if let Some(value) = setting.strip_prefix("PATH=") {
                            path = Some(value.into());
                        }
                    }
                    path
                })
                .as_ref()?;
            std::env::split_paths(path).find_map(|p| {
                let p = if p.is_absolute() { p } else { cwd.join(p) };
                Build::read(&p.join(command)).ok()
            })
        };
        let mut build = resolve(&a.argv[i])?;
        // Record a script's entrypoint as well as its interpreter, never just the latter.
        let mut f = std::fs::File::open(&build.resolved_path).ok()?;
        let mut header = [0; 256];
        let n = std::io::Read::read(&mut f, &mut header).ok()?;
        if header[..n].starts_with(b"#!") {
            let line = std::str::from_utf8(header[..n].split(|b| *b == b'\n').next()?).ok()?;
            let words: Vec<_> = line[2..].split_whitespace().collect();
            let first = *words.first()?;
            let interpreter = if Path::new(first).file_name()?.to_str()? == "env" {
                if words.len() != 2 || words[1].starts_with('-') {
                    return None;
                }
                words[1]
            } else {
                first
            };
            build.interpreter = Some(Box::new(resolve(interpreter)?));
        }
        Some((i, build))
    }
    fn launcher_path(&self, a: &Agent) -> Option<String> {
        let r = self.registry().ok()?;
        let session = r
            .runs
            .iter()
            .find(|r| r.project_id == a.project_id)
            .map(|r| r.session.clone())
            .or_else(|| self.project_session(&r, &a.project_id).ok().flatten());
        let value = if let Some(session) = session {
            let global = self.tm(&["show-environment", "-g", "PATH"]).ok();
            self.tm(&["show-environment", "-t", &session, "PATH"])
                .ok()
                .or(global)?
        } else if self.server().is_ok() {
            self.tm(&["show-environment", "-g", "PATH"]).ok()?
        } else {
            return std::env::var("PATH").ok();
        };
        value.strip_prefix("PATH=").map(str::to_string)
    }
    pub(crate) fn build_fields(
        &self,
        a: &Agent,
        run: Option<&Run>,
        status: &str,
        paths: &mut HashMap<String, Option<String>>,
    ) -> Value {
        let installed = self.resolve_build_in(a, paths).map(|(_, b)| b);
        let running = run.and_then(|r| r.running_build.as_ref());
        let stale = matches!(status, "idle" | "working" | "blocked")
            .then(|| running.zip(installed.as_ref()).map(|(a, b)| a != b))
            .flatten();
        let mut blockers = Vec::new();
        if status != "idle" {
            blockers.push(format!("status:{status}"));
        }
        if a.protected {
            blockers.push("protected".into());
        }
        if a.conversation.is_none() {
            blockers.push("conversation_unknown".into());
        }
        if a.argv.iter().any(|s| {
            ["--resume", "-r", "--continue", "-c", "--session-id"].contains(&s.as_str())
                || s.starts_with("--resume=")
                || s.starts_with("--session-id=")
        }) {
            blockers.push("conversation_selector_unverified".into());
        }
        if crate::session::harness(&a.argv) != "claude" {
            blockers.push("harness_not_verified".into());
        }
        match stale {
            None => blockers.push("build_unknown".into()),
            Some(false) => blockers.push("already_current".into()),
            _ => {}
        }
        if let Some(run) = run {
            match self.app_viewed(&run.id) {
                Ok(true) => blockers.push("app_visible".into()),
                Err(_) => blockers.push("app_visibility_unknown".into()),
                _ => {}
            }
        }
        json!({"running_build": running, "installed_build": installed,
            "stale": stale.map_or(json!("unknown"), |s| json!(s)),
            "protected": a.protected, "swap_blockers": blockers})
    }
    pub(crate) fn set_app_viewed(&self, runs: &[String]) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let dir = self.state.join("viewers");
        let pid = std::process::id().to_string();
        let old: serde_json::Value = fs::read(dir.join(format!("{pid}.json")))
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let old_runs: Vec<String> = old["runs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if old_runs != runs {
            for run in &r.runs {
                if old_runs.contains(&run.id) || runs.contains(&run.id) {
                    self.refresh_event_locked(&r, &run.agent_id, &run.id, "visibility")?;
                }
            }
        }
        if runs.is_empty() {
            let path = dir.join(format!("{pid}.json"));
            if path.exists() {
                fs::remove_file(path)?;
            }
            return Ok(());
        }
        fs::create_dir_all(&dir)?;
        crate::atomic_json(
            &dir.join(format!("{pid}.json")),
            &json!({"pid":pid,"start":crate::process_start(&pid)?,"runs":runs}),
        )
    }
    fn app_viewed(&self, run: &str) -> Result<bool> {
        let dir = self.state.join("viewers");
        if !dir.exists() {
            return Ok(false);
        }
        for f in fs::read_dir(dir)? {
            let v: Value = serde_json::from_slice(&fs::read(f?.path())?)?;
            let pid = v["pid"].as_str().context("missing viewer pid")?;
            let start = v["start"].as_str().context("missing viewer generation")?;
            if crate::process_start(pid).ok().as_deref() == Some(start)
                && v["runs"]
                    .as_array()
                    .context("missing viewer runs")?
                    .iter()
                    .any(|r| r == run)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unambiguous_env_wrappers_resolve() {
        let index =
            |args: &[&str]| command_index(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(index(&["claude"]), Some(0));
        assert_eq!(index(&["env", "PATH=/test", "claude"]), Some(2));
        assert_eq!(index(&["/usr/bin/env", "A=b", "claude"]), Some(2));
        assert_eq!(index(&["env", "-S", "claude"]), None);
        assert_eq!(index(&["env", "A=b"]), None);
        assert_eq!(index(&[]), None);
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unrepresentable_paths_are_unknown_instead_of_json_errors() {
        use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join(std::ffi::OsString::from_vec(vec![0xff]));
        fs::write(&file, b"fixture").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(Build::read(&file).is_err());
    }
    #[test]
    fn live_viewers_block_only_their_runs_and_clear_on_leave() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::new(d.path().join("s"), "tmux".into()).unwrap();
        assert!(!core.app_viewed("run").unwrap());
        core.set_app_viewed(&["run".into()]).unwrap();
        assert!(core.app_viewed("run").unwrap());
        assert!(!core.app_viewed("other").unwrap());
        core.set_app_viewed(&[]).unwrap();
        assert!(!core.app_viewed("run").unwrap());
    }
}
