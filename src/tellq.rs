//! Messages kept for a session that could not take them because it waits for a human decision.
//!
//! One folder per recipient under `<state>/tell/queue/` (0700), one file per message (0600, named
//! by time, so the order is the name order). There is no daemon: whoever reads the session's status
//! (`session list` — the app polls it —, `session get`, `session wait`) or tells it something calls
//! [`deliver`], which types the kept messages in, in order, once each, through `session_prompt`
//! (same run, never into a dialog). A message whose run changed, or one that was being sent when
//! the sender died, moves to `<state>/tell/dropped/` with the reason and is counted by
//! `tell_dropped` in `session get`; nothing is discarded silently.
use crate::Core;
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// A name as a folder name: letters, digits, `-` and `_` stay, everything else is `%XX`.
fn folder(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
fn queue_dir(state: &Path, name: &str) -> PathBuf {
    state.join("tell").join("queue").join(folder(name))
}
fn dropped_dir(state: &Path, name: &str) -> PathBuf {
    state.join("tell").join("dropped").join(folder(name))
}
fn count(dir: &Path, ext: &str) -> usize {
    fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == ext))
            .count()
    })
}

/// Messages waiting for this session.
pub fn len(state: &Path, name: &str) -> usize {
    count(&queue_dir(state, name), "json") + count(&queue_dir(state, name), "sending")
}
/// Strict metadata-only queue read for refresh observation; filesystem errors are unknown.
pub(crate) fn observation(state: &Path, name: &str) -> anyhow::Result<Value> {
    let mut files = Vec::new();
    match fs::read_dir(queue_dir(state, name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let n = entry.file_name();
                if n == ".lock" {
                    continue;
                }
                files.push(
                    n.into_string()
                        .map_err(|_| anyhow::anyhow!("unreadable tell queue filename"))?,
                );
            }
        }
    }
    files.sort();
    let has_dropped = match fs::read_dir(dropped_dir(state, name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
        Ok(mut entries) => entries.next().transpose()?.is_some(),
    };
    let unresolved = has_dropped || files.iter().any(|f| !f.ends_with(".json"));
    Ok(json!({"pending":files.len(),"unresolved":unresolved,"files":files}))
}
/// Messages that could not be delivered (see the module text).
pub fn dropped(state: &Path, name: &str) -> usize {
    count(&dropped_dir(state, name), "json")
}

fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}
fn write_private(file: &Path, body: &str) -> io::Result<()> {
    let tmp = file.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, file)
}

/// Keep `text` (sender line included) for the session's current `run`. Returns how many wait now.
pub fn push(state: &Path, name: &str, run: &str, text: &str) -> io::Result<usize> {
    let dir = queue_dir(state, name);
    private_dir(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let file = dir.join(format!("{nanos:040}-{}.json", std::process::id()));
    write_private(&file, &json!({"run": run, "text": text}).to_string())?;
    Ok(len(state, name))
}

/// Move a message out of the queue, with why.
fn drop_to(state: &Path, name: &str, file: &Path, reason: &str) {
    let mut item: Value = fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    item["reason"] = json!(reason);
    let dir = dropped_dir(state, name);
    let base = file.file_stem().map(|s| s.to_string_lossy().into_owned());
    let to = dir.join(format!("{}.json", base.unwrap_or_default()));
    if private_dir(&dir).is_ok() && write_private(&to, &item.to_string()).is_ok() {
        let _ = fs::remove_file(file);
    }
}

/// Type the kept messages into the session if it can take them: idle (or, for a sender that is
/// about to type anyway, working). One caller delivers at a time; the others return at once.
pub fn deliver(core: &Core, name: &str, allow_working: bool) {
    let dir = queue_dir(&core.state, name);
    if len(&core.state, name) == 0 {
        return;
    }
    let Ok(lock) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(".lock"))
    else {
        return;
    };
    if lock.try_lock_exclusive().is_err() {
        return;
    }
    let Ok(current) = core.session_get(name) else {
        return;
    };
    let Some(run) = current["run_id"].as_str() else {
        return;
    };
    if core.refresh_queue_event(name, run).is_err() {
        return;
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    // We hold the lock, so a file being sent belongs to a sender that died: unknown if it arrived.
    for f in files
        .iter()
        .filter(|f| f.extension().is_some_and(|x| x == "sending"))
    {
        drop_to(
            &core.state,
            name,
            f,
            "interrupted while being typed in; it may or may not have arrived",
        );
        crate::tell::log(core, "queue", name, "dropped", 0);
    }
    let Ok(view) = core.session_get(name) else {
        return;
    };
    let status = view["status"].as_str().unwrap_or_default();
    if status != "idle" && !(allow_working && status == "working") {
        return;
    }
    for file in files
        .iter()
        .filter(|f| f.extension().is_some_and(|x| x == "json"))
    {
        let item: Option<Value> = fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let (Some(run), Some(text)) = (
            item.as_ref().and_then(|i| i["run"].as_str()),
            item.as_ref().and_then(|i| i["text"].as_str()),
        ) else {
            drop_to(&core.state, name, file, "unreadable");
            continue;
        };
        if view["run_id"].as_str() != Some(run) {
            drop_to(
                &core.state,
                name,
                file,
                "the session was restarted while it waited (another run)",
            );
            crate::tell::log(core, "queue", name, "dropped", text.len());
            continue;
        }
        let sending = file.with_extension("sending");
        if fs::rename(file, &sending).is_err() {
            return;
        }
        if core.session_prompt(name, text).is_err() {
            // Still blocked, or changed under us: keep it, try at the next read.
            let _ = fs::rename(&sending, file);
            return;
        }
        let _ = fs::remove_file(&sending);
        crate::tell::log(core, "queue", name, "delivered", text.len());
    }
}

/// After a status read: a session that is idle gets its kept messages, and the view shows what is
/// left. The status in `view` is the one read before the messages were typed in, so it can be one
/// read behind.
pub fn after_read(core: &Core, view: &mut Value) {
    let Some(name) = view["name"].as_str().map(String::from) else {
        return;
    };
    if view["tell_queue"].as_u64().unwrap_or(0) == 0 {
        return;
    }
    if view["status"] == "idle" {
        deliver(core, &name, false);
        view["tell_queue"] = json!(len(&core.state, &name));
        let d = dropped(&core.state, &name);
        if d > 0 {
            view["tell_dropped"] = json!(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pctl-tellq-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_become_safe_folders() {
        assert_eq!(folder("a-main_1"), "a-main_1");
        assert_eq!(folder("../x y"), "%2E%2E%2Fx%20y");
    }

    #[test]
    fn messages_are_kept_privately_and_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let st = temp();
        assert_eq!(len(&st, "w"), 0);
        for i in 0..3 {
            push(&st, "w", "run1", &format!("m{i}")).unwrap();
        }
        assert_eq!(len(&st, "w"), 3);
        assert_eq!(len(&st, "other"), 0);
        let dir = queue_dir(&st, "w");
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        names.sort_by_key(|e| e.file_name());
        let texts: Vec<String> = names
            .iter()
            .map(|e| {
                assert_eq!(e.metadata().unwrap().permissions().mode() & 0o777, 0o600);
                let v: Value =
                    serde_json::from_str(&fs::read_to_string(e.path()).unwrap()).unwrap();
                v["text"].as_str().unwrap().into()
            })
            .collect();
        assert_eq!(texts, ["m0", "m1", "m2"]);
        let first = names[0].path();
        drop_to(&st, "w", &first, "why");
        assert_eq!((len(&st, "w"), dropped(&st, "w")), (2, 1));
        fs::remove_dir_all(st).unwrap();
    }
}
