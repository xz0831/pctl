//! Observation only: no function in this module stops or starts a session.
use crate::{Core, Registry, now};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    pub enabled: bool,
    pub app_observer: bool,
    pub poll_interval_secs: u64,
    pub idle_for_secs: u64,
    pub quit_timeout_secs: u64,
    pub ready_timeout_secs: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            app_observer: true,
            poll_interval_secs: 60,
            idle_for_secs: 1800,
            quit_timeout_secs: 10,
            ready_timeout_secs: 30,
        }
    }
}
impl Policy {
    fn load() -> Result<Self> {
        let mut p = Self::default();
        if let Some(path) = crate::app::keys::config_path() {
            let text = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(p),
                Err(e) => return Err(e.into()),
            };
            for (key, value) in crate::app::keys::section(&text, "refresh") {
                match key.as_str() {
                    "enabled" => {
                        p.enabled = value
                            .parse()
                            .context("refresh.enabled must be true/false")?
                    }
                    "app_observer" => {
                        p.app_observer = value
                            .parse()
                            .context("refresh.app_observer must be true/false")?
                    }
                    "poll_interval_secs" => p.poll_interval_secs = value.parse()?,
                    "idle_for_secs" => p.idle_for_secs = value.parse()?,
                    "quit_timeout_secs" => p.quit_timeout_secs = value.parse()?,
                    "ready_timeout_secs" => p.ready_timeout_secs = value.parse()?,
                    _ => anyhow::bail!("unknown refresh setting: {key}"),
                }
            }
        }
        ensure!(
            p.poll_interval_secs > 0,
            "refresh.poll_interval_secs must be positive"
        );
        Ok(p)
    }
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct Activity {
    run_id: String,
    conversation: Option<String>,
    revision: u64,
    input_epoch: u64,
    queue_epoch: u64,
    last_activity_at: u64,
    instrumented: bool,
    turn_input_epoch: Option<u64>,
    ready: bool,
    last_hook: Option<String>,
    background_tasks: Option<usize>,
    session_crons: Option<usize>,
    tools: BTreeSet<String>,
    subagents: BTreeSet<String>,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    sessions: BTreeMap<String, Activity>,
    projects: BTreeMap<String, Window>,
}
#[derive(Default, Serialize, Deserialize)]
struct Window {
    #[serde(default)]
    epoch: String,
    cohort: Value,
    since: Option<u64>,
    last_seen: u64,
}
fn increment(n: &mut u64) -> Result<()> {
    *n = n
        .checked_add(1)
        .context("refresh epoch exhausted; observation unavailable")?;
    Ok(())
}
impl Activity {
    fn bind(&mut self, run: &str) {
        if self.run_id != run {
            *self = Self {
                run_id: run.into(),
                ..Self::default()
            };
        }
    }
    fn event(&mut self, kind: &str, at: u64) -> Result<()> {
        increment(&mut self.revision)?;
        self.last_activity_at = at;
        if ["input", "queue", "lifecycle", "launch"].contains(&kind) {
            increment(&mut self.input_epoch)?;
            self.ready = false;
        }
        if kind == "queue" {
            increment(&mut self.queue_epoch)?;
        }
        Ok(())
    }
    fn hook(&mut self, v: &Value, at: u64) -> Result<()> {
        self.event("hook", at)?;
        self.ready = false;
        let event = v["hook_event_name"].as_str().unwrap_or("");
        self.last_hook = Some(
            if [
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "PermissionRequest",
                "Notification",
                "StopFailure",
                "SessionEnd",
            ]
            .contains(&event)
            {
                event
            } else {
                "unsupported"
            }
            .into(),
        );
        match event {
            "SessionStart" => {
                self.instrumented = true;
                self.turn_input_epoch = None;
                self.tools.clear();
                self.subagents.clear();
            }
            "UserPromptSubmit" => {
                self.event("input", at)?;
                self.turn_input_epoch = Some(self.input_epoch);
            }
            "PreToolUse" | "SubagentStart" => {
                let key = if event == "PreToolUse" {
                    "tool_use_id"
                } else {
                    "agent_id"
                };
                let set = if event == "PreToolUse" {
                    &mut self.tools
                } else {
                    &mut self.subagents
                };
                if let Some(id) = v[key].as_str().filter(|s| !s.is_empty() && s.len() <= 128) {
                    if set.len() < 256 {
                        set.insert(id.into());
                    } else {
                        self.instrumented = false;
                    }
                } else {
                    self.instrumented = false;
                }
            }
            "PostToolUse" | "PostToolUseFailure" | "SubagentStop" => {
                let key = if event == "SubagentStop" {
                    "agent_id"
                } else {
                    "tool_use_id"
                };
                let set = if event == "SubagentStop" {
                    &mut self.subagents
                } else {
                    &mut self.tools
                };
                if !v[key].as_str().is_some_and(|id| set.remove(id)) {
                    self.instrumented = false;
                }
            }
            "Stop" => {
                self.background_tasks = v["background_tasks"].as_array().map(Vec::len);
                self.session_crons = v["session_crons"].as_array().map(Vec::len);
                let empty = |key: &str| v[key].as_array().is_some_and(Vec::is_empty);
                self.ready = self.instrumented
                    && self.turn_input_epoch == Some(self.input_epoch)
                    && self.tools.is_empty()
                    && self.subagents.is_empty()
                    && empty("background_tasks")
                    && empty("session_crons")
                    && v["stop_hook_active"] == false;
            }
            "PermissionRequest" | "Notification" | "StopFailure" | "SessionEnd" => {}
            _ => {
                self.instrumented = false;
            }
        }
        Ok(())
    }
}
impl Window {
    fn update(&mut self, cohort: Value, clear: bool, at: u64, policy: &Policy) -> u64 {
        let continuous = at
            .checked_sub(self.last_seen)
            .is_some_and(|gap| u128::from(gap) <= u128::from(policy.poll_interval_secs) * 2);
        if !clear {
            self.epoch = crate::id();
            self.since = None;
        } else if self.cohort != cohort || !continuous || self.since.is_none() {
            self.epoch = crate::id();
            self.since = Some(at);
        }
        self.cohort = cohort;
        self.last_seen = at;
        self.since.and_then(|s| at.checked_sub(s)).unwrap_or(0)
    }
}
impl Core {
    fn refresh_state(&self) -> Result<State> {
        let path = self.state.join("refresh-observe.json");
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
            Ok(m) => {
                ensure!(
                    m.len() <= 2 * 1024 * 1024,
                    "refresh observation capacity exceeded"
                );
                Ok(serde_json::from_slice(&fs::read(path)?)?)
            }
        }
    }
    fn save_refresh_state(&self, mut s: State, r: &Registry) -> Result<()> {
        s.sessions
            .retain(|id, _| r.agents.iter().any(|a| &a.id == id));
        s.projects
            .retain(|id, _| r.projects.iter().any(|p| &p.id == id));
        ensure!(
            serde_json::to_vec(&s)?.len() <= 2 * 1024 * 1024,
            "refresh observation capacity exceeded"
        );
        crate::atomic_json(&self.state.join("refresh-observe.json"), &s)
    }
    /// Caller holds the registry lock. Record intent before a key/queue side effect.
    pub(crate) fn refresh_event_locked(
        &self,
        r: &Registry,
        agent: &str,
        run: &str,
        kind: &str,
    ) -> Result<()> {
        ensure!(
            r.runs.iter().any(|x| x.agent_id == agent && x.id == run),
            "stale_run: activity belongs to another run"
        );
        let mut state = self.refresh_state()?;
        let a = state.sessions.entry(agent.into()).or_default();
        a.bind(run);
        if kind == "launch" {
            a.conversation = r
                .agents
                .iter()
                .find(|a| a.id == agent)
                .and_then(|a| a.conversation.clone());
        }
        a.event(kind, now())?;
        self.save_refresh_state(state, r)
    }
    pub(crate) fn refresh_queue_event(&self, name: &str, run: &str) -> Result<()> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let a = self.by_name(&r, name)?;
        self.refresh_event_locked(&r, &a.id, run, "queue")
    }
    pub(crate) fn refresh_pane_input(&self, pane: &str, run: &str) -> Result<bool> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let Some(record) = r.runs.iter().find(|r| r.pane == pane && r.id == run) else {
            return Ok(false);
        };
        self.refresh_event_locked(&r, &record.agent_id, run, "input")?;
        Ok(true)
    }
    pub fn session_refresh_hook(&self, input: &str) -> Result<()> {
        let (Ok(agent), Ok(run)) = (std::env::var("PCTL_AGENT_ID"), std::env::var("PCTL_RUN_ID"))
        else {
            return Ok(());
        };
        let v: Value = serde_json::from_str(input)?;
        let _lock = self.lock()?;
        let r = self.registry()?;
        let Some(a) = r.agents.iter().find(|a| a.id == agent) else {
            return Ok(());
        };
        if crate::session::harness(&a.argv) != "claude"
            || !r.runs.iter().any(|x| x.agent_id == agent && x.id == run)
        {
            return Ok(());
        }
        if a.conversation.is_none() || v["session_id"].as_str() != a.conversation.as_deref() {
            return Ok(());
        }
        let mut state = self.refresh_state()?;
        let activity = state.sessions.entry(agent).or_default();
        activity.bind(&run);
        if activity.conversation.is_none() || activity.conversation != a.conversation {
            return Ok(());
        }
        activity.hook(&v, now())?;
        self.save_refresh_state(state, &r)
    }
    /// Advisory revalidation only; this never activates a reservation or grants permission to stop.
    pub fn session_refresh_proposal_valid(&self, proposal: &Value) -> Result<bool> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.refresh_proposal_valid_locked(&r, proposal, &Policy::load()?)
    }
    fn refresh_proposal_valid_locked(
        &self,
        r: &Registry,
        proposal: &Value,
        policy: &Policy,
    ) -> Result<bool> {
        if proposal["server_instance"] != r.server_instance || proposal["active"] != false {
            return Ok(false);
        }
        let Some(project) = proposal["project_id"].as_str() else {
            return Ok(false);
        };
        let state = self.refresh_state()?;
        let Some(window) = state.projects.get(project) else {
            return Ok(false);
        };
        let at = now();
        if window.epoch.is_empty()
            || proposal["project_epoch"] != window.epoch
            || !at
                .checked_sub(window.last_seen)
                .is_some_and(|gap| u128::from(gap) <= u128::from(policy.poll_interval_secs) * 2)
            || !window
                .since
                .and_then(|s| at.checked_sub(s))
                .is_some_and(|age| age >= policy.idle_for_secs)
        {
            return Ok(false);
        }
        let Some(saved) = window.cohort.as_array() else {
            return Ok(false);
        };
        if r.agents.iter().filter(|a| a.project_id == project).count() != saved.len() {
            return Ok(false);
        }
        let views = self.session_observations(r);
        let clients = self.tm(&["list-clients", "-F", "#{session_id}"])?;
        let sessions = self.tm(&[
            "list-sessions",
            "-F",
            "#{session_id}\t#{session_last_attached}",
        ])?;
        let mail = self.mailbox_summaries(r)?;
        for row in saved {
            let Some(a) = r
                .agents
                .iter()
                .find(|a| row["agent"] == a.id && a.project_id == project)
            else {
                return Ok(false);
            };
            let run = r.runs.iter().find(|r| r.agent_id == a.id);
            let activity = state
                .sessions
                .get(&a.id)
                .filter(|act| run.is_none_or(|r| act.run_id == r.id));
            if row["run"] != json!(run.map(|r| &r.id))
                || row["activity"] != serde_json::to_value(activity)?
                || row["protected"] != a.protected
                || row["queue"] != crate::tellq::observation(&self.state, &a.name)?
            {
                return Ok(false);
            }
            let view = views
                .iter()
                .find(|v| v["agent_id"] == a.id)
                .context("missing observation")?;
            if view["status"] != row["status"]
                || view["swap_blockers"].as_array().is_none_or(|bs| {
                    bs.iter()
                        .any(|b| b == "app_visible" || b == "app_visibility_unknown")
                })
            {
                return Ok(false);
            }
            if let Some(run) = run {
                let attached = sessions.lines().find_map(|line| {
                    let (s, t) = line.split_once('\t')?;
                    (s == run.session).then_some(t)
                });
                if clients.lines().any(|s| s == run.session)
                    || mail.get(&run.id).copied().unwrap_or(0) > 0
                    || row["attached"] != json!(attached)
                {
                    return Ok(false);
                }
            }
            if proposal["agent_id"] == a.id
                && (view["swap_blockers"]
                    .as_array()
                    .is_none_or(|b| !b.is_empty())
                    || !activity.is_some_and(|activity| {
                        activity.ready
                            && activity.conversation.is_some()
                            && activity.conversation == a.conversation
                    })
                    || proposal["input_epoch"] != json!(activity.map(|a| a.input_epoch))
                    || proposal["queue_epoch"] != json!(activity.map(|a| a.queue_epoch))
                    || proposal["conversation"] != json!(a.conversation)
                    || proposal["run_id"] != view["run_id"]
                    || proposal["target_build"] != view["installed_build"]
                    || a.protected)
            {
                return Ok(false);
            }
        }
        Ok(saved.iter().any(|a| a["agent"] == proposal["agent_id"]))
    }
    /// Dry-run only. Persist observations, never reserve input, deliver messages or call refresh.
    pub fn session_refresh_idle_dry_run(&self) -> Result<Value> {
        let sweep = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("refresh-observe.lock"))?;
        fs2::FileExt::try_lock_exclusive(&sweep).context("refresh observation already running")?;
        let policy = Policy::load()?;
        let r = self.registry()?;
        let views = self.session_observations(&r);
        let mailbox = self.mailbox_summaries(&r)?;

        let clients = self.tm(&["list-clients", "-F", "#{session_id}"]).ok();
        let sessions = self
            .tm(&[
                "list-sessions",
                "-F",
                "#{session_id}\t#{session_last_attached}",
            ])
            .ok();
        // Screen/PATH reads do not hold up typing; accept the sample only on the same registry.
        let _lock = self.lock()?;
        ensure!(
            self.registry()? == r,
            "registry changed during observation; repeat the dry-run"
        );
        let mut state = self.refresh_state()?;
        let at = now();
        let mut projects = Vec::new();
        let mut would_swap = Vec::new();
        for p in &r.projects {
            let mut reasons = BTreeSet::new();
            let mut cohort = Vec::new();
            let mut members = Vec::new();
            let mut live = 0;
            for a in r.agents.iter().filter(|a| a.project_id == p.id) {
                let view = views
                    .iter()
                    .find(|v| v["agent_id"] == a.id)
                    .context("missing session observation")?;
                let run = r.runs.iter().find(|x| x.agent_id == a.id);
                let activity = state
                    .sessions
                    .get(&a.id)
                    .filter(|act| run.is_none_or(|run| act.run_id == run.id));
                let queue = crate::tellq::observation(&self.state, &a.name);
                let mut held = Vec::new();
                if let Some(run) = run {
                    live += 1;
                    if view["status"] != "idle" {
                        held.push(format!(
                            "status:{}",
                            view["status"].as_str().unwrap_or("unknown")
                        ));
                    }
                    if mailbox.get(&run.id).copied().unwrap_or(0) > 0 {
                        held.push("mailbox_pending".into());
                    }
                    match &clients {
                        Some(c) if c.lines().any(|s| s == run.session) => {
                            held.push("raw_client_attached".into())
                        }
                        None => held.push("raw_clients_unknown".into()),
                        _ => {}
                    }
                    if crate::session::harness(&a.argv) == "claude"
                        && !activity.is_some_and(|activity| {
                            activity.ready
                                && activity.conversation.is_some()
                                && activity.conversation == a.conversation
                        })
                    {
                        held.push("readiness_unknown".into());
                    }
                    for b in view["swap_blockers"]
                        .as_array()
                        .context("missing swap blockers")?
                    {
                        if b == "app_visible" || b == "app_visibility_unknown" {
                            held.push(b.as_str().unwrap_or("visibility_unknown").into());
                        }
                    }
                }
                match &queue {
                    Ok(q) if q["pending"] != 0 || q["unresolved"] != false => {
                        held.push("tell_pending_or_unresolved".into())
                    }
                    Err(_) => held.push("tell_queue_unknown".into()),
                    _ => {}
                }
                for reason in &held {
                    reasons.insert(format!("{}:{reason}", a.name));
                }
                let attached = run.and_then(|run| {
                    sessions.as_ref()?.lines().find_map(|line| {
                        let (s, time) = line.split_once('\t')?;
                        (s == run.session).then_some(time.to_string())
                    })
                });
                if run.is_some() && attached.is_none() {
                    reasons.insert(format!("{}:session_activity_unknown", a.name));
                }
                cohort.push(json!({"agent":a.id,"run":run.map(|r| &r.id),"activity":activity,"protected":a.protected,
                    "queue":queue.as_ref().ok(),"attached":attached,"status":view["status"]}));
                members.push((a, view, activity.cloned(), held));
            }
            if live == 0 {
                reasons.insert("no_live_sessions".into());
            }
            let cohort = json!(cohort);
            let window = state.projects.entry(p.id.clone()).or_default();
            let idle = window.update(cohort.clone(), reasons.is_empty(), at, &policy);
            if reasons.is_empty() && idle < policy.idle_for_secs {
                reasons.insert("idle_window_incomplete".into());
            }
            let eligible = reasons.is_empty();
            let mut entries = Vec::new();
            for (a, view, activity, mut held) in members {
                if !eligible {
                    held.push("project_not_idle".into());
                }
                for reason in view["swap_blockers"]
                    .as_array()
                    .context("missing swap blockers")?
                {
                    held.push(reason.as_str().unwrap_or("unknown_blocker").into());
                }
                held.sort();
                held.dedup();
                // A proposal is evidence at this read, never an active reservation or future permission.
                let reservation = json!({"server_instance":r.server_instance,"project_id":p.id,
                    "agent_id":a.id,"run_id":view["run_id"],"conversation":a.conversation,
                    "input_epoch":activity.as_ref().map(|a| a.input_epoch),"queue_epoch":activity.as_ref().map(|a| a.queue_epoch),
                    "project_epoch":window.epoch,"target_build":view["installed_build"],"active":false});
                if held.is_empty() {
                    would_swap.push(json!({"name":a.name,"reservation":reservation}));
                }
                entries.push(json!({"name":a.name,"run_id":view["run_id"],"conversation":a.conversation,
                    "harness":crate::session::harness(&a.argv),"status":view["status"],"stale":view["stale"],
                    "readiness":activity,"reasons":held}));
            }
            projects.push(json!({"name":p.name,"project_id":p.id,"idle_seconds":idle,"required_idle_seconds":policy.idle_for_secs,
                "eligible":eligible,"reasons":reasons,"sessions":entries}));
        }
        self.save_refresh_state(state, &r)?;
        Ok(
            json!({"dry_run":true,"execution_supported":false,"policy":policy,"observed_at":at,
            "would_swap":would_swap,"projects":projects}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_bridge_input_retains_the_false_result_contract() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::new(d.path().join("s"), "missing-tmux".into()).unwrap();
        assert!(!core.refresh_pane_input("%0", "old").unwrap());
        assert!(!core.state.join("refresh-observe.json").exists());
    }
    #[test]
    fn readiness_requires_complete_bound_events_and_empty_task_fields() {
        let mut a = Activity::default();
        a.bind("run");
        let hook = |event| json!({"hook_event_name":event});
        a.hook(&hook("SessionStart"), 1).unwrap();
        a.hook(&hook("UserPromptSubmit"), 2).unwrap();
        let stop = json!({"hook_event_name":"Stop","background_tasks":[],"session_crons":[],"stop_hook_active":false});
        a.hook(&stop, 3).unwrap();
        assert!(a.ready);
        a.event("input", 4).unwrap();
        a.hook(&stop, 5).unwrap();
        assert!(!a.ready);
        a.hook(&hook("UserPromptSubmit"), 6).unwrap();
        a.hook(
            &json!({"hook_event_name":"PreToolUse","tool_use_id":"tool"}),
            7,
        )
        .unwrap();
        a.hook(&stop, 8).unwrap();
        assert!(!a.ready);
        a.hook(
            &json!({"hook_event_name":"PostToolUse","tool_use_id":"tool"}),
            9,
        )
        .unwrap();
        a.hook(&stop, 10).unwrap();
        assert!(a.ready);
        let mut missing = stop.clone();
        missing.as_object_mut().unwrap().remove("background_tasks");
        a.hook(&missing, 11).unwrap();
        assert!(!a.ready);
        let mut background = stop.clone();
        background["session_crons"] = json!([{}]);
        a.hook(&background, 12).unwrap();
        assert!(!a.ready);
        a.bind("new-run");
        a.hook(&stop, 13).unwrap();
        assert!(!a.ready);
        a.hook(&hook("SessionStart"), 14).unwrap();
        for i in 0..257 {
            a.hook(
                &json!({"hook_event_name":"PreToolUse","tool_use_id":format!("tool-{i}")}),
                15,
            )
            .unwrap();
        }
        assert_eq!(a.tools.len(), 256);
        assert!(!a.instrumented);
        a.hook(&hook("SessionStart"), 16).unwrap();
        assert!(a.tools.is_empty() && a.subagents.is_empty());
    }
    #[test]
    fn idle_window_requires_continuity_and_resets_for_activity_or_clock_changes() {
        let p = Policy::default();
        assert_eq!(p.idle_for_secs, 1800);
        assert!(!p.enabled);
        let mut w = Window::default();
        assert_eq!(w.update(json!(1), true, 100, &p), 0);
        for t in (160..=1900).step_by(60) {
            w.update(json!(1), true, t, &p);
        }
        assert_eq!(w.since, Some(100));
        assert_eq!(w.update(json!(1), true, 1900, &p), 1800);
        assert_eq!(w.update(json!(2), true, 1960, &p), 0);
        assert_eq!(w.update(json!(2), true, 2200, &p), 0);
        assert_eq!(w.update(json!(2), true, 2000, &p), 0);
        w.update(json!(2), false, 2060, &p);
        assert_eq!(w.since, None);
        let max = Policy {
            poll_interval_secs: u64::MAX,
            idle_for_secs: u64::MAX,
            ..p
        };
        w.update(json!(3), true, 0, &max);
        assert_eq!(w.update(json!(3), true, u64::MAX, &max), u64::MAX);
        let mut epoch = u64::MAX;
        assert!(increment(&mut epoch).is_err());
        assert_eq!(epoch, u64::MAX);
    }
    #[test]
    fn proposal_revalidation_catches_input_queue_protection_build_and_membership_changes() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let executable = d.path().join("claude");
        fs::write(&executable, "#!/bin/sh\n# old\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = d.path().join("tmux");
        let core = Core::new(d.path().join("s"), tmux.clone()).unwrap();
        let project = core.register_project("p", d.path()).unwrap();
        let a = core
            .register_agent(
                &project.id,
                "worker",
                d.path(),
                vec![executable.to_string_lossy().into()],
                "generic",
            )
            .unwrap();
        let mut r = core.registry().unwrap();
        r.agents[0].conversation = Some("conversation".into());
        let old = core.resolve_build(&r.agents[0]).unwrap().1;
        fs::write(&executable, "#!/bin/sh\n# replacement build\n").unwrap();
        let pid = std::process::id().to_string();
        let run = crate::Run {
            id: "run".into(),
            agent_id: a.id.clone(),
            project_id: project.id.clone(),
            server_instance: r.server_instance.clone(),
            session: "$0".into(),
            pane: "%0".into(),
            window: "@0".into(),
            pid: pid.clone(),
            process_start: crate::process_start(&pid).unwrap(),
            started_at: now(),
            running_build: Some(old),
        };
        let row = format!(
            "$0\t@0\t%0\t{pid}\t0\t\trun\t{}\t{}\t0\t0",
            a.id, project.id
        );
        fs::write(&tmux,format!("#!/bin/sh\ncase \"$6\" in\nshow-options) echo '{}' ;;\nlist-panes) printf '%s\\n' '{}' ;;\ncapture-pane) echo '> ' ;;\nlist-sessions) printf '$0\\t0\\n' ;;\nesac\n",r.server_instance,row)).unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        r.runs.push(run);
        core.save(&r).unwrap();
        let activity = Activity {
            run_id: "run".into(),
            conversation: Some("conversation".into()),
            instrumented: true,
            ready: true,
            ..Activity::default()
        };
        let cohort = json!([{"agent":a.id,"run":"run","activity":activity,"protected":false,"queue":{"pending":0,"unresolved":false,"files":[]},"attached":"0","status":"idle"}]);
        let mut state = State::default();
        state.sessions.insert(a.id.clone(), activity);
        state.projects.insert(
            project.id.clone(),
            Window {
                epoch: "project-epoch".into(),
                cohort: cohort.clone(),
                since: Some(now().saturating_sub(1800)),
                last_seen: now(),
            },
        );
        core.save_refresh_state(state, &r).unwrap();
        let mut proposal = json!({"server_instance":r.server_instance,"project_id":project.id,"agent_id":a.id,"run_id":"run","conversation":"conversation","project_epoch":"project-epoch",
            "input_epoch":0,"queue_epoch":0,"target_build":core.resolve_build(&r.agents[0]).unwrap().1,"active":false});
        let policy = Policy::default();
        assert!(
            core.refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        proposal["project_epoch"] = json!("old-project-epoch");
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        proposal["project_epoch"] = json!("project-epoch");
        proposal["queue_epoch"] = json!(1);
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        proposal["queue_epoch"] = json!(0);
        let target = proposal["target_build"].clone();
        proposal["target_build"] = Value::Null;
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        proposal["target_build"] = target;
        let mut extra = r.agents[0].clone();
        extra.id = "extra".into();
        r.agents.push(extra);
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        r.agents.pop();
        r.agents[0].protected = true;
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        r.agents[0].protected = false;
        core.refresh_event_locked(&r, &a.id, "run", "input")
            .unwrap();
        assert!(
            !core
                .refresh_proposal_valid_locked(&r, &proposal, &policy)
                .unwrap()
        );
        // Removing an agent also removes its owned activity record, with no tombstones.
        r.agents.clear();
        r.runs.clear();
        core.save_refresh_state(core.refresh_state().unwrap(), &r)
            .unwrap();
        assert!(core.refresh_state().unwrap().sessions.is_empty());
    }
}
