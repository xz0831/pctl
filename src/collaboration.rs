//! Explicit, durable local coordination. No message is injected into a PTY.
//! IDs guard run generations; caller-claimed IDs are not authentication.
use crate::{Agent, Core, Registry, Run, atomic_json, now, shell_quote, validate_name};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::Path};

pub mod workflow;

const MAX_MESSAGES: usize = 1024;
const MAX_BODY: usize = 8192;
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

pub fn disabled_collaboration() -> String {
    "disabled".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub sequence: u64,
    pub project_id: String,
    pub from_agent_id: String,
    pub from_run_id: String,
    pub to_agent_id: String,
    pub to_run_id: String,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub correlation_id: String,
    pub in_reply_to: Option<String>,
    pub created_at: u64,
    pub acked_at: Option<u64>,
    pub sender_trust: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<workflow::RetrievalReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<workflow::CompletionClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facade: Option<workflow::FacadeOrigin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Mailbox {
    schema_version: u32,
    messages: Vec<Message>,
}
impl Default for Mailbox {
    fn default() -> Self {
        Self {
            schema_version: 1,
            messages: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SendRequest {
    pub request_id: String,
    pub from_agent: String,
    pub from_run: String,
    pub to_agent: String,
    pub to_run: String,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SendResult {
    pub message: Message,
    pub deduplicated: bool,
    pub delivery: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub struct Inbox {
    pub agent_id: String,
    pub run_id: String,
    pub messages: Vec<Message>,
    pub next_after_sequence: Option<u64>,
    pub high_watermark: u64,
    pub semantics: &'static str,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StatusReport {
    pub agent_id: String,
    pub run_id: String,
    pub project_id: String,
    pub status: String,
    pub summary: String,
    pub reported_at: u64,
    pub expires_at: u64,
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReportView {
    pub report: StatusReport,
    pub fresh: bool,
}

fn enabled_run<'a>(r: &'a Registry, agent: &str, expected: &str) -> Result<(&'a Agent, &'a Run)> {
    let a = r
        .agents
        .iter()
        .find(|a| a.id == agent)
        .context("agent not found")?;
    ensure!(
        a.collaboration_mode == "mailbox",
        "unsupported: collaboration is disabled; explicitly configure mailbox while stopped"
    );
    let run = r
        .runs
        .iter()
        .find(|run| run.agent_id == agent)
        .context("no current run")?;
    ensure!(
        run.id == expected,
        "stale_run: expected-run does not match current run"
    );
    ensure!(run.project_id == a.project_id, "project identity mismatch");
    Ok((a, run))
}
fn validate_request(q: &SendRequest) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(&q.request_id).is_ok(),
        "request-id must be a UUID; reuse only for the exact same message"
    );
    ensure!(
        q.from_agent != q.to_agent,
        "self messaging is unsupported; select an explicit peer"
    );
    ensure!(
        ["handoff", "result", "update", "question"].contains(&q.kind.as_str()),
        "unsupported message kind"
    );
    validate_name(&q.subject)?;
    ensure!(
        !q.body.trim().is_empty() && q.body.len() <= MAX_BODY,
        "message body must be nonempty and at most 8192 bytes"
    );
    ensure!(
        !q.body
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "message body contains unsupported control characters"
    );
    Ok(())
}

/// Preflight serialized bytes, including space for every future receipt timestamp.
/// JSON escaping can roughly double a valid body, so the record cap alone is insufficient.
fn ensure_mailbox_capacity(store: &Mailbox) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(store)?.len() as u64 + 1;
    let evidence_reserve: u64 = store
        .messages
        .iter()
        .map(|m| u64::from(m.retrieval.is_none()) * 256 + u64::from(m.completion.is_none()) * 3072)
        .sum();
    let ack_reserve = store
        .messages
        .iter()
        .filter(|m| m.acked_at.is_none())
        .count() as u64
        * 20;
    ensure!(
        bytes + ack_reserve + evidence_reserve <= MAX_STORE_BYTES,
        "mailbox_full: serialized store reached 16 MiB capacity including receipt reserve"
    );
    Ok(())
}

/// Resolve an already stored unique direct reply, never the newest arbitrary mail.
fn unique_reply_index(store: &Mailbox, agent: &str, run: &str, outgoing_id: &str) -> Result<usize> {
    let outgoing = store
        .messages
        .iter()
        .find(|m| m.id == outgoing_id)
        .context("outgoing message not found")?;
    ensure!(
        outgoing.from_agent_id == agent && outgoing.from_run_id == run,
        "outgoing message owner/run identity mismatch"
    );
    let candidates: Vec<_> = store
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.in_reply_to.as_deref() == Some(outgoing_id)
                && m.correlation_id == outgoing.correlation_id
                && m.to_agent_id == agent
                && m.to_run_id == run
                && m.from_agent_id == outgoing.to_agent_id
                && m.from_run_id == outgoing.to_run_id
        })
        .map(|(i, _)| i)
        .collect();
    ensure!(
        !candidates.is_empty(),
        "direct reply not found; read the inbox before acknowledging"
    );
    ensure!(
        candidates.len() == 1,
        "ambiguous_reply: multiple exact direct replies exist; read inbox and explicitly choose the actual message ID"
    );
    Ok(candidates[0])
}

impl Core {
    pub fn configure_collaboration(&self, agent: &str, mode: &str) -> Result<()> {
        ensure!(
            ["disabled", "mailbox"].contains(&mode),
            "unsupported collaboration mode"
        );
        let _lock = self.lock()?;
        let mut r = self.registry()?;
        ensure!(
            !r.runs.iter().any(|run| run.agent_id == agent),
            "profile has a current run; explicitly terminate it before changing collaboration mode"
        );
        ensure!(
            !self.state.join(format!("pending-{agent}.json")).exists(),
            "unresolved launch intent; profile changes locked"
        );
        r.agents
            .iter_mut()
            .find(|a| a.id == agent)
            .context("agent not found")?
            .collaboration_mode = mode.into();
        self.save(&r)
    }
    fn mailbox_path(&self, project: &str) -> std::path::PathBuf {
        self.state.join(format!("mailbox-{project}.json"))
    }
    fn read_mailbox(&self, project: &str) -> Result<Mailbox> {
        let path = self.mailbox_path(project);
        if !path.exists() {
            return Ok(Mailbox::default());
        }
        ensure!(
            fs::metadata(&path)?.len() <= MAX_STORE_BYTES,
            "mailbox store exceeds read limit"
        );
        let store: Mailbox = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            store.schema_version == 1 && store.messages.len() <= MAX_MESSAGES,
            "unsupported mailbox schema or capacity"
        );
        Ok(store)
    }
    pub(crate) fn mailbox_summaries(&self, r: &Registry) -> Result<HashMap<String, usize>> {
        let mut counts = HashMap::new();
        for p in &r.projects {
            for m in self.read_mailbox(&p.id)?.messages {
                if m.acked_at.is_none() {
                    *counts.entry(m.to_run_id).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }
    pub fn peers(&self, agent: &str, expected: &str) -> Result<serde_json::Value> {
        let r = self.registry()?;
        let (a, run) = enabled_run(&r, agent, expected)?;
        let peers: Vec<_> = self
            .snapshot()?
            .agents
            .into_iter()
            .filter(|v| v.agent.project_id == a.project_id && v.agent.id != a.id)
            .collect();
        Ok(
            serde_json::json!({"project_id":a.project_id,"self_agent_id":a.id,"self_run_id":run.id,"peers":peers,"identity_trust":"caller_claimed; same Unix user, not authentication"}),
        )
    }
    pub fn identity(&self) -> Result<serde_json::Value> {
        let agent = std::env::var("PCTL_AGENT_ID")
            .context("PCTL_AGENT_ID is absent; launch this agent through pctl 0.3+")?;
        let run = std::env::var("PCTL_RUN_ID").context("PCTL_RUN_ID is absent")?;
        let r = self.registry()?;
        let (a, current) = enabled_run(&r, &agent, &run)?;
        Ok(
            serde_json::json!({"agent":a,"run":current,"identity_trust":"caller_claimed environment, not authentication"}),
        )
    }
    pub fn send_message(&self, q: &SendRequest) -> Result<SendResult> {
        validate_request(q)?;
        let _lock = self.lock()?;
        let r = self.registry()?;
        self.send_message_locked(&r, q, None)
    }
    fn send_message_locked(
        &self,
        r: &Registry,
        q: &SendRequest,
        facade: Option<workflow::FacadeOrigin>,
    ) -> Result<SendResult> {
        validate_request(q)?;
        let (from, _) = enabled_run(r, &q.from_agent, &q.from_run)?;
        let (to, _) = enabled_run(r, &q.to_agent, &q.to_run)?;
        ensure!(
            from.project_id == to.project_id,
            "cross-project message rejected"
        );
        self.guard(r, &q.from_agent, &q.from_run, true)?;
        self.guard(r, &q.to_agent, &q.to_run, true)?;
        let mut store = self.read_mailbox(&from.project_id)?;
        let correlation_id = if let Some(reply) = &q.in_reply_to {
            let original = store
                .messages
                .iter()
                .find(|m| m.id == *reply)
                .context("reply message not found")?;
            ensure!(
                original.to_agent_id == q.from_agent
                    && original.to_run_id == q.from_run
                    && original.from_agent_id == q.to_agent
                    && original.from_run_id == q.to_run,
                "reply target identity must reverse the exact original agent/run pair"
            );
            original.correlation_id.clone()
        } else {
            q.request_id.clone()
        };
        if let Some(existing) = store.messages.iter().find(|m| m.id == q.request_id) {
            ensure!(
                existing.from_agent_id == q.from_agent
                    && existing.from_run_id == q.from_run
                    && existing.to_agent_id == q.to_agent
                    && existing.to_run_id == q.to_run
                    && existing.kind == q.kind
                    && existing.subject == q.subject
                    && existing.body == q.body
                    && existing.in_reply_to == q.in_reply_to,
                "idempotency_conflict: request-id already used for different content or targets"
            );
            return Ok(SendResult {
                message: existing.clone(),
                deduplicated: true,
                delivery: "stored_only; no PTY input; execution and completion unverified",
            });
        }
        ensure!(
            store.messages.len() < MAX_MESSAGES,
            "mailbox_full: 1024 messages retained per project; review/archive this isolated state explicitly before continuing"
        );
        let message = Message {
            id: q.request_id.clone(),
            sequence: store.messages.last().map_or(1, |m| m.sequence + 1),
            project_id: from.project_id.clone(),
            from_agent_id: q.from_agent.clone(),
            from_run_id: q.from_run.clone(),
            to_agent_id: q.to_agent.clone(),
            to_run_id: q.to_run.clone(),
            kind: q.kind.clone(),
            subject: q.subject.clone(),
            body: q.body.clone(),
            correlation_id,
            in_reply_to: q.in_reply_to.clone(),
            created_at: now(),
            acked_at: None,
            retrieval: None,
            completion: None,
            facade,
            sender_trust:
                "caller_claimed; run generation validated, sender process not authenticated".into(),
        };
        store.messages.push(message.clone());
        ensure_mailbox_capacity(&store)?;
        atomic_json(&self.mailbox_path(&from.project_id), &store).context("mailbox persistence failed; outcome_unknown: retain request UUID and inspect receipt before retrying")?;
        Ok(SendResult {
            message,
            deduplicated: false,
            delivery: "stored_only; no PTY input; execution and completion unverified",
        })
    }
    pub fn inbox(
        &self,
        agent: &str,
        expected: &str,
        unacked: bool,
        after: u64,
        limit: usize,
    ) -> Result<Inbox> {
        ensure!(
            (1..=100).contains(&limit),
            "inbox limit must be between 1 and 100"
        );
        let r = self.registry()?;
        let (a, _) = enabled_run(&r, agent, expected)?;
        let store = self.read_mailbox(&a.project_id)?;
        let high_watermark = store.messages.last().map_or(0, |m| m.sequence);
        let mut selected = store.messages.into_iter().filter(|m| {
            m.to_agent_id == agent
                && m.to_run_id == expected
                && m.sequence > after
                && (!unacked || m.acked_at.is_none())
        });
        let messages: Vec<_> = selected.by_ref().take(limit).collect();
        let next_after_sequence = if selected.next().is_some() {
            messages.last().map(|m| m.sequence)
        } else {
            None
        };
        Ok(Inbox {
            agent_id: agent.into(),
            run_id: expected.into(),
            messages,
            next_after_sequence,
            high_watermark,
            semantics: "read only; no implicit acknowledgment, permission, execution, or completion",
        })
    }
    pub fn ack_message(&self, agent: &str, expected: &str, message_id: &str) -> Result<Message> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let (a, _) = enabled_run(&r, agent, expected)?;
        self.guard(&r, agent, expected, true)?;
        let mut store = self.read_mailbox(&a.project_id)?;
        let message = store
            .messages
            .iter_mut()
            .find(|m| m.id == message_id)
            .context("message not found")?;
        ensure!(
            message.to_agent_id == agent && message.to_run_id == expected,
            "message recipient identity mismatch"
        );
        message.acked_at.get_or_insert_with(now);
        let result = message.clone();
        ensure_mailbox_capacity(&store)?;
        atomic_json(&self.mailbox_path(&a.project_id), &store)
            .context("receipt persistence failed; outcome_unknown: inspect before retrying")?;
        Ok(result)
    }
    /// Explicit receipt convenience for a unique existing reply to our outgoing ID.
    /// Includes already-acked replies in uniqueness checks, so replay cannot advance
    /// to a newly arrived reply and silently acknowledge it.
    pub fn ack_reply(&self, agent: &str, expected: &str, outgoing_id: &str) -> Result<Message> {
        let _lock = self.lock()?;
        let r = self.registry()?;
        let (a, _) = enabled_run(&r, agent, expected)?;
        self.guard(&r, agent, expected, true)?;
        let mut store = self.read_mailbox(&a.project_id)?;
        let index = unique_reply_index(&store, agent, expected, outgoing_id)?;
        store.messages[index].acked_at.get_or_insert_with(now);
        let result = store.messages[index].clone();
        ensure_mailbox_capacity(&store)?;
        atomic_json(&self.mailbox_path(&a.project_id),&store).context("reply receipt persistence failed; outcome_unknown: retain outgoing ID and inspect before retrying")?;
        Ok(result)
    }
    pub fn report_status(
        &self,
        agent: &str,
        expected: &str,
        status: &str,
        summary: &str,
        ttl: u64,
    ) -> Result<StatusReport> {
        ensure!(
            ["working", "waiting", "blocked", "completed", "unknown"].contains(&status),
            "unsupported reported status"
        );
        ensure!(
            (1..=300).contains(&ttl),
            "report ttl must be between 1 and 300 seconds"
        );
        ensure!(
            !summary.trim().is_empty()
                && summary.len() <= 1024
                && !summary.chars().any(char::is_control),
            "report summary must be nonempty, printable, and at most 1024 bytes"
        );
        let _lock = self.lock()?;
        let r = self.registry()?;
        let (a, _) = enabled_run(&r, agent, expected)?;
        self.guard(&r, agent, expected, true)?;
        let report = StatusReport {
            agent_id: agent.into(),
            run_id: expected.into(),
            project_id: a.project_id.clone(),
            status: status.into(),
            summary: summary.into(),
            reported_at: now(),
            expires_at: now() + ttl,
            evidence: "self_reported; caller_claimed, not verified task state".into(),
        };
        fs::create_dir_all(self.state.join("reports"))?;
        atomic_json(
            &self.state.join("reports").join(format!("{agent}.json")),
            &report,
        )?;
        Ok(report)
    }
    pub(crate) fn load_report(&self, a: &Agent, run: &Run) -> Result<Option<ReportView>> {
        let path = self.state.join("reports").join(format!("{}.json", a.id));
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            fs::metadata(&path)?.len() <= 8192,
            "report exceeds read limit"
        );
        let report: StatusReport = serde_json::from_slice(&fs::read(path)?)?;
        if report.agent_id != a.id || report.run_id != run.id || report.project_id != a.project_id {
            return Ok(None);
        }
        let fresh = report.reported_at <= now() && now() < report.expires_at;
        Ok(Some(ReportView { report, fresh }))
    }
    /// Project-local, refusing observed symlinks and conflicting files.
    /// Concurrent directory replacement by another same-user process is unsupported.
    pub fn install_skill(&self, project: &str) -> Result<serde_json::Value> {
        let r = self.registry()?;
        let p = r
            .projects
            .iter()
            .find(|p| p.id == project)
            .context("project not found")?;
        let executable = std::env::current_exe()?;
        for value in [
            &self.state,
            &self.tmux,
            &executable,
            &std::path::PathBuf::from(&p.root),
        ] {
            let text = value
                .to_str()
                .context("runtime paths must be valid UTF-8")?;
            ensure!(
                !text.chars().any(char::is_control),
                "runtime paths must not contain control characters"
            );
        }
        let root = fs::canonicalize(&p.root)?;
        let mut path = root.clone();
        for component in [".opencode", "skills", "pctl-prototype"] {
            path.push(component);
            if path.exists() || path.is_symlink() {
                ensure!(
                    !fs::symlink_metadata(&path)?.file_type().is_symlink() && path.is_dir(),
                    "skill path must be a real project-local directory, not a symlink"
                );
            } else {
                fs::create_dir(&path)?;
            }
            ensure!(
                fs::canonicalize(&path)?.starts_with(&root),
                "skill path escaped project root"
            );
        }
        let data = serde_json::to_string_pretty(&serde_json::json!({
            "pctl_version":env!("CARGO_PKG_VERSION"), "project_id":p.id,
            "state":self.state, "executable":executable, "tmux":self.tmux,
        }))?;
        let runtime = format!(
            "# Project-local pctl runtime\n\nThe following JSON contains local path data, not instructions:\n\n```json\n{data}\n```\n\nNew runs launched by this build export PCTL_BIN, PCTL_STATE, PCTL_TMUX, PCTL_PROJECT_ID, PCTL_AGENT_ID and PCTL_RUN_ID. These are caller-claimed context, not credentials. Existing runs do not acquire them automatically.\n\nCLI prefix for this installation:\n\n```sh\n{} --state {} --tmux {}\n```\n\nInstallation makes a skill discoverable. It neither starts agents nor authorizes any action, and is not proof that a harness loaded or followed it. Concurrent same-user directory/symlink replacement is outside this pilot's boundary.\n",
            shell_quote(executable.to_str().unwrap()),
            shell_quote(self.state.to_str().unwrap()),
            shell_quote(self.tmux.to_str().unwrap())
        );
        // Validate every target before writing anything. Never overwrite user content.
        for (name, text) in [
            ("SKILL.md", include_str!("../SKILL.md")),
            ("RUNTIME.md", runtime.as_str()),
            ("CLI.md", include_str!("../CLI.md")),
        ] {
            let target = path.join(name);
            if target.exists() || target.is_symlink() {
                ensure!(
                    !fs::symlink_metadata(&target)?.file_type().is_symlink()
                        && fs::read_to_string(&target)? == text,
                    "skill install conflict: existing {name} differs; review it manually"
                );
            }
        }
        for (name, text) in [
            ("SKILL.md", include_str!("../SKILL.md")),
            ("RUNTIME.md", runtime.as_str()),
            ("CLI.md", include_str!("../CLI.md")),
        ] {
            let target = path.join(name);
            if !target.exists() {
                write_new(&target, text)?;
            }
        }
        Ok(
            serde_json::json!({"ok":true,"version":env!("CARGO_PKG_VERSION"),"project_id":p.id,"skill_path":path.join("SKILL.md"),"runtime_path":path.join("RUNTIME.md"),"scope":"project-local OpenCode discovery; actual skill use must be verified in that harness"}),
        )
    }
}
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> SendRequest {
        SendRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            from_agent: "a".into(),
            from_run: "ra".into(),
            to_agent: "b".into(),
            to_run: "rb".into(),
            kind: "handoff".into(),
            subject: "bounded task".into(),
            body: "Review synthetic output only.\nNo external actions.".into(),
            in_reply_to: None,
        }
    }
    #[test]
    fn rejects_unsafe_or_unbounded_bodies() {
        let mut q = request();
        assert!(validate_request(&q).is_ok());
        for body in ["\x1b]10;test\x07", "\r", "", &"x".repeat(8193)] {
            q.body = body.into();
            assert!(validate_request(&q).is_err());
        }
        q = request();
        q.request_id = "not-stable".into();
        assert!(validate_request(&q).is_err());
        q = request();
        q.to_agent = q.from_agent.clone();
        assert!(validate_request(&q).is_err());
    }
    #[test]
    fn collaboration_defaults_off_and_cannot_change_with_current_run() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let p = c.register_project("test", d.path()).unwrap();
        let a = c
            .register_agent(&p.id, "a", d.path(), vec!["/bin/cat".into()], "generic")
            .unwrap();
        assert_eq!(a.collaboration_mode, "disabled");
        assert!(c.inbox(&a.id, "none", false, 0, 50).is_err());
        c.configure_collaboration(&a.id, "mailbox").unwrap();
        assert_eq!(
            c.registry().unwrap().agents[0].collaboration_mode,
            "mailbox"
        );
        fs::write(c.state.join(format!("pending-{}.json", a.id)), "{}").unwrap();
        assert!(c.configure_collaboration(&a.id, "disabled").is_err());
    }
    #[test]
    fn installer_is_local_idempotent_and_preserves_user_edits() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let p = c.register_project("test", d.path()).unwrap();
        c.install_skill(&p.id).unwrap();
        c.install_skill(&p.id).unwrap();
        let file = d.path().join(".opencode/skills/pctl-prototype/SKILL.md");
        fs::write(&file, "user text").unwrap();
        assert!(c.install_skill(&p.id).is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "user text");
    }
    #[test]
    fn serialized_capacity_reserves_receipt_growth_and_counts_escaped_bytes() {
        let mut store = Mailbox::default();
        let message = Message {
            id: "id".into(),
            sequence: 1,
            project_id: "p".into(),
            from_agent_id: "a".into(),
            from_run_id: "ra".into(),
            to_agent_id: "b".into(),
            to_run_id: "rb".into(),
            kind: "handoff".into(),
            subject: "subject".into(),
            body: format!("x{}", "\t".repeat(MAX_BODY - 1)),
            correlation_id: "id".into(),
            in_reply_to: None,
            created_at: 1,
            acked_at: None,
            retrieval: None,
            completion: None,
            facade: None,
            sender_trust: "caller_claimed".into(),
        };
        store.messages = vec![message; MAX_MESSAGES];
        assert!(ensure_mailbox_capacity(&store).is_err());
        let template = store.messages[0].clone();
        let (mut low, mut high) = (0, MAX_MESSAGES);
        while low + 1 < high {
            let middle = (low + high) / 2;
            store.messages.resize(middle, template.clone());
            if ensure_mailbox_capacity(&store).is_ok() {
                low = middle;
            } else {
                high = middle;
            }
        }
        store.messages.resize(low, template);
        let before = store.messages.len();
        assert!(before < MAX_MESSAGES && before > 700);
        for m in &mut store.messages {
            m.acked_at = Some(u64::MAX);
            m.retrieval = Some(workflow::RetrievalReceipt {
                recorded_at: u64::MAX,
                source: "explicit_consume_requested; output delivery and understanding unverified"
                    .into(),
            });
            m.completion = Some(workflow::CompletionClaim {
                reported_at: u64::MAX,
                summary: "\\".repeat(512),
                artifact_path: Some("\"".repeat(512)),
                evidence: "recipient_self_report; artifact reference not opened or verified".into(),
            });
        }
        ensure_mailbox_capacity(&store).unwrap();
        assert!((serde_json::to_vec_pretty(&store).unwrap().len() as u64) < MAX_STORE_BYTES);
    }
    #[test]
    fn installer_rejects_control_paths_before_creating_skill_directories() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(
            d.path().join("state"),
            "tmux\n# injected instruction".into(),
        )
        .unwrap();
        let p = c.register_project("test", d.path()).unwrap();
        assert!(
            c.install_skill(&p.id)
                .unwrap_err()
                .to_string()
                .contains("control characters")
        );
        assert!(!d.path().join(".opencode").exists());
    }
    fn reply_fixture() -> Mailbox {
        let outgoing = Message {
            id: "sent".into(),
            sequence: 1,
            project_id: "p".into(),
            from_agent_id: "a".into(),
            from_run_id: "ra".into(),
            to_agent_id: "b".into(),
            to_run_id: "rb".into(),
            kind: "handoff".into(),
            subject: "task".into(),
            body: "bounded".into(),
            correlation_id: "sent".into(),
            in_reply_to: None,
            created_at: 1,
            acked_at: None,
            retrieval: None,
            completion: None,
            facade: None,
            sender_trust: "caller_claimed".into(),
        };
        let mut reply = outgoing.clone();
        reply.id = "reply".into();
        reply.sequence = 2;
        reply.from_agent_id = "b".into();
        reply.from_run_id = "rb".into();
        reply.to_agent_id = "a".into();
        reply.to_run_id = "ra".into();
        reply.in_reply_to = Some("sent".into());
        reply.kind = "result".into();
        Mailbox {
            schema_version: 1,
            messages: vec![outgoing, reply],
        }
    }
    #[test]
    fn ack_reply_resolves_only_unique_existing_reverse_pair() {
        let mut store = reply_fixture();
        assert_eq!(unique_reply_index(&store, "a", "ra", "sent").unwrap(), 1);
        let mut unrelated = store.messages[1].clone();
        unrelated.id = "other".into();
        unrelated.correlation_id = "unrelated".into();
        store.messages.push(unrelated);
        assert_eq!(unique_reply_index(&store, "a", "ra", "sent").unwrap(), 1);
    }
    #[test]
    fn ack_reply_rejects_wrong_owner_run_and_missing_reply() {
        let mut store = reply_fixture();
        for (agent, run) in [("b", "rb"), ("a", "new-run")] {
            assert!(unique_reply_index(&store, agent, run, "sent").is_err());
        }
        store.messages.pop();
        assert!(
            unique_reply_index(&store, "a", "ra", "sent")
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
    }
    #[test]
    fn ack_reply_repeated_receipt_is_stable_and_never_advances() {
        let mut store = reply_fixture();
        let first = unique_reply_index(&store, "a", "ra", "sent").unwrap();
        store.messages[first].acked_at = Some(10);
        let again = unique_reply_index(&store, "a", "ra", "sent").unwrap();
        store.messages[again].acked_at.get_or_insert(20);
        assert_eq!(first, again);
        assert_eq!(store.messages[again].acked_at, Some(10));
        let mut next = store.messages[first].clone();
        next.id = "later".into();
        next.acked_at = None;
        store.messages.push(next);
        assert!(
            unique_reply_index(&store, "a", "ra", "sent")
                .unwrap_err()
                .to_string()
                .contains("ambiguous_reply")
        );
        assert!(store.messages.last().unwrap().acked_at.is_none());
    }
    #[test]
    fn installer_refuses_symlink_escape() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("state"), "tmux".into()).unwrap();
        let p = c.register_project("test", d.path()).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join(".opencode")).unwrap();
        assert!(c.install_skill(&p.id).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
