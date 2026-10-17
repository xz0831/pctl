//! The documents agents show the person: `pctl show` records one in `docs.json` in the state
//! folder; the app lists them and `pctl view` reads them. A document is never opened for anyone.
//! Each shown file keeps its latest content and the one before it (when it changed), up to 20 MiB.
use crate::{Core, atomic_json, bridge::hostname, id, now};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// A file bigger than this is shown by path only: no copy, so no earlier version.
pub const MAX_COPY: u64 = 20 * 1024 * 1024;
/// The newest documents kept; older ones go with their copies.
const MAX_DOCS: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Doc {
    pub id: String,
    /// The project name.
    pub project: String,
    /// The session that showed it, with its run; None when a person ran `pctl show`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Canonical, on the computer named in `host`.
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub shown: u64,
    pub host: String,
    /// Not opened by the person yet (or changed since).
    pub new: bool,
    pub size: u64,
    pub mtime: u64,
    /// The version before the latest, when the file changed between two shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Previous {
    pub shown: u64,
    /// Where its content is kept; None when it was too big to copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    docs: Vec<Doc>,
}

/// Private places: the same rule the phone page uses. A hidden entry directly under the home
/// folder is private except `.config` and `.local`, whose secret corners are named.
const HOME_DOT_ALLOW: &[&str] = &[".config", ".local"];
const DENY_UNDER_HOME: &[&str] = &[
    ".config/secrets",
    ".config/gh",
    ".config/op",
    ".config/gcloud",
    ".config/github-copilot",
    "Library",
];
const DENY_ABS: &[&str] = &[
    "/Library/Keychains",
    "/private/var/db",
    "/private/etc/master.passwd",
    "/private/var/root",
];
fn deny_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == ".env"
        || n.starts_with(".env.")
        || n.ends_with(".env")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.starts_with("id_ecdsa")
        || [
            ".pem",
            ".key",
            ".p12",
            ".pfx",
            ".keychain",
            ".keychain-db",
            ".kdbx",
        ]
        .iter()
        .any(|s| n.ends_with(s))
        || n.contains("credential")
        || n.contains("secret")
        || n == "auth.json"
        || (n.contains("token") && !n.starts_with("tokenizer"))
        || n.ends_with("_history")
        || n.ends_with(".history")
}
/// `p` is canonical. Disks here do not tell `A` from `a`, so the comparison is lower case.
fn denied(p: &Path, home: &Path) -> bool {
    let low = |x: &Path| PathBuf::from(x.to_string_lossy().to_lowercase());
    let (p, home) = (low(p), low(home));
    if DENY_ABS.iter().any(|d| p.starts_with(d.to_lowercase())) {
        return true;
    }
    if let Ok(rel) = p.strip_prefix(&home) {
        if let Some(first) = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            && first.starts_with('.')
            && !HOME_DOT_ALLOW.contains(&first.as_str())
        {
            return true;
        }
        if DENY_UNDER_HOME
            .iter()
            .any(|d| rel.starts_with(d.to_lowercase()))
        {
            return true;
        }
    }
    p.components()
        .any(|c| deny_name(&c.as_os_str().to_string_lossy()))
}

/// One line of printable text, at most `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn mtime(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The session that reads document `id` in the app's document cell.
pub fn viewer_name(id: &str) -> String {
    format!("doc-{}", id.chars().take(8).collect::<String>())
}
/// Whether a session's command line is `pctl view <path>`.
pub fn is_viewer(argv: &[String]) -> bool {
    match argv {
        [exe, view, _, rest @ ..] => {
            view == "view"
                && (rest.is_empty() || rest == ["--remote"])
                && Path::new(exe).file_stem().is_some_and(|n| n == "pctl")
        }
        _ => false,
    }
}

impl Core {
    fn docs_store(&self) -> Store {
        fs::read(self.state.join("docs.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    fn docs_dir(&self, id: &str) -> PathBuf {
        self.state.join("docs").join(id)
    }
    /// Write a copy readable by this user only.
    fn docs_copy(&self, id: &str, name: &str, bytes: &[u8]) -> Result<String> {
        let dir = self.docs_dir(id);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let file = dir.join(name);
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)?
            .write_all(bytes)?;
        Ok(file.to_string_lossy().into_owned())
    }

    /// Record that `path` is shown. `by` is the calling session's name and run, when there is one.
    pub fn doc_show(
        &self,
        path: &Path,
        title: Option<&str>,
        note: Option<&str>,
        project: Option<&str>,
    ) -> Result<Doc> {
        self.doc_show_within(path, title, note, project, MAX_COPY)
    }
    fn doc_show_within(
        &self,
        path: &Path,
        title: Option<&str>,
        note: Option<&str>,
        project: Option<&str>,
        cap: u64,
    ) -> Result<Doc> {
        let canon = fs::canonicalize(path)
            .with_context(|| format!("cannot show {}: no such file or folder", path.display()))?;
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        ensure!(
            !denied(&canon, &home) && !canon.starts_with(&self.state),
            "cannot show {}: a private place",
            path.display()
        );
        let meta = fs::metadata(&canon)?;
        let me = self.session_whoami().ok();
        let by = me
            .as_ref()
            .and_then(|v| v["name"].as_str())
            .map(String::from);
        let run = me
            .as_ref()
            .filter(|v| v["current_run"] == true)
            .and_then(|_| std::env::var("PCTL_RUN_ID").ok());
        let project = match (project, &by) {
            (Some(p), _) => {
                let r = self.registry()?;
                r.projects
                    .iter()
                    .find(|x| x.id == p || x.name == p)
                    .map(|x| x.name.clone())
                    .with_context(|| format!("project not found: {p}"))?
            }
            (None, Some(name)) => self.session_get(name)?["project"]
                .as_str()
                .context("the session has no project")?
                .to_string(),
            (None, None) => bail!("not inside a session; name the project with --project"),
        };
        let bytes = if meta.is_file() && meta.len() <= cap {
            Some(fs::read(&canon)?)
        } else {
            None
        };
        let given = title.map(|t| clean(t, 120)).filter(|t| !t.is_empty());
        let title = given.clone().unwrap_or_else(|| {
            canon
                .file_name()
                .map_or_else(|| "/".into(), |n| clean(&n.to_string_lossy(), 120))
        });
        let note = note.map(|n| clean(n, 500)).filter(|n| !n.is_empty());
        let (size, modified) = (meta.len(), mtime(&meta));
        let path = canon.to_string_lossy().into_owned();

        let _lock = self.lock()?;
        let mut store = self.docs_store();
        let doc = match store
            .docs
            .iter_mut()
            .find(|d| d.path == path && d.project == project)
        {
            Some(d) => {
                let latest = self.docs_dir(&d.id).join("latest");
                let changed = meta.is_file()
                    && match (&bytes, fs::read(&latest)) {
                        (Some(now), Ok(then)) => *now != then,
                        _ => (d.size, d.mtime) != (size, modified),
                    };
                if changed {
                    let prev = self.docs_dir(&d.id).join("previous");
                    // No latest copy (it was too big) leaves nothing to keep.
                    let kept = fs::rename(&latest, &prev).is_ok();
                    if !kept {
                        let _ = fs::remove_file(&prev);
                    }
                    d.previous = Some(Previous {
                        shown: d.shown,
                        copy: kept.then(|| prev.to_string_lossy().into_owned()),
                    });
                    if let Some(b) = &bytes {
                        self.docs_copy(&d.id, "latest", b)?;
                    }
                    d.new = true;
                }
                if let Some(n) = &note {
                    d.note = Some(n.clone());
                }
                if let Some(t) = &given {
                    d.title = t.clone();
                }
                d.by = by;
                d.run = run;
                d.shown = now();
                (d.size, d.mtime) = (size, modified);
                d.clone()
            }
            None => {
                let d = Doc {
                    id: id(),
                    project,
                    by,
                    run,
                    path,
                    title,
                    note,
                    shown: now(),
                    host: hostname(),
                    new: true,
                    size,
                    mtime: modified,
                    previous: None,
                };
                if let Some(b) = &bytes {
                    self.docs_copy(&d.id, "latest", b)?;
                }
                store.docs.push(d.clone());
                d
            }
        };
        store.docs.sort_by_key(|d| std::cmp::Reverse(d.shown));
        for gone in store.docs.split_off(store.docs.len().min(MAX_DOCS)) {
            let _ = fs::remove_dir_all(self.docs_dir(&gone.id));
        }
        atomic_json(&self.state.join("docs.json"), &store)?;
        Ok(doc)
    }

    /// The documents, newest first; of one project when it is named.
    pub fn doc_list(&self, project: Option<&str>) -> Vec<Doc> {
        let mut docs = self.docs_store().docs;
        docs.retain(|d| project.is_none_or(|p| d.project == p));
        docs.sort_by_key(|d| std::cmp::Reverse(d.shown));
        docs
    }

    /// Start the reader for a document in the project's tmux session and mark it read. The path is
    /// the recorded one, checked again: the file may have been replaced by a link since.
    pub fn doc_open(&self, id: &str) -> Result<String> {
        let doc = self
            .doc_list(None)
            .into_iter()
            .find(|d| d.id == id)
            .with_context(|| format!("unknown document {id}"))?;
        let canon = fs::canonicalize(&doc.path).context("the document is gone")?;
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        ensure!(
            !denied(&canon, &home) && !canon.starts_with(&self.state),
            "cannot open {}: a private place",
            doc.path
        );
        let name = viewer_name(id);
        match self.session_get(&name) {
            Ok(s) if s["process"] == "running" => {}
            old => {
                if old.is_ok() {
                    self.doc_close(&name)?;
                }
                let dir = if canon.is_dir() {
                    &canon
                } else {
                    canon.parent().unwrap_or(&canon)
                };
                let exe = std::env::current_exe()?.to_string_lossy().into_owned();
                let mut argv = vec![exe, "view".into(), canon.to_string_lossy().into_owned()];
                // Asked over ssh: the person is at another computer, so no clipboard command here.
                if std::env::var_os("SSH_CONNECTION").is_some() {
                    argv.push("--remote".into());
                }
                self.session_add(&name, dir, Some(&doc.project), argv, None, None, None)?;
            }
        }
        self.doc_read(id)?;
        Ok(name)
    }

    /// End a reader session: stop it and take it off the list. The document stays.
    pub fn doc_close(&self, name: &str) -> Result<()> {
        let s = self.session_get(name)?;
        ensure!(s["viewer"] == true, "{name} is not a document reader");
        if let (Some(agent), Some(run)) = (s["agent_id"].as_str(), s["run_id"].as_str()) {
            self.terminate(agent, run)?;
        }
        self.session_remove(name)?;
        Ok(())
    }

    /// The person opened it.
    pub fn doc_read(&self, id: &str) -> Result<Value> {
        let _lock = self.lock()?;
        let mut store = self.docs_store();
        let d = store
            .docs
            .iter_mut()
            .find(|d| d.id == id)
            .with_context(|| format!("unknown document {id}"))?;
        d.new = false;
        atomic_json(&self.state.join("docs.json"), &store)?;
        Ok(json!({"doc": id, "new": false}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> (tempfile::TempDir, Core) {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("s"), "tmux".into()).unwrap();
        c.register_project("demo", d.path()).unwrap();
        (d, c)
    }
    fn show(c: &Core, f: &Path, cap: u64) -> Doc {
        c.doc_show_within(f, None, None, Some("demo"), cap).unwrap()
    }

    #[test]
    fn a_reader_is_told_by_its_command_line() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_viewer(&argv(&["/bin/pctl", "view", "/d/a.md"])));
        assert!(is_viewer(&argv(&["pctl", "view", "/d/a.md", "--remote"])));
        assert!(!is_viewer(&argv(&["pctl", "view", "/d/a.md", "--dark"])));
        assert!(!is_viewer(&argv(&["claude", "view", "x"])));
        assert!(!is_viewer(&argv(&["pctl", "show", "x"])));
        assert_eq!(viewer_name("0123456789abcdef"), "doc-01234567");
    }

    #[test]
    fn private_places_are_refused() {
        let h = Path::new("/home/u");
        for p in [
            "/home/u/.ssh/id_ed25519",
            "/home/u/.config/secrets/api.env",
            "/home/u/p/.env.local",
            "/home/u/p/server.pem",
            "/home/u/.zsh_history",
            "/home/u/p/client_secret.json",
            "/home/U/.SSH/config",
            "/Library/Keychains/x",
        ] {
            assert!(denied(Path::new(p), h), "must be refused: {p}");
        }
        for p in [
            "/home/u/Projects/a/README.md",
            "/home/u/.config/pctl/config.toml",
            "/home/u/tokenizer.md",
        ] {
            assert!(!denied(Path::new(p), h), "must be allowed: {p}");
        }
    }

    #[test]
    fn showing_twice_keeps_one_record_and_changes_make_it_new_again() {
        let (d, c) = core();
        let f = d.path().join("a.md");
        fs::write(&f, "one").unwrap();
        let first = show(&c, &f, MAX_COPY);
        assert!(first.new && first.previous.is_none() && first.title == "a.md");
        c.doc_read(&first.id).unwrap();
        assert!(!c.doc_list(None)[0].new);
        // Same content: still one record, still read.
        let again = show(&c, &f, MAX_COPY);
        assert_eq!(again.id, first.id);
        assert!(!again.new && again.previous.is_none());
        // Changed: new again, and the old content is the previous version.
        fs::write(&f, "two").unwrap();
        let second = show(&c, &f, MAX_COPY);
        assert!(second.new);
        let copy = second.previous.unwrap().copy.unwrap();
        assert_eq!(fs::read_to_string(copy).unwrap(), "one");
        assert_eq!(
            fs::read_to_string(c.docs_dir(&first.id).join("latest")).unwrap(),
            "two"
        );
        // Changed again: only the latest and the one before it remain.
        fs::write(&f, "three").unwrap();
        let third = show(&c, &f, MAX_COPY);
        assert_eq!(
            fs::read_to_string(third.previous.unwrap().copy.unwrap()).unwrap(),
            "two"
        );
        assert_eq!(c.doc_list(None).len(), 1);
        assert_eq!(fs::read_dir(c.docs_dir(&first.id)).unwrap().count(), 2);
    }

    #[test]
    fn a_big_file_is_shown_by_path_only() {
        let (d, c) = core();
        let f = d.path().join("big.md");
        fs::write(&f, "0123456789").unwrap();
        let first = c.doc_show_within(&f, None, None, Some("demo"), 5).unwrap();
        assert!(!c.docs_dir(&first.id).exists(), "no copy over the cap");
        fs::write(&f, "0123456789abc").unwrap();
        let second = c.doc_show_within(&f, None, None, Some("demo"), 5).unwrap();
        assert!(second.new, "a changed size is a change");
        assert_eq!(second.previous.unwrap().copy, None);
    }

    #[test]
    fn project_filter_title_note_and_refusals() {
        let (d, c) = core();
        let f = d.path().join("b.md");
        fs::write(&f, "x").unwrap();
        let doc = c
            .doc_show(
                &f,
                Some("A\ntitle"),
                Some("read the\nsecond part"),
                Some("demo"),
            )
            .unwrap();
        assert_eq!(
            (doc.title.as_str(), doc.note.as_deref()),
            ("A title", Some("read the second part"))
        );
        assert_eq!(c.doc_list(Some("demo")).len(), 1);
        assert!(c.doc_list(Some("other")).is_empty());
        assert!(
            c.doc_show(&f, None, None, None).is_err(),
            "no session and no --project"
        );
        assert!(c.doc_show(&f, None, None, Some("nope")).is_err());
        assert!(
            c.doc_show(&d.path().join("missing.md"), None, None, Some("demo"))
                .is_err()
        );
        assert!(
            c.doc_show(&c.state.join("registry.json"), None, None, Some("demo"))
                .is_err()
        );
        assert!(c.doc_read("nope").is_err());
        // A re-show without a title keeps the given one.
        assert_eq!(
            c.doc_show(&f, None, None, Some("demo")).unwrap().title,
            "A title"
        );
    }
}
