//! Short, run-bound role-name facade. No terminal delivery and no authenticated identity.
use super::*;
use serde_json::{Value, json};
use std::path::Component;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetrievalReceipt {
    pub recorded_at: u64,
    pub source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CompletionClaim {
    pub reported_at: u64,
    pub summary: String,
    pub artifact_path: Option<String>,
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FacadeOrigin {
    pub key: String,
    pub target_role: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Caller {
    pub agent: String,
    pub run: String,
    pub project: String,
}
#[derive(Clone, Debug)]
pub struct RoleSend {
    pub role: Option<String>,
    pub reply: Option<String>,
    pub key: String,
    pub kind: String,
    pub subject: String,
    pub body: String,
}
const RECEIPT_SOURCE: &str =
    "explicit_consume_requested; output delivery and understanding unverified";
const COMPLETION_EVIDENCE: &str =
    "recipient_self_report; artifact reference not opened or verified";

fn bounded_printable(value: &str, max: usize, field: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control),
        "{field} must be nonempty printable text of at most {max} bytes"
    );
    Ok(())
}
fn validate_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 64
            && key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "key must be 1..64 ASCII letters, digits, hyphens, underscores or dots; retain it for retries"
    );
    Ok(())
}
fn validate_artifact(path: Option<&str>) -> Result<()> {
    if let Some(path) = path {
        bounded_printable(path, 512, "artifact path")?;
        ensure!(
            !path.split('/').any(|part| matches!(part, "" | "." | "..")),
            "artifact path must not contain empty/current/parent components"
        );
        ensure!(
            Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "artifact path must be relative with no parent/current/root components; it is an unverified reference only"
        );
    }
    Ok(())
}
fn handle(m: &Message) -> String {
    format!("m{}", m.sequence)
}
fn find_message<'a>(store: &'a Mailbox, c: &Caller, reference: &str) -> Result<&'a Message> {
    let sequence = reference
        .strip_prefix('m')
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0 && reference == format!("m{n}"))
        .context("message handle must be exactly m followed by its sequence, for example m1")?;
    store.messages.iter().find(|m| m.sequence == sequence && m.project_id == c.project &&
        ((m.from_agent_id == c.agent && m.from_run_id == c.run) ||
         (m.to_agent_id == c.agent && m.to_run_id == c.run)))
        .context("message not found for this exact caller run; handles do not transfer across runs/projects")
}
fn unique_role<'a>(r: &'a Registry, project: &str, role: &str) -> Result<&'a Agent> {
    validate_name(role)?;
    let candidates: Vec<_> = r
        .agents
        .iter()
        .filter(|a| a.project_id == project && a.name == role)
        .collect();
    ensure!(
        !candidates.is_empty(),
        "role not found: use an exact role from msg peers; matching is case-sensitive"
    );
    ensure!(
        candidates.len() == 1,
        "ambiguous_role: multiple profiles have this exact role, including stopped/disabled profiles"
    );
    Ok(candidates[0])
}
fn check_caller(r: &Registry, c: &Caller) -> Result<()> {
    let (a, _) = enabled_run(r, &c.agent, &c.run)?;
    ensure!(
        a.project_id == c.project,
        "caller project identity mismatch"
    );
    Ok(())
}
fn role_name<'a>(r: &'a Registry, id: &str) -> Option<&'a str> {
    r.agents
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.name.as_str())
}
fn view(r: &Registry, m: &Message, body: bool) -> Value {
    let mut value = json!({
        "handle":handle(m), "from":role_name(r,&m.from_agent_id), "to":role_name(r,&m.to_agent_id),
        "kind":m.kind, "subject":m.subject,
        "in_reply_to":m.in_reply_to,
        "stored_at":m.created_at,
        "retrieval":m.retrieval,
        "acknowledged_at":m.acked_at,
        "completion":m.completion,
        "verified_result":null,
    });
    if body {
        value["text"] = json!(m.body);
    }
    value
}
fn with_reply_handle(r: &Registry, store: &Mailbox, m: &Message, body: bool) -> Value {
    let mut value = view(r, m, body);
    value["in_reply_to"] = json!(
        m.in_reply_to.as_ref().and_then(|id| store
            .messages
            .iter()
            .find(|x| x.id == *id)
            .map(handle))
    );
    value
}
/// Open an already existing launcher-bound state without mkdir, chmod or registry writes.
/// This preflight runs before global settings or any Core::new side effects.
pub fn open_bound_core(state: std::path::PathBuf, tmux: std::path::PathBuf) -> Result<Core> {
    for key in [
        "PCTL_AGENT_ID",
        "PCTL_RUN_ID",
        "PCTL_PROJECT_ID",
        "PCTL_STATE",
        "PCTL_TMUX",
    ] {
        std::env::var(key).with_context(|| {
            format!("context_missing: {key} is absent; do not invent launcher context")
        })?;
    }
    let expected_state = fs::canonicalize(std::env::var("PCTL_STATE")?)
        .context("context_mismatch: launcher state is not an existing directory")?;
    let selected_state = fs::canonicalize(state)
        .context("context_mismatch: selected state is not an existing directory")?;
    ensure!(
        selected_state == expected_state,
        "context_mismatch: selected state differs from launcher context"
    );
    ensure!(
        Path::new(&std::env::var("PCTL_TMUX")?) == tmux,
        "context_mismatch: selected tmux differs from launcher context"
    );
    Ok(Core {
        state: selected_state,
        tmux,
    })
}

impl Core {
    /// Launcher environment is context, never a credential. No sender override is exposed.
    pub fn msg_caller(&self) -> Result<Caller> {
        fn env(key: &str) -> Result<String> {
            std::env::var(key).with_context(|| {
                format!("context_missing: {key} is absent; do not guess another run's identity")
            })
        }
        let caller = Caller {
            agent: env("PCTL_AGENT_ID")?,
            run: env("PCTL_RUN_ID")?,
            project: env("PCTL_PROJECT_ID")?,
        };
        ensure!(
            fs::canonicalize(env("PCTL_STATE")?)? == self.state,
            "context_mismatch: selected state differs from launcher context"
        );
        ensure!(
            Path::new(&env("PCTL_TMUX")?) == self.tmux,
            "context_mismatch: selected tmux differs from launcher context"
        );
        check_caller(&self.registry()?, &caller)?;
        Ok(caller)
    }
    pub fn msg_peers(&self, c: &Caller) -> Result<Value> {
        let r = self.registry()?;
        check_caller(&r, c)?;
        let snapshot = self.snapshot()?;
        let peers: Vec<_> = snapshot.agents.iter().filter(|v| v.agent.project_id == c.project && v.agent.id != c.agent).map(|v| {
            let unique = r.agents.iter().filter(|a| a.project_id == c.project && a.name == v.agent.name).count() == 1;
            json!({"role":v.agent.name,"unique":unique,"mailbox":v.agent.collaboration_mode == "mailbox", "process":v.process,
                "send_candidate":unique && v.agent.collaboration_mode == "mailbox" && v.process == "running" && v.guard_error.is_none()})
        }).collect();
        Ok(
            json!({"self":role_name(&r,&c.agent),"peers":peers,"identity_trust":"caller_claimed launcher context; same Unix user, not authentication","scope":"exact same-project roles; send rechecks current run"}),
        )
    }
    pub fn msg_send(&self, c: &Caller, q: &RoleSend) -> Result<Value> {
        validate_key(&q.key)?;
        ensure!(
            q.role.is_some() != q.reply.is_some(),
            "exactly one role or reply handle is required"
        );
        let _lock = self.lock()?;
        let r = self.registry()?;
        check_caller(&r, c)?;
        let store = self.read_mailbox(&c.project)?;
        let original = q
            .reply
            .as_deref()
            .map(|h| find_message(&store, c, h))
            .transpose()?;
        if let Some(m) = original {
            ensure!(
                m.to_agent_id == c.agent && m.to_run_id == c.run,
                "reply requires a received message for this exact caller run"
            );
        }
        let reply_id = original.map(|m| m.id.clone());
        let existing = store.messages.iter().find(|m| {
            m.from_agent_id == c.agent
                && m.from_run_id == c.run
                && m.facade.as_ref().is_some_and(|f| f.key == q.key)
        });
        let (to_agent, to_run, request_id) = if let Some(m) = existing {
            ensure!(
                m.facade.as_ref().unwrap().target_role == q.role
                    && m.in_reply_to == reply_id
                    && m.kind == q.kind
                    && m.subject == q.subject
                    && m.body == q.body,
                "idempotency_conflict: key already binds a different operation, role, reply, kind, subject or text"
            );
            // Preserve the first target/run even when roles have changed. Guards below
            // reject a restart instead of resolving to a newly launched recipient.
            (m.to_agent_id.clone(), m.to_run_id.clone(), m.id.clone())
        } else if let Some(m) = original {
            (
                m.from_agent_id.clone(),
                m.from_run_id.clone(),
                uuid::Uuid::new_v4().to_string(),
            )
        } else {
            let target = unique_role(&r, &c.project, q.role.as_deref().unwrap())?;
            let run = r
                .runs
                .iter()
                .find(|run| run.agent_id == target.id)
                .context("role has no current run")?;
            (
                target.id.clone(),
                run.id.clone(),
                uuid::Uuid::new_v4().to_string(),
            )
        };
        let request = SendRequest {
            request_id,
            from_agent: c.agent.clone(),
            from_run: c.run.clone(),
            to_agent,
            to_run,
            kind: q.kind.clone(),
            subject: q.subject.clone(),
            body: q.body.clone(),
            in_reply_to: reply_id,
        };
        let sent = self.send_message_locked(
            &r,
            &request,
            Some(FacadeOrigin {
                key: q.key.clone(),
                target_role: q.role.clone(),
            }),
        ).map_err(|error| {
            if format!("{error:#}").contains("outcome_unknown") {
                error.context(format!("retain the original --key {}; inspect existing evidence before an identical retry; never use a new key to hide uncertainty", q.key))
            } else { error }
        })?;
        let current = self.read_mailbox(&c.project)?;
        Ok(
            json!({"message":with_reply_handle(&r,&current,&sent.message,true),"key":q.key,"deduplicated":sent.deduplicated,"delivery":"stored_only; no automatic wake, PTY input or execution"}),
        )
    }
    pub fn msg_inbox(&self, c: &Caller, unacked: bool, after: u64, limit: usize) -> Result<Value> {
        let r = self.registry()?;
        check_caller(&r, c)?;
        let inbox = self.inbox(&c.agent, &c.run, unacked, after, limit)?;
        let store = self.read_mailbox(&c.project)?;
        let messages: Vec<_> = inbox
            .messages
            .iter()
            .map(|m| with_reply_handle(&r, &store, m, true))
            .collect();
        Ok(
            json!({"messages":messages,"next_after_sequence":inbox.next_after_sequence,"high_watermark":inbox.high_watermark,
            "semantics":"read-only snapshot; no retrieval receipt, acknowledgment, acceptance or completion is recorded"}),
        )
    }
    pub fn msg_status(&self, c: &Caller, reference: &str) -> Result<Value> {
        let r = self.registry()?;
        check_caller(&r, c)?;
        let store = self.read_mailbox(&c.project)?;
        let message = find_message(&store, c, reference)?;
        let mut evidence = with_reply_handle(&r, &store, message, false);
        evidence["binding"] = json!({"message_id":message.id,"project_id":message.project_id,"from_agent_id":message.from_agent_id,"from_run_id":message.from_run_id,"to_agent_id":message.to_agent_id,"to_run_id":message.to_run_id});
        Ok(json!({"message":evidence,
            "semantics":"stored, explicit-consume receipt, acknowledgment and completion claim are independent; null receipt does not prove unread; acknowledgment is not acceptance/approval; verified_result is never inferred"}))
    }
    pub fn msg_record(
        &self,
        c: &Caller,
        reference: &str,
        action: &str,
        summary: Option<&str>,
        artifact: Option<&str>,
    ) -> Result<Value> {
        ensure!(
            ["consume", "ack", "complete"].contains(&action),
            "unsupported receipt action"
        );
        if action == "complete" {
            bounded_printable(
                summary.context("completion summary required")?,
                512,
                "completion summary",
            )?;
            validate_artifact(artifact)?;
        }
        let _lock = self.lock()?;
        let r = self.registry()?;
        check_caller(&r, c)?;
        self.guard(&r, &c.agent, &c.run, true)?;
        let mut store = self.read_mailbox(&c.project)?;
        let target = find_message(&store, c, reference)?;
        ensure!(
            target.to_agent_id == c.agent && target.to_run_id == c.run,
            "receipt/completion requires the exact recipient run"
        );
        if action == "complete" {
            ensure!(
                target.kind == "handoff",
                "completion claims require a handoff message"
            );
        }
        let index = store
            .messages
            .iter()
            .position(|m| m.id == target.id)
            .unwrap();
        let m = &mut store.messages[index];
        match action {
            "consume" => {
                m.retrieval.get_or_insert_with(|| RetrievalReceipt {
                    recorded_at: now(),
                    source: RECEIPT_SOURCE.into(),
                });
            }
            "ack" => {
                m.acked_at.get_or_insert_with(now);
            }
            "complete" => {
                let summary = summary.unwrap();
                if let Some(claim) = &m.completion {
                    ensure!(
                        claim.summary == summary && claim.artifact_path.as_deref() == artifact,
                        "idempotency_conflict: completion already recorded with different summary/artifact"
                    );
                } else {
                    m.completion = Some(CompletionClaim {
                        reported_at: now(),
                        summary: summary.into(),
                        artifact_path: artifact.map(str::to_owned),
                        evidence: COMPLETION_EVIDENCE.into(),
                    });
                }
            }
            _ => unreachable!(),
        }
        ensure_mailbox_capacity(&store)?;
        atomic_json(&self.mailbox_path(&c.project),&store).context("receipt persistence failed; outcome_unknown: inspect msg status before retrying the same handle")?;
        Ok(
            json!({"message":with_reply_handle(&r,&store,&store.messages[index],true),"action":action,
            "semantics":"consume records a request, not understanding; ack is receipt only; complete is a recipient self-report, never permission, approval or verified success"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_messages_have_no_fabricated_receipts_or_completion() {
        let message: Message = serde_json::from_value(json!({
            "id":"legacy", "sequence":1, "project_id":"p",
            "from_agent_id":"a", "from_run_id":"ra", "to_agent_id":"b", "to_run_id":"rb",
            "kind":"handoff", "subject":"legacy task", "body":"bounded", "correlation_id":"legacy",
            "in_reply_to":null, "created_at":1, "acked_at":null, "sender_trust":"caller_claimed"
        }))
        .unwrap();
        assert!(
            message.retrieval.is_none() && message.completion.is_none() && message.facade.is_none()
        );
        let serialized = serde_json::to_value(message).unwrap();
        assert!(serialized.get("retrieval").is_none() && serialized.get("completion").is_none());
    }
    #[test]
    fn exact_role_rejects_all_duplicates_and_does_not_normalize() {
        let d = tempfile::tempdir().unwrap();
        let c = Core::new(d.path().join("s"), "tmux".into()).unwrap();
        let p = c.register_project("p", d.path()).unwrap();
        let other = c.register_project("other", d.path()).unwrap();
        c.register_agent(
            &p.id,
            "reviewer",
            d.path(),
            vec!["/bin/cat".into()],
            "generic",
        )
        .unwrap();
        c.register_agent(
            &other.id,
            "reviewer",
            d.path(),
            vec!["/bin/cat".into()],
            "generic",
        )
        .unwrap();
        let r = c.registry().unwrap();
        assert!(unique_role(&r, &p.id, "reviewer").is_ok());
        for role in ["Reviewer", "reviewer ", " review", "review"] {
            assert!(unique_role(&r, &p.id, role).is_err());
        }
        c.register_agent(
            &p.id,
            "reviewer",
            d.path(),
            vec!["/bin/cat".into()],
            "generic",
        )
        .unwrap();
        assert!(
            unique_role(&c.registry().unwrap(), &p.id, "reviewer")
                .unwrap_err()
                .to_string()
                .contains("ambiguous_role")
        );
    }
    #[test]
    fn keys_and_artifact_references_are_bounded_inert_data() {
        for key in ["one", "review-1", "reply_2.3"] {
            assert!(validate_key(key).is_ok());
        }
        for key in ["", "../abc", "a b", "한글", &"x".repeat(65)] {
            assert!(validate_key(key).is_err());
        }
        assert!(validate_artifact(None).is_ok());
        assert!(validate_artifact(Some("results/review.json")).is_ok());
        for path in [
            "/etc/passwd",
            "../escape",
            "foo/../escape",
            "./file",
            "a/./b",
            "a//b",
            "",
            "line\nfile",
        ] {
            assert!(validate_artifact(Some(path)).is_err(), "{path}");
        }
    }
}
