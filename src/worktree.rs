//! Git worktrees for workers: `session add --worktree` creates a branch and a worktree beside the
//! repository, `worktree list` shows their state, and `worktree remove` cleans up only a stopped,
//! clean, merged one (folder to ~/.Trash, never deleted). pctl calls `git`; no git library.
use crate::{Core, now};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Worktree {
    /// Main worktree of the repository.
    pub repo: String,
    pub branch: String,
    /// The ref the branch started from and is expected to be merged into.
    pub base: String,
}

/// Run git in `dir`: stdout on success, git's own message on failure.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // A caller's GIT_DIR would point every check below at another repository.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .context("git")?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

#[derive(Default)]
struct Entry {
    path: String,
    branch: Option<String>,
    bare: bool,
    locked: bool,
    prunable: bool,
}
/// `git worktree list`; the first entry is the main worktree.
fn entries(dir: &Path) -> Result<Vec<Entry>> {
    let text = git(dir, &["worktree", "list", "--porcelain", "-z"])?;
    Ok(text
        .split("\0\0")
        .filter(|r| !r.trim_matches('\0').is_empty())
        .map(|r| {
            let mut e = Entry::default();
            for l in r.split('\0') {
                let (k, v) = l.split_once(' ').unwrap_or((l, ""));
                match k {
                    "worktree" => e.path = v.into(),
                    "branch" => e.branch = Some(v.trim_start_matches("refs/heads/").into()),
                    "bare" => e.bare = true,
                    "locked" => e.locked = true,
                    "prunable" => e.prunable = true,
                    _ => {}
                }
            }
            e
        })
        .collect())
}
fn same(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(a)) == canon(b)
}
fn first_lines(s: &str) -> String {
    let lines: Vec<_> = s.lines().collect();
    let more = lines.len().saturating_sub(5);
    let mut out = lines[..lines.len().min(5)].join("\n");
    if more > 0 {
        out += &format!("\n… and {more} more");
    }
    out
}
fn merged(repo: &Path, w: &Worktree) -> bool {
    git(repo, &["merge-base", "--is-ancestor", &w.branch, &w.base]).is_ok()
}

/// Check everything, then create branch `branch` from `base` (default: the branch checked out at
/// `cwd`) in a new worktree `<repo>-wt-<name>` beside the main worktree. Nothing is created when
/// a check fails.
pub(crate) fn create(
    cwd: &Path,
    name: &str,
    branch: &str,
    base: Option<&str>,
) -> Result<(PathBuf, Worktree)> {
    ensure!(
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "--worktree needs a session name of letters, digits, '-', '_' or '.' (it names the folder)"
    );
    let list = entries(cwd).context("--worktree needs a git checkout as --cwd")?;
    let main = list.first().context("git listed no worktree")?;
    ensure!(!main.bare, "bare repository: pass a checkout as --cwd");
    let repo = PathBuf::from(&main.path);
    ensure!(
        !branch.starts_with('-') && git(&repo, &["check-ref-format", "--branch", branch]).is_ok(),
        "invalid branch name {branch}"
    );
    ensure!(
        git(
            &repo,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ]
        )
        .is_err(),
        "conflict: branch {branch} already exists"
    );
    let here = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let base = match base {
        Some(b) => b.to_string(),
        None => here
            .clone()
            .context("detached HEAD at --cwd: pass --base")?,
    };
    ensure!(
        !base.starts_with('-')
            && git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{base}^{{commit}}")
                ]
            )
            .is_ok(),
        "base {base} is not a commit"
    );
    if here.as_deref() == Some(base.as_str()) {
        let dirty = git(cwd, &["status", "--porcelain"])?;
        ensure!(
            dirty.is_empty(),
            "dirty base: {base} at {} has changes the new worktree would not get; commit or stash them:\n{}",
            cwd.display(),
            first_lines(&dirty)
        );
    }
    let folder = repo
        .file_name()
        .and_then(|n| n.to_str())
        .context("repository folder name")?;
    let path = repo
        .parent()
        .context("repository has no parent folder")?
        .join(format!("{folder}-wt-{name}"));
    ensure!(
        !path.exists(),
        "conflict: {} already exists",
        path.display()
    );
    if let Some(e) = list.iter().find(|e| same(&e.path, &path)) {
        bail!(
            "conflict: git still lists a worktree at {}{}; run `git worktree prune`{}",
            path.display(),
            if e.locked { " (locked)" } else { " (prunable)" },
            if e.locked {
                " after `git worktree unlock`"
            } else {
                ""
            }
        );
    }
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            branch,
            &path.to_string_lossy(),
            &base,
        ],
    )?;
    Ok((
        path,
        Worktree {
            repo: repo.to_string_lossy().into(),
            branch: branch.into(),
            base,
        },
    ))
}

/// OpenCode 2 has no TUI model flag, and its shared background service neither sees the caller's
/// environment nor re-reads a folder's config after first use. A private server (`--standalone`)
/// reads OPENCODE_CONFIG_CONTENT, which overrides every config file, and nothing is written.
pub(crate) fn with_model(argv: Vec<String>, model: &str) -> Result<Vec<String>> {
    ensure!(
        argv.first()
            .and_then(|a| Path::new(a).file_name())
            .is_some_and(|n| n == "opencode"),
        "--model is for opencode; claude and codex take their own model flag in the command"
    );
    ensure!(
        model.len() <= 200
            && model
                .split_once('/')
                .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
            && model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-:#~@+".contains(c)),
        "invalid model {model}: use provider/model"
    );
    ensure!(
        !argv.iter().any(|a| a == "--server"),
        "--model needs a private OpenCode server; drop --server"
    );
    let mut v = vec![
        "env".to_string(),
        format!("OPENCODE_CONFIG_CONTENT={}", json!({ "model": model })),
    ];
    v.extend(argv);
    if !v.iter().any(|a| a == "--standalone") {
        v.push("--standalone".into());
    }
    Ok(v)
}

impl Core {
    /// Sessions whose worktree `session add --worktree` made, with git's view of each.
    pub fn worktree_list(&self) -> Result<Vec<Value>> {
        let sessions = self.session_list()?;
        let r = self.registry()?;
        Ok(r.agents
            .iter()
            .filter_map(|a| Some((a, a.worktree.as_ref()?)))
            .map(|(a, w)| {
                let (repo, path) = (Path::new(&w.repo), Path::new(&a.cwd));
                let status = sessions
                    .iter()
                    .find(|s| s["name"] == a.name.as_str())
                    .map(|s| s["status"].clone());
                let entry = entries(repo)
                    .ok()
                    .and_then(|l| l.into_iter().find(|e| same(&e.path, path)));
                let range = format!("{}..{}", w.base, w.branch);
                json!({
                    "session": a.name, "status": status, "path": a.cwd,
                    "branch": w.branch, "base": w.base, "exists": path.is_dir(),
                    "checked_out": entry.as_ref().and_then(|e| e.branch.clone()),
                    "dirty": git(path, &["status", "--porcelain"]).ok().map(|s| s.lines().count()),
                    "ahead": git(repo, &["rev-list", "--count", &range]).ok().and_then(|n| n.parse::<u64>().ok()),
                    "merged": merged(repo, w),
                    "locked": entry.as_ref().map(|e| e.locked),
                })
            })
            .collect())
    }
    /// Remove a worktree session only when nothing can be lost: no run, the worktree is unlocked,
    /// still on its branch and clean, and the branch is merged into its base. The folder and its own
    /// git record (`.git/worktrees/<id>`) move to ~/.Trash side by side, the merged branch is deleted
    /// (`git branch -d`, kept if git refuses), and the session is unregistered. No `git worktree
    /// prune`: it is repo-wide and permanent, so it would also drop the record of any other worktree
    /// whose folder is away at that moment, and a worktree's submodule data lives in its record.
    pub fn worktree_remove(&self, name: &str) -> Result<Value> {
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        let a = self.by_name(&r, name)?.clone();
        let w = a
            .worktree
            .clone()
            .with_context(|| format!("{name} has no worktree from session add --worktree"))?;
        ensure!(
            !r.runs.iter().any(|x| x.agent_id == a.id),
            "has_run: {name} still has a run; stop it first (pctl agent terminate)"
        );
        let (repo, path) = (Path::new(&w.repo), Path::new(&a.cwd));
        // Any symlink on the way (the folder itself or a parent): git would answer for wherever it
        // points, so the wrong folder's record could move.
        ensure!(
            fs::canonicalize(path).map_or(true, |c| c == path),
            "symlink: {} now goes through a symbolic link; nothing removed",
            a.cwd
        );
        ensure!(
            path.is_dir(),
            "missing: {} is gone, so nothing can be checked; nothing removed. Put the folder back, or clean up with git yourself",
            a.cwd
        );
        let list = entries(repo)?;
        let e = list
            .iter()
            .skip(1)
            .find(|e| same(&e.path, path))
            .with_context(|| {
                format!(
                    "git does not list {} as a worktree of {}; not touching it",
                    a.cwd, w.repo
                )
            })?;
        ensure!(
            !e.locked,
            "locked: {} is locked; `git worktree unlock` it first",
            a.cwd
        );
        ensure!(
            e.branch.as_deref() == Some(w.branch.as_str()),
            "moved: {} has {} checked out, not {}",
            a.cwd,
            e.branch.as_deref().unwrap_or("a detached HEAD"),
            w.branch
        );
        let dirty = git(path, &["status", "--porcelain"])?;
        ensure!(
            dirty.is_empty(),
            "dirty: {} has uncommitted changes:\n{}",
            a.cwd,
            first_lines(&dirty)
        );
        let admin = PathBuf::from(git(path, &["rev-parse", "--absolute-git-dir"])?);
        let common = path.join(git(path, &["rev-parse", "--git-common-dir"])?);
        ensure!(
            admin.is_dir()
                && admin
                    .parent()
                    .is_some_and(|p| same(&p.to_string_lossy(), &common.join("worktrees"))),
            "{} is not a linked worktree's own git record; not touching it",
            admin.display()
        );
        ensure!(
            merged(repo, &w),
            "unmerged: {} is not merged into {} (or is gone); nothing removed",
            w.branch,
            w.base
        );
        let trash = PathBuf::from(std::env::var_os("HOME").context("HOME")?).join(".Trash");
        fs::create_dir_all(&trash)?;
        let folder = path.file_name().context("worktree folder name")?;
        let to = trash.join(format!("{}-{}", folder.to_string_lossy(), now()));
        let git_to = to.with_file_name(format!(
            "{}.gitdir",
            to.file_name().context("name")?.to_string_lossy()
        ));
        fs::rename(path, &to).with_context(|| format!("moving {} to {}", a.cwd, to.display()))?;
        fs::rename(&admin, &git_to).with_context(|| {
            format!(
                "{} is in {}, but its git record {} did not move; put the folder back, or move the record yourself",
                a.cwd,
                to.display(),
                admin.display()
            )
        })?;
        let branch = git(repo, &["branch", "-d", &w.branch]);
        r.agents.retain(|x| x.id != a.id);
        self.save(&r)?;
        self.audit(
            "worktree_remove",
            "-",
            &format!("{name}: worktree to trash, unregistered"),
        )?;
        Ok(json!({
            "removed": name, "trashed": to, "trashed_git_dir": git_to, "branch": w.branch,
            "branch_deleted": branch.is_ok(),
            "branch_kept_because": branch.err().map(|e| e.to_string()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_goes_into_a_private_server_environment() {
        let v = with_model(vec!["/bin/opencode".into()], "opencode/glm-5.3-flash-free").unwrap();
        assert_eq!(
            v,
            [
                "env",
                r#"OPENCODE_CONFIG_CONTENT={"model":"opencode/glm-5.3-flash-free"}"#,
                "/bin/opencode",
                "--standalone"
            ]
        );
        let v = with_model(vec!["opencode".into(), "--standalone".into()], "a/b#high").unwrap();
        assert_eq!(v.iter().filter(|a| *a == "--standalone").count(), 1);
        assert!(with_model(vec!["claude".into()], "a/b").is_err());
        assert!(
            with_model(
                vec!["opencode".into(), "--server".into(), "x".into()],
                "a/b"
            )
            .is_err()
        );
        for bad in ["glm", "/m", "p/", "a/b c", "a/b\"", "a/b;rm"] {
            assert!(with_model(vec!["opencode".into()], bad).is_err(), "{bad}");
        }
    }
}
