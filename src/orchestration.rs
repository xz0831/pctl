//! Cooperative, operator-configured orchestration of registered same-project profiles.
//! The same Unix user can bypass this facade; it is not authentication or a sandbox.
use crate::{Agent, Core, Registry, Run, atomic_json, now};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 1024;
const MAX_GRANTS: usize = 128;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Launch,
    Focus,
    Pair,
    Unpair,
    Zoom,
    Stage,
    Interrupt,
    Stop,
    Restart,
}

#[derive(Args, Debug)]
pub struct ChildArg {
    /// Exact returned child handle, for example c1. Never a pane number.
    pub child: String,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Operator only: grant bounded capabilities to an exact currently live controller run.
    Grant {
        #[arg(long)]
        controller: String,
        #[arg(long)]
        controller_run: String,
        #[arg(long, required = true)]
        target: Vec<String>,
        #[arg(long, value_enum, required = true)]
        allow: Vec<Capability>,
        #[arg(long, default_value_t = 4)]
        max_live: usize,
        #[arg(long, default_value_t = 8)]
        max_launches: usize,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: u64,
        /// Exact previous grant ID required to replace its configuration; budgets do not reset.
        #[arg(long)]
        replace_grant: Option<String>,
    },
    /// Operator only: revoke an exact grant immediately; existing children remain running.
    Revoke {
        #[arg(long)]
        grant: String,
    },
    /// Operator inspection of stored grants, attempts and ownership; no mutation.
    Policy,
    /// Discover only this exact controller run's granted profiles and limits.
    Profiles,
    /// Inspect own exact children; old handles remain historical and never retarget.
    Children,
    /// Start one preconfigured profile in its registered cwd. No arbitrary command or cwd input.
    Launch {
        profile: String,
        #[arg(long)]
        key: String,
    },
    /// Select one owned native child pane; may switch the sole attached client's view.
    Focus(ChildArg),
    /// Join existing native panes. `self` is the exact live controller, not a new process.
    Pair {
        child: String,
        #[arg(long)]
        with: String,
    },
    /// Move an existing child into a separate window. Does not spawn a process.
    Unpair(ChildArg),
    /// Toggle native pane zoom; do not automatically retry an uncertain response.
    Zoom(ChildArg),
    /// Store a local draft bound to the child run. Nothing is sent or submitted.
    Stage {
        child: String,
        #[arg(long)]
        text: String,
    },
    /// Send literal Ctrl-C only. Cancellation/SIGINT is unverified; never approval.
    Interrupt(ChildArg),
    /// Remove this exact child pane/PTY. This is process stop, not UI close or client detach.
    Stop(ChildArg),
    /// Replace one exact owned child with a new run and new handle. Requires a new stable key.
    Restart {
        child: String,
        #[arg(long)]
        key: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Grant {
    id: String,
    controller: Agent,
    controller_run: String,
    project_root: String,
    targets: Vec<Agent>,
    allow: Vec<Capability>,
    max_live: usize,
    max_launches: usize,
    created_at: u64,
    expires_at: u64,
    revoked_at: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attempt {
    sequence: u64,
    controller_id: String,
    controller_run: String,
    project_id: String,
    grant_id: String,
    key: String,
    operation: String,
    reference: String,
    target_id: String,
    created_at: u64,
    state: String,
    child: Option<Run>,
}
#[derive(Default, Debug, Serialize, Deserialize)]
struct Store {
    schema_version: u32,
    grants: Vec<Grant>,
    attempts: Vec<Attempt>,
}
#[derive(Clone)]
struct Caller {
    agent: String,
    run: String,
    project: String,
}

fn load(core: &Core) -> Result<Store> {
    let path = core.state.join("orchestration.json");
    if !path.exists() {
        return Ok(Store {
            schema_version: 1,
            ..Store::default()
        });
    }
    ensure!(
        fs::metadata(&path)?.len() <= MAX_BYTES,
        "orchestration_capacity: store exceeds 2 MiB"
    );
    let store: Store = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        store.schema_version == 1,
        "unsupported orchestration schema"
    );
    ensure!(
        store.grants.len() <= MAX_GRANTS && store.attempts.len() <= MAX_ATTEMPTS,
        "orchestration_capacity: record count exceeded"
    );
    Ok(store)
}
fn save(core: &Core, store: &Store) -> Result<()> {
    ensure!(
        store.grants.len() <= MAX_GRANTS && store.attempts.len() <= MAX_ATTEMPTS,
        "orchestration_capacity: record count exceeded"
    );
    // Reserve enough for each pending attempt's eventual exact Run binding.
    let reserve = store.attempts.iter().filter(|a| a.child.is_none()).count() * 4096
        + store
            .grants
            .iter()
            .filter(|g| g.revoked_at.is_none())
            .count()
            * 20;
    ensure!(
        serde_json::to_vec_pretty(store)?.len() + reserve < MAX_BYTES as usize,
        "orchestration_capacity: 2 MiB ledger limit including pending-run reserve"
    );
    atomic_json(&core.state.join("orchestration.json"), store)
}
fn operator_only() -> Result<()> {
    ensure!(
        std::env::var_os("PCTL_AGENT_ID").is_none() && std::env::var_os("PCTL_RUN_ID").is_none(),
        "operator_only: launched agents cannot configure their own orchestration grants"
    );
    Ok(())
}
fn caller(core: &Core) -> Result<Caller> {
    fn env(key: &str) -> Result<String> {
        std::env::var(key)
            .with_context(|| format!("context_missing: {key}; use the actual launcher context"))
    }
    ensure!(
        fs::canonicalize(env("PCTL_STATE")?)? == core.state,
        "context_mismatch: orchestration state differs from launcher context"
    );
    ensure!(
        Path::new(&env("PCTL_TMUX")?) == core.tmux,
        "context_mismatch: orchestration tmux differs from launcher context"
    );
    Ok(Caller {
        agent: env("PCTL_AGENT_ID")?,
        run: env("PCTL_RUN_ID")?,
        project: env("PCTL_PROJECT_ID")?,
    })
}
fn profile<'a>(r: &'a Registry, id: &str) -> Result<&'a Agent> {
    r.agents
        .iter()
        .find(|a| a.id == id)
        .context("agent not found")
}
fn validate_path(root: &str, a: &Agent) -> Result<()> {
    let root_path = fs::canonicalize(root).context("project root unavailable; controls locked")?;
    ensure!(
        root_path == Path::new(root),
        "project root identity changed; controls locked"
    );
    let cwd = fs::canonicalize(&a.cwd).context("registered cwd unavailable; controls locked")?;
    ensure!(
        cwd == Path::new(&a.cwd) && cwd.is_dir() && cwd.starts_with(&root_path),
        "registered cwd outside registered project or identity changed; controls locked"
    );
    Ok(())
}
fn checked_grant<'a>(core: &Core, r: &Registry, s: &'a Store, c: &Caller) -> Result<&'a Grant> {
    let a = profile(r, &c.agent)?;
    ensure!(
        a.project_id == c.project,
        "context_mismatch: controller project differs"
    );
    core.guard(r, &c.agent, &c.run, true)?;
    let g = s
        .grants
        .iter()
        .rev()
        .find(|g| g.controller.id == c.agent && g.controller_run == c.run && g.revoked_at.is_none())
        .context("capability_denied: no operator grant for this exact controller run")?;
    ensure!(
        now() < g.expires_at,
        "capability_denied: operator grant expired"
    );
    ensure!(
        &g.controller == a,
        "capability_denied: controller profile changed since grant"
    );
    let p = r
        .projects
        .iter()
        .find(|p| p.id == c.project)
        .context("project not found")?;
    ensure!(
        p.root == g.project_root,
        "project root identity changed; controls locked"
    );
    validate_path(&g.project_root, a)?;
    Ok(g)
}
fn allow(g: &Grant, cap: Capability) -> Result<()> {
    ensure!(
        g.allow.contains(&cap),
        "capability_denied: {cap:?} is not granted"
    );
    Ok(())
}
fn checked_target<'a>(r: &'a Registry, g: &Grant, id: &str) -> Result<&'a Agent> {
    let frozen = g
        .targets
        .iter()
        .find(|a| a.id == id)
        .context("capability_denied: target profile is not granted")?;
    let a = profile(r, id)?;
    ensure!(
        a.project_id == g.controller.project_id,
        "cross-project orchestration prohibited"
    );
    ensure!(
        a.id != g.controller.id,
        "capability_denied: controller cannot be its own child"
    );
    ensure!(
        a == frozen,
        "capability_denied: target profile changed since grant; operator must review it"
    );
    validate_path(&g.project_root, a)?;
    Ok(a)
}
fn key_valid(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 64
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "key must be 1..64 ASCII letters, digits, hyphens, underscores or dots"
    );
    Ok(())
}
fn handle(a: &Attempt) -> String {
    format!("c{}", a.sequence)
}
fn own_attempt<'a>(s: &'a Store, c: &Caller, child: &str) -> Result<&'a Attempt> {
    s.attempts
        .iter()
        .find(|a| {
            handle(a) == child
                && a.controller_id == c.agent
                && a.controller_run == c.run
                && a.project_id == c.project
        })
        .context("ownership_denied: child handle not owned by this exact controller run")
}
fn owned_run<'a>(
    core: &Core,
    r: &Registry,
    s: &'a Store,
    g: &Grant,
    c: &Caller,
    child: &str,
    alive: bool,
) -> Result<&'a Run> {
    let a = own_attempt(s, c, child)?;
    ensure!(
        a.state == "started",
        "outcome_unknown: launch is unresolved; inspect before any retry"
    );
    let run = a
        .child
        .as_ref()
        .context("outcome_unknown: child binding unavailable")?;
    checked_target(r, g, &run.agent_id)?;
    core.guard(r, &run.agent_id, &run.id, alive)?;
    Ok(run)
}
fn attempt_view(core: &Core, r: &Registry, a: &Attempt) -> Value {
    let (current, verified, error) = if let Some(run) = &a.child {
        let current = r
            .runs
            .iter()
            .any(|n| n.agent_id == run.agent_id && n.id == run.id);
        let check = if current {
            core.guard(r, &run.agent_id, &run.id, false).map(|_| ())
        } else {
            Err(anyhow::anyhow!(
                "run is no longer current; handle remains historical"
            ))
        };
        (current, check.is_ok(), check.err().map(|e| e.to_string()))
    } else {
        (
            false,
            false,
            Some("launch outcome unresolved; operator inspection required".into()),
        )
    };
    json!({"child":handle(a),"profile_id":a.target_id,"operation":a.operation,"key":a.key,
        "launch_state":a.state,"binding":a.child,"current":current,"identity_verified":verified,"guard_error":error,
        "task":"unknown","semantics":"owned exact run; process identity is not task completion"})
}
fn quota(s: &Store, r: &Registry, c: &Caller) -> (usize, usize) {
    let own: Vec<_> = s
        .attempts
        .iter()
        .filter(|a| a.controller_id == c.agent && a.controller_run == c.run)
        .collect();
    let occupied = own
        .iter()
        .filter(|a| {
            a.state != "started"
                || a.child
                    .as_ref()
                    .is_some_and(|ch| r.runs.iter().any(|run| run.id == ch.id))
        })
        .count();
    (occupied, own.len())
}
fn mutate_result(
    core: &Core,
    c: &Caller,
    action: &str,
    run: &Run,
    semantics: &str,
    result: Result<()>,
) -> Result<Value> {
    result.with_context(|| format!("outcome_unknown: {action} did not complete cleanly; inspect and do not retry automatically"))?;
    core.audit(
        &format!("team_{action}"),
        &run.id,
        &format!("controller_run={}; {semantics}", c.run),
    )
    .context("outcome_unknown: action returned success but audit failed; do not retry")?;
    Ok(json!({"ok":true,"run_id":run.id,"result":semantics}))
}

#[allow(clippy::too_many_arguments)]
fn grant(
    core: &Core,
    controller: &str,
    controller_run: &str,
    targets: &[String],
    caps: &[Capability],
    max_live: usize,
    max_launches: usize,
    ttl: u64,
    replace: Option<&str>,
) -> Result<Value> {
    operator_only()?;
    ensure!(
        (1..=16).contains(&max_live) && (1..=64).contains(&max_launches),
        "limits: max-live 1..16 and max-launches 1..64"
    );
    ensure!(
        (1..=86400).contains(&ttl),
        "grant ttl must be 1..86400 seconds"
    );
    ensure!(
        !targets.is_empty() && targets.len() <= 16 && !caps.is_empty(),
        "grant needs 1..16 targets and explicit capabilities"
    );
    let _lock = core.lock()?;
    let r = core.registry()?;
    let mut s = load(core)?;
    let actor = profile(&r, controller)?.clone();
    core.guard(&r, controller, controller_run, true)?;
    let root = r
        .projects
        .iter()
        .find(|p| p.id == actor.project_id)
        .context("project not found")?
        .root
        .clone();
    validate_path(&root, &actor)?;
    let mut frozen = vec![];
    for target in targets {
        let a = profile(&r, target)?;
        ensure!(
            a.id != actor.id,
            "controller cannot grant itself as a child"
        );
        ensure!(
            a.project_id == actor.project_id,
            "cross-project orchestration prohibited"
        );
        validate_path(&root, a)?;
        ensure!(
            !frozen.iter().any(|b: &Agent| b.id == a.id),
            "duplicate target grant"
        );
        ensure!(
            r.agents
                .iter()
                .filter(|b| b.project_id == actor.project_id && b.name == a.name)
                .count()
                == 1,
            "ambiguous_role: target name must be unique in the project"
        );
        frozen.push(a.clone());
    }
    let old = s
        .grants
        .iter()
        .rposition(|g| g.controller.id == controller && g.controller_run == controller_run);
    match (old, replace) {
        (Some(i), Some(id)) if s.grants[i].id == id => s.grants[i].revoked_at = Some(now()),
        (None, None) => {}
        _ => {
            bail!("state_conflict: replacing a grant requires its exact latest --replace-grant ID")
        }
    }
    let g = Grant {
        id: crate::id(),
        controller: actor,
        controller_run: controller_run.into(),
        project_root: root,
        targets: frozen,
        allow: caps.to_vec(),
        max_live,
        max_launches,
        created_at: now(),
        expires_at: now() + ttl,
        revoked_at: None,
    };
    s.grants.push(g.clone());
    save(core, &s)?;
    core.audit(
        "team_grant",
        controller_run,
        &format!(
            "grant_id={}; explicit operator configuration; not authenticated user approval",
            g.id
        ),
    )
    .context("outcome_unknown: grant saved but audit failed; inspect policy")?;
    Ok(
        json!({"grant":g,"semantics":"cooperative exact-run capabilities; does not expand user authorization; no sandbox"}),
    )
}

#[allow(clippy::too_many_arguments)]
fn launch(
    core: &Core,
    r: &mut Registry,
    s: &mut Store,
    g: &Grant,
    c: &Caller,
    reference: &str,
    key: &str,
    restart: bool,
) -> Result<Value> {
    key_valid(key)?;
    let operation = if restart { "restart" } else { "launch" };
    allow(
        g,
        if restart {
            Capability::Restart
        } else {
            Capability::Launch
        },
    )?;
    if let Some(previous) = s
        .attempts
        .iter()
        .find(|a| a.controller_id == c.agent && a.controller_run == c.run && a.key == key)
    {
        ensure!(
            previous.operation == operation && previous.reference == reference,
            "idempotency_conflict: key already binds a different operation or reference"
        );
        ensure!(
            previous.state == "started",
            "outcome_unknown: original launch attempt unresolved; no second process created"
        );
        return Ok(
            json!({"deduplicated":true,"result":attempt_view(core,r,previous),"semantics":"original binding returned; no launch or retarget"}),
        );
    }
    let old = if restart {
        Some(owned_run(core, r, s, g, c, reference, false)?.clone())
    } else {
        None
    };
    let target = if let Some(run) = &old {
        checked_target(r, g, &run.agent_id)?.clone()
    } else {
        let named: Vec<_> = r
            .agents
            .iter()
            .filter(|a| a.project_id == c.project && a.name == reference)
            .collect();
        ensure!(
            named.len() == 1,
            "ambiguous_role: launch requires exactly one registered same-project profile with this name"
        );
        checked_target(r, g, &named[0].id)?.clone()
    };
    if !restart {
        ensure!(
            !r.runs.iter().any(|run| run.agent_id == target.id),
            "existing run: no implicit adoption; use owned exact child handle"
        );
    }
    ensure!(
        !core
            .state
            .join(format!("pending-{}.json", target.id))
            .exists(),
        "unresolved launch intent; operator inspection required"
    );
    let (occupied, attempts) = quota(s, r, c);
    ensure!(
        attempts < g.max_launches,
        "launch_limit: per-controller-run launch budget reached"
    );
    // Restart replaces one owned slot; pending intent still consumes the launch budget.
    ensure!(
        occupied < g.max_live || (restart && occupied <= g.max_live),
        "launch_limit: live or unresolved child limit reached"
    );
    ensure!(
        s.attempts.len() < MAX_ATTEMPTS,
        "orchestration_capacity: 1024 attempt limit"
    );
    let sequence = s.attempts.last().map_or(1, |a| a.sequence + 1);
    s.attempts.push(Attempt {
        sequence,
        controller_id: c.agent.clone(),
        controller_run: c.run.clone(),
        project_id: c.project.clone(),
        grant_id: g.id.clone(),
        key: key.into(),
        operation: operation.into(),
        reference: reference.into(),
        target_id: target.id.clone(),
        created_at: now(),
        state: "pending".into(),
        child: None,
    });
    save(core, s)?; // Durable debit and intent BEFORE any pane/process mutation.
    let result = if let Some(run) = old {
        core.restart_locked(r, &run.agent_id, &run.id)
    } else {
        core.start_locked(r, &target.id)
    };
    match result {
        Ok(run) => {
            let a = s.attempts.last_mut().context("missing launch intent")?;
            a.state = "started".into();
            a.child = Some(run.clone());
            save(core,s).context("outcome_unknown: process start returned but ownership persistence failed; do not retry")?;
            core.audit(&format!("team_{operation}"),&run.id,&format!("controller_run={}; child=c{sequence}",c.run))
                .context("outcome_unknown: start and ownership saved but audit failed; inspect using original key")?;
            Ok(
                json!({"deduplicated":false,"result":attempt_view(core,r,s.attempts.last().unwrap()),"semantics":"registered native process launched; model readiness and task acceptance unverified"}),
            )
        }
        Err(error) => {
            s.attempts.last_mut().unwrap().state = "uncertain".into();
            let _ = save(core, s);
            bail!(
                "outcome_unknown: {operation} attempt c{sequence} is unresolved; original key retained; no automatic retry: {error:#}"
            )
        }
    }
}

pub fn requires_context(command: &Command) -> bool {
    !matches!(
        command,
        Command::Grant { .. } | Command::Revoke { .. } | Command::Policy
    )
}
/// Must be called before Core initialization, which can otherwise create state.
pub fn preflight(command: &Command) -> Result<()> {
    if !requires_context(command) {
        operator_only()?;
    }
    Ok(())
}

pub fn run(core: &Core, command: Command) -> Result<Value> {
    match command {
        Command::Grant {
            controller,
            controller_run,
            target,
            allow,
            max_live,
            max_launches,
            ttl_seconds,
            replace_grant,
        } => {
            return grant(
                core,
                &controller,
                &controller_run,
                &target,
                &allow,
                max_live,
                max_launches,
                ttl_seconds,
                replace_grant.as_deref(),
            );
        }
        Command::Revoke { grant } => {
            operator_only()?;
            let _lock = core.lock()?;
            let mut s = load(core)?;
            let g = s
                .grants
                .iter_mut()
                .find(|g| g.id == grant)
                .context("grant not found")?;
            g.revoked_at.get_or_insert_with(now);
            let result = g.clone();
            save(core, &s)?;
            core.audit(
                "team_revoke",
                &result.controller_run,
                &format!("grant_id={grant}"),
            )
            .context("outcome_unknown: revocation saved but audit failed; inspect policy")?;
            return Ok(
                json!({"grant":result,"result":"revoked; existing children unchanged; operator may stop them explicitly"}),
            );
        }
        Command::Policy => {
            operator_only()?;
            let _lock = core.lock()?;
            return Ok(
                json!({"policy":load(core)?,"security_boundary":false,"semantics":"operator configuration and historical exact ownership; not user approval"}),
            );
        }
        _ => {}
    }
    let c = caller(core)?;
    let _lock = core.lock()?;
    let mut r = core.registry()?;
    let mut s = load(core)?;
    // Historical read-only inspection survives grant expiry and controller exit.
    if matches!(command, Command::Children) {
        ensure!(
            profile(&r, &c.agent)?.project_id == c.project,
            "context_mismatch: controller project differs"
        );
        return Ok(
            json!({"children":s.attempts.iter().filter(|a|a.controller_id==c.agent&&a.controller_run==c.run&&a.project_id==c.project).map(|a|attempt_view(core,&r,a)).collect::<Vec<_>>(),
            "controller_current":r.runs.iter().any(|run|run.agent_id==c.agent&&run.id==c.run),
            "semantics":"read-only historical exact bindings; no authority to mutate"}),
        );
    }
    let g = checked_grant(core, &r, &s, &c)?.clone();
    match command {
        Command::Profiles => {
            let (occupied, attempts) = quota(&s, &r, &c);
            let profiles: Vec<_>=g.targets.iter().map(|a|{
                let check=checked_target(&r,&g,&a.id);
                let current=r.runs.iter().find(|run|run.agent_id==a.id);
                json!({"profile":a.name,"id":a.id,"cwd":a.cwd,"registered_argv":a.argv,"profile_verified":check.is_ok(),
                    "profile_error":check.as_ref().err().map(|e|e.to_string()),"current_run":current,
                    "launch_candidate":check.is_ok() && g.allow.contains(&Capability::Launch) && occupied < g.max_live && attempts < g.max_launches && current.is_none() && !core.state.join(format!("pending-{}.json",a.id)).exists(),
                    "task":"unknown"})
            }).collect();
            Ok(
                json!({"profiles":profiles,"allowed":g.allow,"limits":{"max_live":g.max_live,"occupied_slots":occupied,"max_launches":g.max_launches,"attempts":attempts,"expires_at":g.expires_at},
                "identity_trust":"caller_claimed launcher context; same Unix user, not authentication"}),
            )
        }
        Command::Children => Ok(
            json!({"children":s.attempts.iter().filter(|a|a.controller_id==c.agent&&a.controller_run==c.run).map(|a|attempt_view(core,&r,a)).collect::<Vec<_>>()}),
        ),
        Command::Launch { profile, key } => {
            launch(core, &mut r, &mut s, &g, &c, &profile, &key, false)
        }
        Command::Restart { child, key } => launch(core, &mut r, &mut s, &g, &c, &child, &key, true),
        Command::Focus(q) => {
            allow(&g, Capability::Focus)?;
            let child = owned_run(core, &r, &s, &g, &c, &q.child, false)?.clone();
            mutate_result(
                core,
                &c,
                "focus",
                &child,
                "native pane selected; sole client view may change",
                core.focus_locked(&r, &child.agent_id, &child.id),
            )
        }
        Command::Pair { child, with } => {
            allow(&g, Capability::Pair)?;
            let child = owned_run(core, &r, &s, &g, &c, &child, false)?.clone();
            let other = if with == "self" {
                r.runs
                    .iter()
                    .find(|run| run.agent_id == c.agent && run.id == c.run)
                    .context("no current controller run")?
                    .clone()
            } else {
                owned_run(core, &r, &s, &g, &c, &with, false)?.clone()
            };
            mutate_result(
                core,
                &c,
                "pair",
                &child,
                "existing native panes joined; no process spawned",
                core.pair_locked(&r, &other.agent_id, &other.id, &child.agent_id, &child.id),
            )
        }
        Command::Unpair(q) => {
            allow(&g, Capability::Unpair)?;
            let child = owned_run(core, &r, &s, &g, &c, &q.child, false)?.clone();
            mutate_result(
                core,
                &c,
                "unpair",
                &child,
                "existing child moved to its own window; no process spawned",
                core.split_locked(&r, &child.agent_id, &child.id),
            )
        }
        Command::Zoom(q) => {
            allow(&g, Capability::Zoom)?;
            let child = owned_run(core, &r, &s, &g, &c, &q.child, false)?.clone();
            mutate_result(
                core,
                &c,
                "zoom",
                &child,
                "native zoom toggled; do not automatically retry",
                core.zoom_locked(&r, &child.agent_id, &child.id),
            )
        }
        Command::Stage { child, text } => {
            allow(&g, Capability::Stage)?;
            let child = owned_run(core, &r, &s, &g, &c, &child, true)?.clone();
            ensure!(text.len() <= 8192, "stage text exceeds 8192 bytes");
            core.save_draft_locked(&r, &child.agent_id, &child.id, &text)?;
            Ok(
                json!({"ok":true,"run_id":child.id,"result":"stored locally; nothing transmitted or submitted"}),
            )
        }
        Command::Interrupt(q) => {
            allow(&g, Capability::Interrupt)?;
            let child = owned_run(core, &r, &s, &g, &c, &q.child, true)?.clone();
            mutate_result(
                core,
                &c,
                "interrupt",
                &child,
                "literal Ctrl-C delivered; interruption and cancellation unverified",
                core.send_ctrl_c_locked(&r, &child.agent_id, &child.id),
            )
        }
        Command::Stop(q) => {
            allow(&g, Capability::Stop)?;
            let child = owned_run(core, &r, &s, &g, &c, &q.child, false)?.clone();
            mutate_result(
                core,
                &c,
                "stop",
                &child,
                "exact child pane removed and PTY closed; external descendants may survive",
                core.terminate_locked(&mut r, &child.agent_id, &child.id),
            )
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Core, Registry, Grant, Caller) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().join("s"), "tmux".into()).unwrap();
        let cwd = fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let actor = Agent {
            id: "main".into(),
            project_id: "project".into(),
            name: "main".into(),
            cwd: cwd.clone(),
            argv: vec!["/bin/sleep".into(), "30".into()],
            adapter: "generic".into(),
            input_mode: "native".into(),
            collaboration_mode: "disabled".into(),
            conversation: None,
            worktree: None,
        };
        let mut child = actor.clone();
        child.id = "worker".into();
        child.name = "worker".into();
        let grant = Grant {
            id: "grant".into(),
            controller: actor.clone(),
            controller_run: "controller-run".into(),
            project_root: cwd,
            targets: vec![child.clone()],
            allow: vec![Capability::Launch],
            max_live: 1,
            max_launches: 2,
            created_at: 0,
            expires_at: u64::MAX,
            revoked_at: None,
        };
        let registry = Registry {
            agents: vec![actor, child],
            ..Registry::default()
        };
        let caller = Caller {
            agent: "main".into(),
            run: "controller-run".into(),
            project: "project".into(),
        };
        (dir, core, registry, grant, caller)
    }
    fn pending(c: &Caller) -> Attempt {
        Attempt {
            sequence: 1,
            controller_id: c.agent.clone(),
            controller_run: c.run.clone(),
            project_id: c.project.clone(),
            grant_id: "grant".into(),
            key: "worker-one".into(),
            operation: "launch".into(),
            reference: "worker".into(),
            target_id: "worker".into(),
            created_at: 0,
            state: "pending".into(),
            child: None,
        }
    }
    #[test]
    fn unresolved_attempt_is_debited_and_cannot_replay_or_reset() {
        let (_dir, core, mut r, mut g, c) = fixture();
        let mut s = Store {
            schema_version: 1,
            grants: vec![g.clone()],
            attempts: vec![pending(&c)],
        };
        save(&core, &s).unwrap();
        assert_eq!(quota(&s, &r, &c), (1, 1));
        let err = launch(&core, &mut r, &mut s, &g, &c, "worker", "worker-one", false).unwrap_err();
        assert!(err.to_string().contains("outcome_unknown"));
        assert_eq!(s.attempts.len(), 1);
        assert!(r.runs.is_empty());
        // Replacing configuration cannot erase the exact run's attempt debit.
        g.id = "new-grant".into();
        s.grants.push(g);
        assert_eq!(quota(&s, &r, &c), (1, 1));
        assert_eq!(load(&core).unwrap().attempts.len(), 1);
    }
    #[test]
    fn child_handles_are_scoped_and_never_normalized_or_rebound() {
        let (_dir, _core, _r, _g, c) = fixture();
        let s = Store {
            schema_version: 1,
            grants: vec![],
            attempts: vec![pending(&c)],
        };
        assert!(own_attempt(&s, &c, "c1").is_ok());
        assert!(own_attempt(&s, &c, "c01").is_err());
        for foreign in [
            Caller {
                agent: "other".into(),
                ..c.clone()
            },
            Caller {
                run: "next-run".into(),
                ..c.clone()
            },
            Caller {
                project: "other".into(),
                ..c.clone()
            },
        ] {
            assert!(own_attempt(&s, &foreign, "c1").is_err());
        }
    }
    #[test]
    fn all_frozen_profile_fields_fail_closed_on_drift() {
        let (_dir, _core, r, g, _c) = fixture();
        assert!(checked_target(&r, &g, "worker").is_ok());
        for field in [
            "name",
            "cwd",
            "argv",
            "adapter",
            "input",
            "collaboration",
            "project",
        ] {
            let mut changed = r.clone();
            let a = changed
                .agents
                .iter_mut()
                .find(|a| a.id == "worker")
                .unwrap();
            match field {
                "name" => a.name = "renamed".into(),
                "cwd" => a.cwd = "/tmp".into(),
                "argv" => a.argv.push("extra".into()),
                "adapter" => a.adapter = "synthetic".into(),
                "input" => a.input_mode = "terminal-line".into(),
                "collaboration" => a.collaboration_mode = "mailbox".into(),
                "project" => a.project_id = "other".into(),
                _ => unreachable!(),
            }
            assert!(checked_target(&changed, &g, "worker").is_err(), "{field}");
        }
    }
    #[test]
    fn keys_are_short_explicit_and_safe() {
        for good in ["spawn-review-1", "a.b_c", "1"] {
            assert!(key_valid(good).is_ok());
        }
        for bad in ["", "../escape", "x y", "\x1b", "한글"] {
            assert!(key_valid(bad).is_err());
        }
        assert!(key_valid(&"a".repeat(65)).is_err());
    }
    #[test]
    fn revocation_remains_possible_at_ledger_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().into(), "tmux".into()).unwrap();
        let agent = Agent {
            id: "a".into(),
            project_id: "p".into(),
            name: "main".into(),
            cwd: "/tmp".into(),
            argv: vec![String::new()],
            adapter: "generic".into(),
            input_mode: "native".into(),
            collaboration_mode: "disabled".into(),
            conversation: None,
            worktree: None,
        };
        let g = Grant {
            id: "g".into(),
            controller: agent,
            controller_run: "r".into(),
            project_root: "/tmp".into(),
            targets: vec![],
            allow: vec![Capability::Launch],
            max_live: 1,
            max_launches: 1,
            created_at: 0,
            expires_at: u64::MAX,
            revoked_at: None,
        };
        let mut s = Store {
            schema_version: 1,
            grants: vec![g],
            attempts: vec![],
        };
        let baseline = serde_json::to_vec_pretty(&s).unwrap().len();
        s.grants[0].controller.argv[0] = "x".repeat(MAX_BYTES as usize - baseline - 21);
        save(&core, &s).unwrap();
        s.grants[0].revoked_at = Some(u64::MAX);
        save(&core, &s).unwrap();
        assert_eq!(load(&core).unwrap().grants[0].revoked_at, Some(u64::MAX));
        // A new large replacement is rejected atomically; revocation remains persisted.
        let extra = s.grants[0].clone();
        s.grants.push(extra);
        assert!(save(&core, &s).is_err());
        assert_eq!(load(&core).unwrap().grants.len(), 1);
    }
    #[test]
    fn unknown_store_schema_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(dir.path().into(), "tmux".into()).unwrap();
        fs::write(
            dir.path().join("orchestration.json"),
            r#"{"schema_version":9,"grants":[],"attempts":[]}"#,
        )
        .unwrap();
        assert!(load(&core).is_err());
    }
}
