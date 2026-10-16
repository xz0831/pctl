//! Saved other computers: `pctl machine ...` and the `--machine <label>` shortcut. pctl owns this
//! one file itself — `$XDG_CONFIG_HOME/pctl/machines`, else `~/.config/pctl/machines`, the folder
//! `app::keys::config_path` reads `config.toml` from. `config.toml` is never written here.
use crate::bridge::{Remote, RemoteState};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

/// How long `machine status` and `machine add` wait for a hello, and for one answer.
const WAIT: Duration = Duration::from_secs(15);
const ANSWER: Duration = Duration::from_secs(6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub label: String,
    pub target: String,
    pub enabled: bool,
}

/// The machines file. `None` only when neither `XDG_CONFIG_HOME` nor `HOME` is usable.
pub fn path() -> Option<PathBuf> {
    crate::app::keys::config_path().map(|p| p.with_file_name("machines"))
}

/// Labels are letters, digits and `-`, `_`, `.` — no tabs, no spaces, no empty label.
pub fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `label`, `ssh-target`, `enabled|disabled`, one machine per line, tab separated.
fn parse(text: &str) -> Result<Vec<Machine>> {
    let mut machines = vec![];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let at = n + 1;
        ensure!(
            f.len() == 3,
            "machines line {at}: expected label, ssh target and enabled or disabled"
        );
        ensure!(
            valid_label(f[0]),
            "machines line {at}: bad label {:?}; labels hold letters, digits, - _ .",
            f[0]
        );
        ensure!(!f[1].is_empty(), "machines line {at}: empty ssh target");
        let enabled = match f[2] {
            "enabled" => true,
            "disabled" => false,
            other => bail!("machines line {at}: {other:?} is not enabled or disabled"),
        };
        ensure!(
            !machines.iter().any(|m: &Machine| m.label == f[0]),
            "machines line {at}: {} appears twice",
            f[0]
        );
        machines.push(Machine {
            label: f[0].into(),
            target: f[1].into(),
            enabled,
        });
    }
    Ok(machines)
}

fn render(machines: &[Machine]) -> String {
    machines
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}\n",
                m.label,
                m.target,
                if m.enabled { "enabled" } else { "disabled" }
            )
        })
        .collect()
}

fn path_or_err() -> Result<PathBuf> {
    path().context("no config folder; set HOME or XDG_CONFIG_HOME")
}

pub fn load() -> Result<Vec<Machine>> {
    load_at(&path_or_err()?)
}

fn load_at(path: &Path) -> Result<Vec<Machine>> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub fn save(machines: &[Machine]) -> Result<()> {
    save_at(&path_or_err()?, machines)
}

/// Replace the file in one step: a temp file beside it, then a rename, so a reader never sees half
/// a line and a crash never leaves the list truncated.
fn save_at(path: &Path, machines: &[Machine]) -> Result<()> {
    let dir = path.parent().context("machines file has no folder")?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(render(machines).as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn find<'a>(machines: &'a [Machine], label: &str) -> Result<&'a Machine> {
    machines
        .iter()
        .find(|m| m.label == label)
        .with_context(|| format!("machine {label} not found; see `pctl machine list`"))
}

fn find_mut<'a>(machines: &'a mut [Machine], label: &str) -> Result<&'a mut Machine> {
    machines
        .iter_mut()
        .find(|m| m.label == label)
        .with_context(|| format!("machine {label} not found; see `pctl machine list`"))
}

/// The ssh target `--machine <label>` uses.
pub fn resolve(label: &str) -> Result<String> {
    let machines = load()?;
    let m = find(&machines, label)?;
    ensure!(
        m.enabled,
        "machine {label} is disabled; `pctl machine enable {label}` first"
    );
    Ok(m.target.clone())
}

/// Save a machine, but only after its bridge answers the hello.
pub fn add(label: &str, target: &str) -> Result<Value> {
    ensure!(
        valid_label(label),
        "label {label:?} holds something other than letters, digits, - _ ."
    );
    ensure!(
        !target.trim().is_empty(),
        "the ssh target must not be blank"
    );
    ensure!(
        !target.contains(['\t', '\n', '\r']),
        "the ssh target must be one line"
    );
    let mut machines = load()?;
    ensure!(
        !machines.iter().any(|m| m.label == label),
        "machine {label} already exists"
    );
    let remote = Remote::connect(target)?;
    let hello = match remote.wait_ready(WAIT) {
        RemoteState::Up => remote.hello().unwrap_or_default(),
        RemoteState::Connecting => bail!("machine {label}: no hello from {target}"),
        RemoteState::Down(reason) => bail!("machine {label}: {target}: {reason}"),
    };
    machines.push(Machine {
        label: label.into(),
        target: target.into(),
        enabled: true,
    });
    save(&machines)?;
    Ok(json!({
        "label": label, "target": target, "state": "enabled",
        "host": hello.host, "version": hello.version,
    }))
}

pub fn list() -> Result<Value> {
    let machines = load()?;
    Ok(json!({
        "machines": machines
            .iter()
            .map(|m| json!({"label": m.label, "target": m.target, "enabled": m.enabled}))
            .collect::<Vec<_>>(),
    }))
}

/// Ask every enabled machine over the bridge who it is: reachable, host, pctl version, whether a
/// pctl server runs there and how many agents it holds.
pub fn status() -> Result<Value> {
    let machines = load()?;
    Ok(json!({
        "machines": machines.iter().filter(|m| m.enabled).map(probe).collect::<Vec<_>>(),
    }))
}

fn probe(m: &Machine) -> Value {
    let mut row = json!({
        "label": m.label, "target": m.target, "reachable": false, "state": "connecting",
        "host": Value::Null, "version": Value::Null, "server": false, "agents": 0,
    });
    let remote = match Remote::connect(&m.target) {
        Ok(remote) => remote,
        Err(e) => {
            row["state"] = json!("down");
            row["error"] = json!(format!("{e:#}"));
            return row;
        }
    };
    let hello = match remote.wait_ready(WAIT) {
        RemoteState::Up => remote.hello().unwrap_or_default(),
        RemoteState::Connecting => {
            row["state"] = json!("down");
            row["error"] = json!("no hello");
            return row;
        }
        RemoteState::Down(reason) => {
            row["state"] = json!("down");
            row["error"] = json!(reason);
            return row;
        }
    };
    row["host"] = json!(hello.host);
    row["version"] = json!(hello.version);
    row["reachable"] = json!(true);
    row["state"] = json!("up");
    match remote.request(json!({"op": "list"})).recv_timeout(ANSWER) {
        Ok(Ok(list)) => {
            row["server"] = json!(list["connected"].as_bool().unwrap_or(false));
            row["agents"] = json!(list["sessions"].as_array().map_or(0, Vec::len));
        }
        Ok(Err(e)) => {
            row["error"] = json!(format!("list: {e:#}"));
            if let RemoteState::Down(_) = remote.state() {
                row["state"] = json!("down");
            }
        }
        Err(_) => row["error"] = json!("list: no answer within 6 s"),
    }
    row
}

pub fn remove(label: &str) -> Result<Value> {
    let mut machines = load()?;
    let before = machines.len();
    machines.retain(|m| m.label != label);
    ensure!(machines.len() < before, "machine {label} not found");
    save(&machines)?;
    Ok(json!({"removed": label}))
}

pub fn rename(old: &str, new: &str) -> Result<Value> {
    ensure!(
        valid_label(new),
        "label {new:?} holds something other than letters, digits, - _ ."
    );
    let mut machines = load()?;
    ensure!(
        !machines.iter().any(|m| m.label == new),
        "machine {new} already exists"
    );
    find_mut(&mut machines, old)?.label = new.into();
    save(&machines)?;
    Ok(json!({"renamed": old, "label": new}))
}

pub fn set_enabled(label: &str, enabled: bool) -> Result<Value> {
    let mut machines = load()?;
    find_mut(&mut machines, label)?.enabled = enabled;
    save(&machines)?;
    Ok(json!({
        "label": label,
        "state": if enabled { "enabled" } else { "disabled" },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(label: &str, target: &str, enabled: bool) -> Machine {
        Machine {
            label: label.into(),
            target: target.into(),
            enabled,
        }
    }

    #[test]
    fn a_list_of_machines_round_trips() {
        let machines = vec![m("rick", "rick", true), m("morty-2", "morty", false)];
        assert_eq!(parse(&render(&machines)).expect("parse"), machines);
        assert_eq!(
            render(&machines),
            "rick\trick\tenabled\nmorty-2\tmorty\tdisabled\n"
        );
        assert_eq!(parse("").expect("empty"), vec![]);
        assert_eq!(parse("\n\n").expect("blank lines"), vec![]);
    }

    #[test]
    fn only_letters_digits_and_dot_dash_underscore_are_labels() {
        for good in ["rick", "xz0831", "a.b_c-d", "9"] {
            assert!(valid_label(good), "{good}");
        }
        for bad in ["", "a b", "a\tb", "ri/ck", "리", "a\nb", "-x y"] {
            assert!(!valid_label(bad), "{bad}");
        }
    }

    #[test]
    fn a_broken_line_is_refused() {
        for bad in [
            "rick\thost",
            "rick\t\tenabled",
            "ri ck\thost\tenabled",
            "rick\thost\tmaybe",
            "rick\thost\tenabled\tx",
            "rick\thost\tenabled\nrick\tother\tenabled",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_file_is_replaced_in_one_step() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("pctl").join("machines");
        save_at(&path, &[m("rick", "rick", true)]).expect("first save");
        save_at(
            &path,
            &[m("rick", "rick", true), m("morty", "morty", false)],
        )
        .expect("second save");
        assert_eq!(load_at(&path).expect("load").len(), 2);
        let left: Vec<String> = fs::read_dir(path.parent().expect("dir"))
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            vec!["machines".to_string()],
            "no temp file is left behind"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "rick\trick\tenabled\nmorty\tmorty\tdisabled\n"
        );
    }
}
