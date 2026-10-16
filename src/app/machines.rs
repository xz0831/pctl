//! The computers one app shows: this one first, then each enabled saved machine. A remote one is
//! driven through a `Remote` (the bridge on that computer); nothing here waits for it, so a down or
//! slow machine cannot hold up the drawing or the local group.
use super::Update;
use crate::bridge::{Remote, RemoteState};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

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
}

impl Mach {
    pub fn local(label: String) -> Self {
        let mut m = Self::new(String::new(), label, String::new(), None);
        m.local = true;
        m
    }

    /// A machine from the machines file. Returns as soon as ssh exists; the hello is read in the
    /// background. A start that fails is a machine that is down, retried like any other.
    pub fn remote(label: &str, target: &str) -> Self {
        let (remote, down) = match Remote::connect(target) {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let mut m = Self::new(label.into(), label.into(), target.into(), remote);
        m.down = down;
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
        }
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
        let mut fresh = Self::remote(&self.id, &self.target);
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
        (self.down != before || update.is_some(), update)
    }

    /// Whether something is in flight, so the loop should look again soon.
    pub fn waiting(&self) -> bool {
        self.list.is_some()
    }
}
