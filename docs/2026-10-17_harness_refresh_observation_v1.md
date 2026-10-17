# Harness refresh observation (PR-A)

This release only observes. `pctl session refresh --idle --dry-run` reports proposed Claude swaps and skip reasons. There is no automatic stop/start path, observer-triggered swap, graceful quit, handover, quarantine, scheduler installation or daemon in PR-A. Named `session refresh NAME` remains the explicit step-2 command.

## Operator hook rollout

Merge these entries into the existing Claude settings `hooks` object; preserve all other settings/hooks. This document does not install them. The command guard is inert outside a PCTL-launched session. The readiness handler validates the conversation captured at launch; it does not create or overwrite a conversation ID. Existing unrelated hook entries remain separate. A run needs a launch record from this release and a real SessionStart event processed by this new handler. A later hook or mutable registry ID cannot rebind readiness to another conversation. Reloading other hook entries alone does not mark an already-running session instrumented. The operator must arrange a normal same-conversation restart at a suitable idle time for existing sessions that have not emitted that event after installation. The worker must not restart real sessions or edit these settings.

Validate hook payload support on the installed build before relying on observations. The [official Stop hook schema](https://code.claude.com/docs/en/hooks#stop-input) documents task and scheduled-work arrays. Missing arrays mean unknown, never empty. The handler stores only IDs, counters, booleans, counts and timestamps, never prompt/tool bodies, transcript paths, assistant messages or environment dumps. Tool/subagent starts invalidate readiness; completion alone cannot create readiness. Stop requires an instrumented run, matching conversation/current input epoch, no outstanding tool/subagent, explicit empty task/cron arrays, and stop_hook_active=false. Unmatched completion events or unsupported events invalidate instrumentation until a fresh SessionStart.

The snippet deliberately adds no SessionEnd command: process/lifecycle observation already invalidates exited runs, and an exit hook waiting for PCTL's registry lock must not delay existing manual restart.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "PostToolUseFailure": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "SubagentStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ],
        "matcher": "permission_prompt"
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "[ -n \"$PCTL_RUN_ID\" ] && [ -n \"$PCTL_BIN\" ] && [ -n \"$PCTL_STATE\" ] && [ -n \"$PCTL_TMUX\" ] || exit 0; \"$PCTL_BIN\" --state \"$PCTL_STATE\" --tmux \"$PCTL_TMUX\" session hook --refresh-observe",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

After merging/installing, use the normal terminal for this computer. Inspect without delivering queued tells or injecting a test prompt:

```bash
pctl session refresh --idle --dry-run | jq '{dry_run, execution_supported, policy, projects, would_swap}'
```

For a normally completed real turn, check its `projects[].sessions[].readiness`: `instrumented=true`, `ready=true`, `last_hook="Stop"`, `background_tasks=0`, `session_crons=0`. Run ID/conversation must match the registered run. Stale/mismatched hooks and runs without a launch record are silently ignored; they cannot make another run ready or disturb a session still using the old rollout. Metadata parse/I/O failures remain visible. A session stays readiness_unknown until the required real launch/SessionStart/turn sequence exists. Do not fabricate hook payloads on a real session to get a green result. Unsupported builds/sessions remain readiness_unknown. This rollout does not require a model test prompt or a new conversation.

## Dry-run policy

Defaults (no config file is created):

```toml
[refresh]
enabled = false
app_observer = true
poll_interval_secs = 60
idle_for_secs = 1800
quit_timeout_secs = 10
ready_timeout_secs = 30
```

In PR-A, enabled/app_observer/quit_timeout_secs/ready_timeout_secs are reported policy for PR-B; they activate no execution path. Dry-run evaluates hypothetical candidates even with enabled=false so the operator can measure for a day without enabling automation. Invalid policy values fail visibly. poll_interval_secs must be positive; idle_for_secs=0 is supported for isolated fixtures, not the recommended rollout.

Use the updated PCTL app/bridge and control clients as well as the updated hook handler; older control clients do not record the new transient visibility/input epochs. Observations are cooperative and an inactive proposal must be rechecked before any later execution.

The operator's existing scheduler can invoke the bounded dry-run every 60 seconds and retain its JSON stdout for a day. This change adds/edits no scheduled job. The command itself persists `STATE/refresh-observe.json` under the existing registry lock, plus a nonblocking observation-sweep lock. No input reservation is activated. A first invocation begins observation; it cannot infer the previous 30 minutes. Gaps greater than twice the polling interval, clock reversal, changed cohort/run/input/queue/activity/protection/visibility reset the window. Build installation alone need not reset otherwise unchanged project idleness.

Every registered local project is considered. Stopped registrations do not count as active; any live working/blocked/unknown or unresolved exited/lost run holds the project. No tell/in-flight/unresolved delivery or pending mailbox work, app-visible agent cell (including unfocused splits), or raw tmux client may be present. A changed raw-client last-attached timestamp also resets observation, including brief attachments between sweeps. Protected sessions are never targets; a working protected sibling still holds the whole project. All live Claude sessions require readiness evidence. Other harnesses must be screen-idle but are never targets. No per-project allowlist or remote-host sweep is implemented.

CLI input, app/bridge keys/pastes, queue enqueue/delivery, mailbox sends, lifecycle changes, protection and viewer changes record epochs before the corresponding side effect. Conservative failed input attempts also reset the window. If observation persistence is corrupt, exhausted or unwritable, activity-recording operations fail visibly before sending rather than silently retaining an old ready record; the operator must inspect that state. Input epochs invalidate Stop records for the previous turn. Queued-tell observation is strict and metadata-only; it does not call the status-read queue delivery helper. Undelivered or uncertain deliveries hold the project; this release does not rewrite queued messages for a new run.

`would_swap[].reservation` is an **inactive proposal**, containing server/project/agent/run/conversation, target build, input/queue epochs and a project epoch bound to the stored cohort evidence. The cohort is stored once per project, not copied into every proposal. It is neither a reservation held against the person nor permission to stop later. The core's `session_refresh_proposal_valid` rechecks metadata, project membership, activity/queue epochs, readiness/visibility/protection, client attachment, build and the idle window; changed evidence invalidates a proposal. PR-B must revalidate under its actual handover/input gates immediately before any stop. No active reservation, queue handover, build quarantine or execution receipt is created by this command.

## Reading a day's results

Use `would_swap` to review hypothetical target sessions; `projects[].eligible`, `idle_seconds`, `required_idle_seconds` and per-project/per-session reasons explain exclusions. Existing queue files, registry/run IDs and processes must remain unchanged by the dry-run. Only observation bookkeeping changes. Do not enable PR-B until the operator has reviewed a full day's evidence and the owner approves it separately.

## Evidence limits

These are cooperative PCTL/hook observations, not an authentication boundary against another program running as the same user. A Stop event proves only the task fields reported by the instrumented Claude build; opaque detached work outside that registry is not proven absent. Missing/failed/unobserved hook events cannot be inferred from screen silence. Raw attached clients are excluded. No live settings, real server, live harness refresh or installed-build readiness schema was validated by the worker; those are operator rollout checks. PR-B's graceful exit, tell handover, failed-build quarantine and summaries are intentionally deferred.
