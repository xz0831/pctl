//! The computers one app shows: this one first, then each enabled saved machine. A remote one is
//! driven through a `Remote` (the bridge on that computer); nothing here waits for it, so a down or
//! slow machine cannot hold up the drawing or the local group.
use super::Update;
use crate::bridge::{Remote, RemoteState};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// The log of a computer's connection changes stays about this long; the newest lines are kept.
const LOG_CAP: u64 = 1 << 20;

/// What the app tells a bridge to push for one cell: agent, run, width, height, scroll.
pub type Watched = (String, String, u16, u16, usize);

/// How a computer's screens reach the app. `capable` is None until the bridge has answered a
/// `watch`; Some(false) (an older bridge) keeps today's polling for that computer.
#[derive(Default)]
struct Push {
    capable: Option<bool>,
    /// The connection the state below belongs to (`Remote::generation`).
    generation: u64,
    /// The cells the bridge was last told to watch, with the revision of each one's settings.
    sent: HashMap<u32, (u64, Watched)>,
    next_rev: u64,
    inflight: Option<mpsc::Receiver<Result<Value>>>,
    retry_at: Option<Instant>,
}

/// How often a machine that is up is asked for its list.
const LIST_EVERY: Duration = Duration::from_millis(1500);
/// First wait before reconnecting a machine that went down; doubles up to the longest.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_LONGEST: Duration = Duration::from_secs(60);
/// Restarting an agent can wait for it to finish its work, so actions get longer than a screen read.
pub const ACT_WITHIN: Duration = Duration::from_secs(30);

/// An agent or a project: the computer it is on ("" is this one) and its name. Names repeat across
/// computers, so every map that holds either is keyed by both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Id {
    pub machine: String,
    pub name: String,
}

impl Id {
    pub fn new(machine: &str, name: &str) -> Self {
        Self {
            machine: machine.into(),
            name: name.into(),
        }
    }
}

pub struct Mach {
    /// "" for this computer, else the label in the machines file.
    pub id: String,
    /// What the sidebar shows: the short host name here, the label elsewhere.
    pub label: String,
    local: bool,
    target: String,
    remote: Option<Remote>,
    /// Why a remote machine is not usable, while it is not.
    pub down: Option<String>,
    pub projects: Vec<(String, String)>,
    /// Whether a pctl server runs there.
    pub connected: bool,
    pub loaded: bool,
    /// The one `list` in flight, if any.
    list: Option<mpsc::Receiver<Result<Value>>>,
    next_list: Instant,
    next_retry: Instant,
    delay: Duration,
    push: Push,
    /// Where connection changes are written, and the last state written.
    log: Option<PathBuf>,
    logged: String,
    /// Wakes the app's loop when a line comes in from the bridge (`true`: a pushed event).
    waker: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

impl Mach {
    pub fn local(label: String) -> Self {
        let mut m = Self::new(String::new(), label, String::new(), None);
        m.local = true;
        m
    }

    /// A machine from the machines file. Returns as soon as ssh exists; the hello is read in the
    /// background. A start that fails is a machine that is down, retried like any other.
    pub fn remote(label: &str, target: &str, log: PathBuf) -> Self {
        let (remote, down) = match Remote::connect(target) {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let mut m = Self::new(label.into(), label.into(), target.into(), remote);
        m.down = down;
        m.log = Some(log);
        m
    }

    fn new(id: String, label: String, target: String, remote: Option<Remote>) -> Self {
        let now = Instant::now();
        Self {
            id,
            label,
            local: false,
            target,
            remote,
            down: None,
            projects: vec![],
            connected: false,
            loaded: false,
            list: None,
            next_list: now,
            next_retry: now + RETRY_FIRST,
            delay: RETRY_FIRST,
            push: Push::default(),
            log: None,
            logged: "connecting".into(),
            waker: None,
        }
    }

    /// Have `f` called from the reader thread for every line the bridge sends.
    pub fn set_waker(&mut self, f: Arc<dyn Fn(bool) + Send + Sync>) {
        if let Some(r) = &self.remote {
            r.set_waker(f.clone());
        }
        self.waker = Some(f);
    }

    /// One line in `app-machines.log`: UTC time, the computer, what changed, and why.
    fn note(&self, what: &str, why: &str) {
        let Some(path) = &self.log else { return };
        let when = crate::utc_now();
        let clean = |s: &str| s.replace(['\n', '\t'], " ");
        crate::append_capped(
            path,
            &format!(
                "{when}\t{}\t{}\t{}",
                clean(&self.label),
                clean(what),
                clean(why)
            ),
            LOG_CAP,
        );
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    /// Why this machine cannot be used right now, or None when it can.
    pub fn unusable(&self) -> Option<String> {
        if self.is_local() {
            return None;
        }
        match self.remote.as_ref().map(Remote::state) {
            Some(RemoteState::Up) => None,
            Some(RemoteState::Connecting) => Some("connecting".into()),
            Some(RemoteState::Down(why)) => Some(why),
            None => Some(self.down.clone().unwrap_or_else(|| "not connected".into())),
        }
    }

    /// Send one request; Err is the reason it cannot be sent (down, connecting).
    pub fn request(
        &self,
        op: Value,
        within: Option<Duration>,
    ) -> std::result::Result<mpsc::Receiver<Result<Value>>, String> {
        if let Some(why) = self.unusable() {
            return Err(why);
        }
        let remote = self.remote.as_ref().ok_or("not connected")?;
        Ok(match within {
            Some(w) => remote.request_within(op, w),
            None => remote.request(op),
        })
    }

    /// Ask for the list at the next tick.
    pub fn poke(&mut self) {
        self.next_list = Instant::now();
    }

    /// Start a new connection now and forget the wait.
    pub fn reconnect(&mut self) {
        self.note("reconnect", "asked for by the person");
        self.delay = RETRY_FIRST;
        self.next_retry = Instant::now() + RETRY_FIRST;
        self.list = None;
        match &self.remote {
            Some(r) => {
                if let Err(e) = r.reconnect() {
                    self.down = Some(format!("{e:#}"));
                }
            }
            None => self.start_again(),
        }
    }

    /// A new `Remote` for a machine whose start failed, keeping what was last listed.
    fn start_again(&mut self) {
        self.note("reconnect", "starting a new ssh connection");
        let mut fresh = Self::remote(&self.id, &self.target, self.log.clone().unwrap_or_default());
        fresh.log = self.log.take();
        if let Some(f) = self.waker.take() {
            fresh.set_waker(f);
        }
        fresh.logged = std::mem::take(&mut self.logged);
        fresh.projects = std::mem::take(&mut self.projects);
        fresh.connected = self.connected;
        fresh.loaded = self.loaded;
        *self = fresh;
    }

    /// Keep a remote machine going: note when it goes down or comes back, retry with backoff, and
    /// run the one list request at a time. Whether anything to draw changed, and a list that arrived.
    pub fn tick(&mut self) -> (bool, Option<Update>) {
        if self.is_local() {
            return (false, None);
        }
        let now = Instant::now();
        let before = self.down.clone();
        match self.remote.as_ref().map(Remote::state) {
            Some(RemoteState::Down(why)) => {
                self.down = Some(why);
                self.list = None;
                if now >= self.next_retry {
                    self.delay = (self.delay * 2).min(RETRY_LONGEST);
                    self.next_retry = now + self.delay;
                    self.note(
                        "reconnect",
                        &format!(
                            "retry after the failure; next in {} s",
                            self.delay.as_secs()
                        ),
                    );
                    if let Some(r) = &self.remote {
                        let _ = r.reconnect();
                    }
                }
            }
            Some(RemoteState::Up) => {
                self.down = None;
                self.delay = RETRY_FIRST;
                self.next_retry = now + RETRY_FIRST;
            }
            Some(RemoteState::Connecting) => {}
            None => {
                self.down.get_or_insert_with(|| "not connected".into());
                if now >= self.next_retry {
                    self.start_again();
                    self.next_retry = now + RETRY_LONGEST;
                }
            }
        }
        let mut update = None;
        if let Some(rx) = &self.list {
            match rx.try_recv() {
                Ok(answer) => {
                    self.list = None;
                    self.next_list = now + LIST_EVERY;
                    update = Some(Update::from_list(&self.id, answer));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.list = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else if self.unusable().is_none() && now >= self.next_list {
            self.list = self.request(json!({"op": "list"}), None).ok();
            self.next_list = now + LIST_EVERY;
        }
        let (state, why) = match self.unusable() {
            None => ("up", String::new()),
            Some(w) if w == "connecting" => ("connecting", String::new()),
            Some(w) => ("down", w),
        };
        if state != self.logged {
            let change = format!("{} -> {state}", self.logged);
            self.note(&change, &why);
            self.logged = state.into();
        }
        (self.down != before || update.is_some(), update)
    }

    /// Whether this computer's screens arrive by push (its bridge answered `watch`).
    pub fn pushing(&self) -> bool {
        self.push.capable == Some(true)
    }

    /// Tell the bridge which cells are on screen: nothing when the set is the one it has, the
    /// whole set when anything in it changed. An older bridge answers `watch` with an error, and
    /// from then on this computer is polled as before.
    pub fn watch(&mut self, want: &[(u32, Watched)]) {
        let Some(remote) = &self.remote else { return };
        let now = Instant::now();
        if remote.generation() != self.push.generation || self.unusable().is_some() {
            // A new connection has watched nothing yet, and may be another pctl.
            self.push = Push {
                generation: remote.generation(),
                ..Push::default()
            };
        }
        if let Some(rx) = &self.push.inflight {
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return,
                Ok(Ok(v)) if v["ok"] == json!(true) => self.push.capable = Some(true),
                Ok(Ok(v))
                    if v["error"]
                        .as_str()
                        .is_some_and(|e| e.contains("unknown op")) =>
                {
                    self.push.capable = Some(false)
                }
                _ => {
                    self.push.sent.clear();
                    self.push.retry_at = Some(now + Duration::from_secs(1));
                }
            }
            self.push.inflight = None;
        }
        if self.push.capable == Some(false)
            || self.unusable().is_some()
            || self.push.retry_at.is_some_and(|t| now < t)
        {
            return;
        }
        let mut next = HashMap::new();
        for (key, cell) in want {
            let rev = match self.push.sent.get(key) {
                Some((rev, old)) if old == cell => *rev,
                _ => {
                    self.push.next_rev += 1;
                    self.push.next_rev
                }
            };
            next.insert(*key, (rev, cell.clone()));
        }
        let same = next.len() == self.push.sent.len()
            && next
                .iter()
                .all(|(k, (r, _))| self.push.sent.get(k).is_some_and(|(o, _)| o == r));
        if same {
            return;
        }
        let cells: Vec<Value> = next
            .iter()
            .map(|(key, (rev, (agent, run, width, height, scroll)))| {
                json!({"key": key.to_string(), "rev": rev, "agent": agent, "run": run,
                    "width": width, "height": height, "scroll": scroll})
            })
            .collect();
        if let Ok(rx) = self.request(json!({"op": "watch", "cells": cells}), None) {
            self.push.inflight = Some(rx);
            self.push.sent = next;
        }
    }

    /// The screens and changes the bridge pushed, as `(cell, event)`. Events for settings the app
    /// has since changed are dropped.
    pub fn pushed(&mut self) -> Vec<(u32, Value)> {
        let Some(remote) = &self.remote else {
            return vec![];
        };
        remote
            .events()
            .into_iter()
            .filter_map(|ev| {
                let key: u32 = ev["key"].as_str()?.parse().ok()?;
                let (rev, _) = self.push.sent.get(&key)?;
                (ev["rev"].as_u64() == Some(*rev)).then_some((key, ev))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_connection_log_is_private_and_keeps_its_newest_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = dir.path().join("app-machines.log");
        for i in 0..200 {
            crate::append_capped(&log, &format!("line {i:04} {}", "x".repeat(40)), 2000);
        }
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.len() <= 2000 + 60, "capped: {}", text.len());
        assert!(text.ends_with(&format!("line 0199 {}\n", "x".repeat(40))));
        assert!(text.lines().all(|l| l.starts_with("line ")), "no cut line");
        let mode = std::fs::metadata(&log).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_machine_that_cannot_connect_logs_its_change_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = dir.path().join("app-machines.log");
        let mut m = Mach::new("rm".into(), "rm".into(), "t".into(), None);
        m.log = Some(log.clone());
        m.tick();
        m.tick();
        let text = std::fs::read_to_string(&log).expect("log");
        let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0][1..], ["rm", "connecting -> down", "not connected"]);
    }
}
