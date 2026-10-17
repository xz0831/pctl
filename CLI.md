# Role-name workflow: 0.7.0

`pctl msg` is a convenience layer over the same durable local mailbox. It does not add transport. The earlier 0.4 low-level `message ack-reply` remains separate and unchanged.

Launcher environment supplies default --state/--tmux. Explicit flags take precedence, but `msg` rejects flags differing from its PCTL_STATE/PCTL_TMUX context. Sender, project and current run are bound to PCTL_AGENT_ID/PCTL_PROJECT_ID/PCTL_RUN_ID; there is no sender flag. These values are caller-claimed context, not authentication or credentials.

```sh
"$PCTL_BIN" msg peers
"$PCTL_BIN" msg send reviewer --key review-1 --text 'Review the authorized synthetic result'
"$PCTL_BIN" msg inbox --unacked --wait-seconds 30
"$PCTL_BIN" msg consume m1
"$PCTL_BIN" msg ack m1
"$PCTL_BIN" msg reply m1 --key answer-1 --text 'The result'
"$PCTL_BIN" msg complete m1 --summary 'Self-reported result' --artifact results/check.json
"$PCTL_BIN" msg status m1
```

Replace m1 with the actual received handle. Handles are immutable project-wide message sequences, visible only to this current sender/recipient run. They are not transferable authority. No `last`/`latest` selection, default peer, fuzzy name or implicit newest reply exists. Exact role means agent.name, case/whitespace sensitive, unique among ALL profiles in the caller's project, including stopped/disabled profiles. Another project's matching name is excluded. Resolution, run checks and storage occur under the same registry lock. A first send resolves the current run at that operation; discovery is not a reserved target. Explicit reply always reverses the original exact agent/run pair.

Every send/reply requires a 1–64 byte ASCII key (letters/digits/hyphen/underscore/dot). Its scope is sender agent/run across send and reply. The key, initial role text, UUID and immutable target/run binding are persisted together in the mailbox, with no separate intent window. Identical retry deduplicates even after receipts change; changed role, operation, reference, kind, subject or body conflicts. Retry checks original live target/run, never re-resolves a renamed/restarted role. After an uncertain result, use status/inbox/operator history; do not generate a new key to conceal uncertainty.

Send defaults to kind=handoff, subject=handoff. Reply defaults to kind=result, subject=reply; result/update/question are allowed. Both accept optional --subject, --kind and either --text or --body-file (8192 UTF-8 bytes maximum). No text is executed; no native terminal receives mailbox contents. Text arguments may appear in local process listings; do not include secrets.

Inbox is a read-only snapshot, default limit20, maximum100, --after-sequence for pagination, optional --unacked receipt filter and bounded --wait-seconds 0..120. It records no receipts. Consume explicitly records the first consume-request timestamp, then returns the body: it does not prove the client received stdout, model comprehension or acceptance. Null retrieval does not mean unread. Ack records independent receipt only. Both are exact-recipient/run bound and idempotent. Completion applies only to a received handoff and records a maximum512-byte printable summary plus optional relative artifact reference (512 bytes, no root/current/parent components). The artifact is never read/opened/executed or checked for existence. Identical completion repeats keep the original timestamp; a changed claim conflicts rather than overwriting evidence.

`status` returns separate stored_at, retrieval, acknowledged_at, completion and verified_result:null, with immutable IDs/run bindings for auditing. Completion is a recipient self-report, not an approval or verified result. No agent-wide report is inferred from it; observed generic task remains unknown. Inbox and Portal inbox inspection show separate evidence without marking anything as consumed. Displayed role labels are current registry labels; status bindings are immutable.

Added error codes: context_missing, context_mismatch, ambiguous_role, capability_denied, ownership_denied, launch_limit, orchestration_capacity and operator_only. Existing errors/guards still apply. Existing mailbox schema1 records without optional metadata remain readable; do not run an older binary against 0.6 state because an old write may discard new evidence. Use a separate state for this prototype. The16MiB mailbox cap reserves future retrieval/receipt/completion growth; refusal may occur before1024 messages. No implicit pruning or migration exists.

Unsupported: automatic PTY/prompt delivery, remote computer routes, A2A, external messages, broadcast, network credentials, authenticated per-process actors and harness approval. Explicit local commands do not expand the user's authorization.

---

# Appearance additions in 0.5

Global `--theme portal|classic|mono` saves and applies a preset to this state directory. `theme [NAME]` changes or inspects it and returns JSON with `theme` and `config`. These commands do not start or send input to agents. The running pctl UIs refresh on the normal observation cadence. The app’s Theme button chooses presets and `? Help` opens its shortcuts.

# 0.6 bounded native agent orchestration

`pctl team` lets a running project controller launch preconfigured helper profiles, select their native panes, arrange a split view, stage local drafts, interrupt, stop and restart exact owned child runs. Task dispatch uses the separate `msg` mailbox. The implementation does not interpret mailbox text as shell commands and does not scrape/re-render native agent terminals.

This is a cooperative same-Unix-user pilot. Configuration expresses an operator's selected capabilities; it does not authenticate a person, replace the user's authorization, sandbox a model, or prevent a same-user process from invoking older operator commands, tmux, or the filesystem directly. Agents must not unset launcher context, edit state, or use another interface to bypass a refusal.

## Operator setup

Register the controller and helper profiles normally. Every target must have a unique exact name within its project and an existing canonical cwd inside the registered project root. Role-specific directories such as `/project/reviewer` and `/project/implementer` can be used. The registered argv array, cwd, name, adapter, input mode and mailbox mode are frozen in each grant. No `team launch` argument can replace them.

Start the controller explicitly and retain its returned run ID. Then, from the operator's shell without `PCTL_AGENT_ID` or `PCTL_RUN_ID`, grant only the needed actions:

```sh
pctl --state STATE --tmux TMUX team grant \
  --controller MAIN_AGENT_ID --controller-run EXACT_MAIN_RUN_ID \
  --target REVIEWER_AGENT_ID --target IMPLEMENTER_AGENT_ID \
  --allow launch --allow focus --allow pair --allow unpair --allow stop \
  --max-live 2 --max-launches 6 --ttl-seconds 3600
```

For an automated test, a preconfigured wrapper may hold the model behind an operator-controlled startup gate until the grant is installed. The grant itself never changes or injects that wrapper's command.

Capabilities are individually named: `launch`, `focus`, `pair`, `unpair`, `zoom`, `stage`, `interrupt`, `stop`, `restart`. Granting `restart` explicitly permits replacement of an already owned child; it does not permit launching arbitrary additional profiles. Nothing grants `paste`, `submit`, approval, user-client detach, UI close, shell registration, or cross-project controls.

Grant limits are `max-live` 1–16, `max-launches` 1–64 and TTL 1–86400 seconds. The launch count belongs to the exact controller run and does not reset on replacement of its grant. Exited but still registered child panes occupy a slot until explicitly stopped. Unresolved launch attempts also occupy a slot and consume a launch attempt.

```sh
pctl --state STATE --tmux TMUX team policy
pctl --state STATE --tmux TMUX team revoke --grant EXACT_GRANT_ID
```

A replacement requires `team grant ... --replace-grant EXACT_LATEST_GRANT_ID`; it is a new reviewed configuration, not an automatic refresh. Revocation and expiry remove mutation rights immediately without killing already running children. Operator inspection remains available. Grant configuration commands reject launcher context before creating or changing any selected state.

## Agent workflow

Use the exact launcher environment. The CLI refuses missing or mismatched state/tmux/project context before changing the selected state. It refuses global `--theme` on context-bound commands. The environment is ordinary local context, not a secret credential.

```sh
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team profiles
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team launch reviewer --key review-one
```

`result.child` is a short handle such as `c1`. It records an immutable controller agent/run, target agent/run, server instance, pane, PID and process-start generation. Copy the returned handle; never guess one. Keep the original key for retries. Keys allow 1–64 ASCII letters, digits, hyphens, underscores and dots.

```sh
# Join the existing child beside the exact live controller.
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team pair c1 --with self
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team focus c1
# A separate authorized handoff is durable mailbox data, not terminal input.
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" msg send reviewer --key review-task-one --text 'Review the authorized fixture output.'
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team unpair c1
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team children
```

A pair operation can use another owned child (`--with c2`) or the explicit `self` anchor. It never accepts a foreign child or arbitrary native pane. Pairing joins existing native panes; unpairing moves one existing pane into its own window. Neither launches a process. Focus can switch the sole attached client's view. The pilot retains its one-interactive-client limit.

`msg` send is authorized separately by the user's task and each profile's mailbox capability. The body is inert local data, not permission. A recipient must explicitly read it and remains responsible for checking scope. A task message, acknowledgment, completion claim or file reference never expands the orchestration grant.

## Keep consequential actions separate

```sh
# Local draft storage only; nothing reaches the child's terminal.
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team stage c1 --text 'A draft for later operator review'
# Literal Ctrl-C; not a guaranteed task cancel or SIGINT.
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team interrupt c1
# Remove the exact child pane and close its PTY.
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team stop c1
```

Stopping a child is different from leaving the app, closing an app cell, or detaching a client (`Ctrl-b d`), which leave worker processes running. External/daemonized descendants can survive a pane stop. There is deliberately no ambiguous `team close` action.

`team stage` accepts at most 8192 bytes and uses the existing run-bound local draft store. It never transmits, sends Enter, or schedules a later commit. Generic terminal paste and Enter remain separate operator-native interfaces with their original input-mode and authorization requirements. The team facade has neither action, and has no approval shortcut.

If restart is explicitly granted:

```sh
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" team restart c1 --key reviewer-restart-one
```

The response supplies a new child handle and run. Old handles never retarget. A duplicate exact launch/restart key returns only the original binding, even if it is now historical; it never silently launches again. Reusing a key for another profile, operation or original child is rejected. A controller restart inherits neither its old grants nor its old children. Existing children remain operator-visible orphans; they are not silently adopted.

## Failure and evidence contract

- Caller authorization, granted profile, ownership and core run guards are checked under one registry lock. Existing pane/server/PID-start checks are reused rather than replaced.
- A durable launch/restart attempt and budget debit are written before native process mutation. Pending or uncertain attempts are retained. Do not clear them, change keys, or create a replacement target to conceal uncertainty.
- Same-user external tmux changes can still create check/use races; this is not a security boundary.
- Profile or canonical workdir changes invalidate the frozen grant. Restarted or independently respawned/moved child processes fail exact identity checks.
- On `outcome_unknown`, inspect `team children` and ask the operator to reconcile. Do not automatically replay a zoom toggle, Ctrl-C, stop, or restart. Launch/restart replay with the retained key can only report the original binding or unresolved intent.
- Store limits: 128 grants, 1024 launch attempts, 2 MiB serialized data, with reserved growth for pending run bindings and revocation timestamps. Full storage is rejected rather than silently purged.
- `team children` is read-only historical inspection and survives expired/revoked grants and controller exit. Actual mutations require an active grant and exact live controller.
- Generic task status remains `unknown`. Process existence, a successful launch return, mailbox storage and model self-reports are not verified task completion.


---

# CLI contract: 0.4.0

All examples require the same explicit `--state STATE --tmux TMUX` prefix. `STATE/tmux.sock` is the only server used; the ordinary default tmux server is never targeted.

## Discovery and configuration

```sh
pctl --state STATE --tmux TMUX doctor
pctl --state STATE --tmux TMUX project list
pctl --state STATE --tmux TMUX project add --name NAME --root /existing/root
pctl --state STATE --tmux TMUX project rename --project PROJECT --name NEW_NAME
pctl --state STATE --tmux TMUX agent add --project PROJECT --name NAME --cwd /existing/cwd --adapter generic -- /absolute/program arg1 'arg two'
pctl --state STATE --tmux TMUX agent edit --agent AGENT --name NAME --cwd /existing/cwd --adapter generic -- /absolute/program arg1
pctl --state STATE --tmux TMUX agent configure-input --agent AGENT --mode terminal-line
```

Registering never starts. Editing and input-mode changes reject current runs, including exited panes, and unresolved launch intents. Explicitly terminate the old run first. Names are labels; duplicate names are allowed, while IDs remain distinct. Legacy profiles deserialize with native input disabled for remote typing.

## Lifecycle and native layout

```sh
pctl --state STATE --tmux TMUX agent list
pctl --state STATE --tmux TMUX agent inspect --agent AGENT
pctl --state STATE --tmux TMUX agent start --agent AGENT
pctl --state STATE --tmux TMUX agent focus --agent AGENT --expected-run RUN
pctl --state STATE --tmux TMUX layout pair --agent A --expected-run RA --with-agent B --with-run RB
pctl --state STATE --tmux TMUX layout split --agent A --expected-run RA
pctl --state STATE --tmux TMUX layout zoom --agent A --expected-run RA
pctl --state STATE --tmux TMUX agent interrupt --agent A --expected-run RA
pctl --state STATE --tmux TMUX agent terminate --agent A --expected-run RA
pctl --state STATE --tmux TMUX agent restart --agent A --expected-run RA
pctl --state STATE --tmux TMUX client list
pctl --state STATE --tmux TMUX client detach --client OPAQUE_TOKEN
pctl --state STATE --tmux TMUX attach --project PROJECT
```

`layout split` separates an already joined pane into its own window; it does not create a second agent. `pair` uses two distinct same-project agents. `focus` supports one attached interactive client. `attach` is an interactive terminal command, so its normal output is tmux rather than JSON. `worker` is a test fixture rather than a JSON command.

## Explicit input

```sh
pctl --state STATE --tmux TMUX input draft --agent A --expected-run R --from-file draft.txt
pctl --state STATE --tmux TMUX input read-draft --agent A --expected-run R
pctl --state STATE --tmux TMUX input paste --agent A --expected-run R --text 'one printable line'
pctl --state STATE --tmux TMUX input submit --agent A --expected-run R
```

Draft JSON is `{"ok":true,"text":"..."}` or `text:null` when none exists for that run. Paste and Enter are separate transmissions, never an automatic pair. Wait for visible native-editor readiness before Enter: OpenCode mini was observed to coalesce an immediate paste+Enter, leaving text unsent. Inspect before any further action; do not repeat the paste to recover uncertain acceptance. Native input mode rejects both. The profile capability is not authorization for a particular communication or approval. Synthetic `input send-fixture --text TEXT` appends a newline and only accepts the verified bundled worker.

## Success and observation shape

For compatibility, project/agent add and agent edit return the object directly. Edit also returns `conversation` (null when absent) and `conversation_cleared`: switching the detected harness clears the previous harness's saved ID, while name, directory, flags and env-wrapper edits within the same harness preserve it. Starting a freshly changed harness still follows `session start --new` when its conversation is unknown. Lists return either arrays (`project list`, `client list`) or a snapshot (`agent list`). Start/restart return a run object. Control results typically return `{"ok":true,"result":"..."}`. `layout` and some simple changes return `{"ok":true}`. There is no universal success envelope or generated request ID in this build.

`agent inspect` returns:

```json
{
  "agent": {"id":"...","project_id":"...","name":"...","cwd":"...","argv":["..."],"adapter":"generic","input_mode":"native"},
  "run": {"id":"...","agent_id":"...","project_id":"...","server_instance":"...","session":"$0","pane":"%1","window":"@1","pid":"123","process_start":"456","started_at":1234567890},
  "process":"running",
  "task":"unknown",
  "evidence":"observed:tmux",
  "observed_at":1234567890,
  "guard_error":null
}
```

A never-started or explicitly terminated agent has `run:null`, `process:not_started`. Other process observations include `starting`, `exited(N)`, `missing`, and `unknown`. A disconnected/replaced server gives snapshot `connected:false`; do not reuse previous live/task state. Synthetic task evidence expires after ten seconds and is identified as stale/invalid; generic task state never becomes completed from exit status.

## Runtime errors

Runtime errors use stderr and exit status 2:

```json
{"schema_version":1,"ok":false,"error":"stale_run: expected-run does not match current run","error_code":"stale_run"}
```

Stable `error_code` categories:

- `stale_run`: captured run no longer matches
- `outcome_unknown`: external result may be partial/ambiguous; inspect, never replay input automatically
- `unsupported`: capability unavailable
- `pane_in_mode`: copy/other mode blocks input
- `synchronized_input`: synchronized panes block input/layout
- `target_unverified`: server/process/pane identity or project membership failed
- `state_conflict`: existing/current run or unresolved launch intent
- `not_found`: registered target missing
- `operation_failed`: other validation, filesystem or runtime failure; use readable detail

Argument syntax/help/version comes from Clap as ordinary text, not this runtime envelope. Machine callers must check exit status and handle non-JSON parser errors. Each tmux child has a three-second deadline; each registry-lock acquisition has a two-second deadline. Timeouts of mutating commands require reconciliation. No operation should be treated as task completion merely because its control response says `ok:true`.

## Explicit same-project mailbox

Both profiles must opt into `mailbox` while stopped. This is a capability switch, not permission to contact any agent or perform a message's instructions.

```sh
pctl --state STATE --tmux TMUX agent configure-collaboration --agent A --mode mailbox
pctl --state STATE --tmux TMUX agent identity
pctl --state STATE --tmux TMUX peer list --agent A --expected-run RA
pctl --state STATE --tmux TMUX message send --from-agent A --expected-run RA --to-agent B --to-run RB --kind handoff --subject 'Bounded task' --body-file task.txt --request-id REQUEST_UUID
pctl --state STATE --tmux TMUX message inbox --agent B --expected-run RB --unacked --after-sequence 0 --limit 50 --wait-seconds 30
pctl --state STATE --tmux TMUX message ack --agent B --expected-run RB --message MESSAGE_UUID
pctl --state STATE --tmux TMUX message send --from-agent B --expected-run RB --to-agent A --to-run RA --kind result --subject 'Synthetic result' --body-file result.txt --request-id NEW_UUID --in-reply-to MESSAGE_UUID
pctl --state STATE --tmux TMUX agent report --agent B --expected-run RB --status completed --summary 'Self-reported synthetic result' --ttl-seconds 60
pctl --state STATE --tmux TMUX skill install --project PROJECT
```

`agent identity` uses the PCTL_AGENT_ID and PCTL_RUN_ID environment exported on new launches, then validates them against the explicitly selected state. This is caller-claimed context, not authentication. `peer list` returns `{project_id,self_agent_id,self_run_id,peers,identity_trust}`, excluding the caller and other projects. A peer may exist without an active run or with collaboration disabled; that is not a valid send target.

Send returns `{message,deduplicated,delivery}`. The caller-generated UUID request ID becomes `message.id`; reuse it only for the identical sender, recipient, run pair, kind, subject, body and reply reference. A new conversation uses that ID as its correlation ID. A reply retains the original correlation ID and must reverse the original exact sender/recipient run pair. Valid kinds are handoff, result, update, question. Body files must be nonempty UTF-8, at most 8192 bytes, with ordinary newlines/tabs allowed but other control characters rejected. Subjects are printable, nonblank, at most 256 bytes.

Inbox returns `{agent_id,run_id,messages,next_after_sequence,high_watermark,semantics,waited_ms,timed_out}`. `messages` are ordered by project-wide monotonic sequence. Use `next_after_sequence` if non-null. Limit is 1–100; default 50. Reading does not mark acknowledged. `--unacked` filters receipts, not unfinished work. Wait is 0–120 seconds; default 0. It polls local files every 250 ms without a registry lock; timeout returns an empty array and timed_out=true, not an error or task result. Do not infer delivery failure or resend from an empty read.

Message fields include id, sequence, project_id, from_agent_id, from_run_id, to_agent_id, to_run_id, kind, subject, body, correlation_id, in_reply_to, created_at, acked_at and sender_trust. Acknowledgment is idempotent, exact-recipient/run-bound, and means receipt only. No CLI mailbox operation writes to any PTY. Caller-claimed sender IDs and message text do not authorize work. No user-level authentication, broadcast, cross-project messaging, automatic forwarding, prompt injection, background delivery daemon, or automatic approval is implemented.

The project store retains at most 1024 messages, including acknowledged history, and fails closed at capacity. There is no automatic pruning or old-run migration. A restart invalidates old targeting and hides old-run messages from the new run's inbox while retaining them on disk. Do not edit state to bypass stale-run or capacity guards.

Additional runtime errors: `cross_project`, `idempotency_conflict`, and `mailbox_full`; existing `stale_run`, `unsupported`, `target_unverified`, `state_conflict` and other errors still apply. A valid run-generation check is not sender-process authentication. On an uncertain filesystem result, inspect state with the same request ID before considering a retry; never generate a different ID to hide uncertainty.

## Self-reported status and project-local skill installation

`agent report` accepts working, waiting, blocked, completed, unknown; printable summary up to 1024 bytes; TTL 1–300 seconds (default 60). `agent inspect`/list add `pending_messages` and `report`. The latter is null or `{report:{agent_id,run_id,project_id,status,summary,reported_at,expires_at,evidence},fresh}`. Generic observed `task` remains unknown even when a fresh self-report says completed. Stale, disconnected or unverified runtime reports are not shown as fresh. `pending_messages` is the current run's unacknowledged-message count, not pending task count.

`skill install` writes the binary's embedded SKILL.md and CLI.md plus local RUNTIME.md into PROJECT_ROOT/.opencode/skills/pctl-prototype. It is scoped to the registered project root, rejects symlink directories/files and differing existing files, and is idempotent for identical content. It does not change global OpenCode settings, permission rules, profiles, or running agents. Discovery and actual model use are distinct acceptance steps. The bundled command contract does not grant authorization.


Audit refinements: serialized mailbox storage is also capped at 16 MiB, reserving space for future receipt timestamps so valid heavily escaped bodies cannot make the store unreadable. Capacity can be reached before 1024 messages. The installer rejects control characters in runtime paths and encodes path display fields. Static symlinks are refused; concurrent ancestor replacement by the same Unix user remains unsupported. Identical send retries still require the original live run pair; after exit/restart they return a stale/unavailable-target error rather than recovering an old receipt. Retain the original request ID and use preserved history/operator review; never retarget or generate a new ID to hide uncertain delivery.



## Explicit unique-reply receipt convenience (0.4)

```sh
pctl --state STATE --tmux TMUX message ack-reply --agent A --expected-run RA --reply-to OWN_OUTGOING_MESSAGE_UUID
```

Use this only when you intend to acknowledge receipt of the unique existing direct reply to a message you sent. The supplied ID is deliberately your outgoing ID. The core checks that it belongs to this exact caller/run, then resolves a stored reply with matching correlation and the exact reverse sender/recipient agent/run pair. It returns the actual received message/body and its receipt timestamp. This grants no work permission and does not claim task completion.

Zero matches returns not-found. Multiple direct replies returns ambiguous_reply, including when an older matching reply is already acknowledged. It never chooses the newest silently. Read inbox and make an explicit message-ID decision when ambiguous. Repeating a successful operation against a still-unique reply returns the same receipt; a later second reply makes it ambiguous rather than advancing to new mail. There is no wait, subscription, bulk acknowledgment, or future-mail acknowledgment. General message ack is unchanged and still requires the actual received message.id, never an outgoing ID or correlation_id.

This convenience addresses an observed small-model error in the frozen0.3 test. It does not retroactively turn that failed attempt into success. Its own0.4 contract tests and any later actual harness use are separately recorded.

## Optional web page (`pctl web`)

`pctl web --listen <IP:PORT> --token-file <file>` serves a small phone page: the session list, one session's screen (refreshed every 3 seconds while open), and a line of input. It runs only while started and never starts the tmux server.

- `--listen` accepts loopback or a Tailscale address only (100.64.0.0/10, fd7a:115c:a1e0::/48). `0.0.0.0`, `::`, LAN and public addresses are refused. Port 0 picks a free port. Once bound it prints `{"listening":"<addr>"}`.
- The token file must be a regular file with no group or other access (0600) holding at least 32 visible ASCII characters. The token is never printed or logged.
- `GET /`, `/app.js` and `/app.css` carry no data. The API is `POST` with a JSON body and `Authorization: Bearer <token>`. A wrong or missing token gets 401 after one second.
  - `/api/sessions` → `[{name,project,harness,status,branch}]` (`branch` is null outside a worktree)
  - `/api/screen` `{name, lines}` (default 60, 1–500) → `{name,status,text}`
  - `/api/prompt` `{name, text}` → the `session prompt` result. A blocked session is refused with 409; other errors are 400 with `error` and `error_code`.
- The page never switches sessions by itself. The log line per request is time, method, path and status only.
- HTTP is plain: use it over Tailscale (WireGuard) or loopback only.

## Human entry: the pctl app (`pctl` alone)

`pctl` with no command, on an interactive terminal, opens the pctl app. It is a full-screen TUI and not a tmux client. Without a terminal on stdin and stdout, `pctl` alone is the usual missing-command usage error (exit 2), so agents and scripts see no change. A launched agent (`PCTL_AGENT_ID` or `PCTL_RUN_ID` set) or anything inside that state's tmux, such as a bash tab of the app itself, is refused with `agent_caller` (exit 1). Its keys would skip the blocked-question refusals of `msg` and `session prompt`. The app sizes the agent's window to its terminal area. With no client attached it unsets `window-size` right away, so nothing stays `manual` however the app ends. With a client attached (`pctl attach`) the size holds while the app shows the window, and leaving hands it back. A pane taller than the app (sized by another viewer) shows its bottom rows, where the input line and dialogs are. "Start a new conversation…" asks first, and Stop acts on the run that was shown when it asked.

Screen:
- **Sidebar:** projects and their sessions with states.
- **Tab bar:** the current project's sessions, then `+`.
- **Terminal:** tmux's own screen of the selected session, read with `capture-pane -e` about 8 times a second and redrawn. Its window is resized to the view. Keys go back with `send-keys` (text with `-l`, other keys by tmux name); a paste goes through a bracketed `paste-buffer`.
- **Status line:** state, the number of sessions waiting for a person, notices, `? Help` and `⏏ Leave`.

Mouse (SGR mouse reporting):
- Click opens a project, session or tab.
- `+` opens a bash tab right away. It is a `session add` named `<project>-bash<n>` in the project folder, running bash with `-l`. bash is `$SHELL` when that is bash, else `/opt/homebrew/bin/bash`, `/usr/local/bin/bash` or `/bin/bash`.
- `+ agent` opens the new-agent form:
  - name and folder
  - kind: bash, Claude Code, Codex, OpenCode or Command
  - optional model: `--model` for Claude Code, `-m` for Codex, pctl's `--model` for OpenCode
  - command (for Command)
  - optional worktree branch
- `[+]` adds a project.
- Right-click on a session (or double-click it, or `ctrl+b m` for the one on screen, for terminals such as MATE Terminal that keep the right button) offers: restart in the same conversation, stop (asks first), start, start a new conversation, remove (asks first). Right-click on a project offers a new bash tab or a new agent.
- An open dialog takes every click; nothing behind it reacts.
- The wheel over the terminal scrolls back (`capture-pane -S/-E`). Any key returns to live.
- Dragging the sidebar edge resizes it.
- The sidebar scrolls when its rows do not fit: the wheel over it moves two rows a notch, the keyboard selection (`ctrl+b w`, arrows) and an agent that comes on screen by any route are kept in view, and a muted `▲ N more` / `▼ N more` line at the top or bottom shows what is hidden (a click on it moves a page).
- The terminal area can be split into cells (a tree of splits, at most 16 cells). Each cell has a one-row title `<computer> · <agent> <state>` (the computer only when machines are saved), its own screen, scroll and read schedule, and its agent's tmux window is resized to the cell. An agent is shown in at most one cell. Typing, paste and the shortcuts act on the focused cell; a click focuses a cell, a drag selects inside one cell, the wheel acts on the cell under the pointer. Dragging a divider resizes; a divider's line is a column between side-by-side cells, or the title row of the lower cell. With one cell nothing is drawn but the terminal itself, as before. These are views only: agents keep one tmux window each, and closing a cell stops nothing.

Shortcuts:
- Defaults are a prefix (`ctrl+b`) and a key: `c` new bash tab, `a` new agent, `shift+c` new project, `n`/`p` tabs, `1`–`9` tab N, `x` stop, `r` restart, `b` sidebar, `w` keyboard sidebar, `?` help, `q` leave. Prefix twice sends the prefix to the session.
  - Cells: `%` split right, `"` split down, `&` close the cell (never the agent), arrows move the focus, `alt`+arrows move a divider by 5 %, `{`/`}` swap with the previous/next cell, `z` zoom.
- Override them in `$XDG_CONFIG_HOME/pctl/config.toml` or `~/.config/pctl/config.toml`, under `[keys]`, with `name = "key"`.
  - Names: `prefix`, `new_tab`, `new_agent`, `new_project`, `next_tab`, `prev_tab`, `stop_agent`, `restart_agent`, `toggle_sidebar`, `focus_sidebar`, `help`, `detach`, `split_right`, `split_down`, `close_cell`, `focus_left`, `focus_right`, `focus_up`, `focus_down`, `resize_left`, `resize_right`, `resize_up`, `resize_down`, `swap_prev`, `swap_next`, `zoom`, `image_paste`.
  - Keys look like `ctrl+b`, `alt+right`, `shift+c`, `f2` or `?`; a `prefix+` in front means after the prefix.
- Unknown names and bad keys are reported in the status line, and the default is kept. An empty value (`image_paste = ""`) turns a shortcut off.
- `image_paste` (default `ctrl+v`): with an agent of another computer on screen, an image on this computer's clipboard is sent to that computer's clipboard (PNG, at most 20 MB), then `ctrl+v` goes to the agent, so Claude Code there pastes it. With no image, or for an agent on this computer, it is an ordinary `ctrl+v`. Text needs no key: pasting (`⌘V`) already reaches agents on other computers. It reads the clipboard with `osascript` (macOS), `wl-paste` or `xclip`; the other computer sets it with the same tools.

Themes:
- Built-in: `portal`, `garage`, `nebula`, `void`, `lab`, `classic`, `mono`. `pctl theme <name>` sets one per state (`appearance.json`); the app's `◐ Theme` button and `ctrl+b t` do the same.
- Each theme also colours the terminal area: the default background and text, and the 16 ANSI colours. 256-colour and RGB output is left as the agent sent it. `classic` and `mono` keep the terminal's own colours.
- Your own themes are files `$XDG_CONFIG_HOME/pctl/themes/<name>.toml` or `~/.config/pctl/themes/<name>.toml`.
  - Names use a-z, 0-9, `-` and `_`, and must differ from the built-in themes.
  - Each file is at most 16 KiB.
  - The format is flat `key = "value"` lines:
    - `base` (a built-in theme to start from)
    - `about`
    - `bg`, `panel`, `raised`, `text`, `muted`, `border`, `accent`, `cyan`, `warn`, `danger`, `ink`, `term_bg`, `term_fg` as `#rrggbb`, or `default` for the terminal's own colour
    - `ansi = [16 colours]`
  - Missing keys come from `base`. Problems are reported and the base colour is kept.
- `pctl theme --new NAME [--from BASE]` writes a filled template and refuses to overwrite. `pctl theme --list` prints the built-in and your own themes, the folder, and any warnings.
- A theme of your own applies to the app and is remembered in `app-view.json`. The tmux chrome of a plain `attach` keeps the state's built-in theme.
- `[ui]` with `wit = false` in `config.toml` replaces the jokes with plain text (loading, empty screens, the stop question's last clause, tips, goodbye).

Other behaviour:
- The last project, its tabs and each project's cells (with the computer of each agent) are kept in `<state>/app-view.json`. A file from before cells loads as one cell per project.
- Leaving the app stops nothing.
- The app never starts a server. Adding a session goes through `session add`, which keeps its own server-start rule.

`pctl attach [--project <name|id>]` is advanced raw tmux access to a verified registered agent pane. It keeps the active eligible agent window, falling back to the first eligible agent window. Without a project it chooses the most recently used eligible project. It never starts a server, creates a placeholder or opens a control panel. Empty projects refuse with guidance to open the app and start an agent. Inside the same server it switches the client; inside another tmux it refuses nesting.

Raw clients keep static project/agent labels, the built-in theme, `ctrl+b s` (tree) and `ctrl+b q` (detach). There is no background `session marks` job or native manager shortcut. The app’s separate `ctrl+b m` menu and built-in sidebar remain.

New sessions start with their first gated agent window; the owned server stays alive when its last session is removed. Upgrading never kills existing manager/sidebar panes, rewrites registries or restarts agents. On a verified owned server, app/start/attach retire only the exact old manager binding and generated status jobs for that state. Custom bindings/jobs remain. Legacy windows, sidebar metadata and inert decorations may remain until the server is next restarted or a person closes them. Native unpair excludes a legacy navigation pane only when its saved generation, server, pane tags and live process identity all match; unknown panes are ordinary panes. A replaced server is never adopted. `pctl demo` remains a synthetic-worker fixture usable from the app.

## Codex conversation recovery

Codex storage is `$CODEX_HOME` when set, otherwise `$HOME/.codex`. Automatic first-turn discovery reads `sessions/` for one recent rollout with matching cwd, a valid `session_meta` header and a filename ending in that header's ID. Only a live registered run is eligible; IDs assigned to other profiles and ambiguous candidates remain unknown. A successful pick writes `codex_conversation` to `audit.jsonl`, with run ID and a JSON result containing source, rollout path, conversation ID, cwd and run start time. Hook-supplied Codex IDs also need a matching saved rollout and cwd, and record their source there. These are cwd/time and file checks, not proof of which process wrote the rollout.

Before a saved or explicit Codex resume, `session add`, `start`, `restart` and `restore` look for the ID in both `sessions/` and `archived_sessions/`. A missing or invalid rollout returns `conversation_unknown` with the searched root and recovery guidance. Restart checks before stopping the old run. Restore the file or correct HOME/CODEX_HOME; `session start/restart --new` clears PCTL's saved conversation ID when a fresh conversation is intended. Explicit resume arguments in a profile still select their own conversation.

Session add/start/restart/restore wait 500 ms after releasing the launch gate, then check the registered process. An early exit is `start_failed`, including exit status and the last 12 screen lines; its dead pane stays registered for inspection or restart. Survival of this short check is not a guarantee of later health or task completion. Lower-level `agent start/restart` keep their immediate launch semantics.

## Telling another agent (`pctl tell`)

`pctl tell <to> <text> [--wait] [--context KEY] [--plain]` is one command for agent-to-agent messages. It is not the same-project mailbox (`pctl msg`, `pctl message`, `pctl peer`), which stays as it is.

Where `<to>` goes:
- `<name>`: a pctl session on this computer. The text is typed with the sender line in front (`session prompt`).
  - `blocked` (waiting for a human decision): refused, exit 6, nothing typed.
  - `stopped`, `lost` or `exited`, and unknown names: exit 5.
  - Otherwise the result is `queued` (it was working when sent), `reached` (it became `working` or `blocked` within 8 s) or `typed_unconfirmed` (neither). All three exit 0; sending again would duplicate.
  - `--wait` waits (up to 30 minutes) until the session stops working, then returns its last 40 lines as `reply`.
- `<machine>:<name>`: a session on another computer. `<machine>` is a saved label (`pctl machine`) or an ssh target. It runs `pctl tell <name> <text> --from-line "<sender line>"` there over ssh (`PCTL_REMOTE_BIN`, like `--host`), so the sender is this computer's sender, not "ssh". Results and exit codes come back unchanged; ssh itself failing is exit 4. `--from-line` is accepted only in an ssh login (`SSH_CONNECTION` set).
- `<name>` listed in `[tell.a2a.<name>]`: an A2A agent, through `curl`. Without `--wait`, `SendStreamingMessage` with `A2A-Version: 1.0`; only the first event is read, then the connection is dropped (`TASK_STATE_SUBMITTED` is `accepted`). With `--wait`, `SendMessage`, and the reply text is printed (up to 330 s).
  - The conversation key is `--context` (sanitized to `[A-Za-z0-9_-]`), else `msg-<host>-<sender>-<YYYYMMDD>`: one conversation per sender per day.
  - An "Anti-loop protection" refusal is retried once with `<key>-r2`, `-r3` and so on. The newest key is remembered in `<state>/tell/ctx-<name>-<key>` (mode 0600).
  - The token is read from `token_file` (a file with `KEY=value` lines, the last one wins) when the message is sent and goes to `curl` on its standard input (`-H @-`). It is in no argument list, log or output.
- `codex:<prefix>`: the session whose conversation starts with the prefix (at least 4 characters). It is the reply address for a sender that runs outside any pane.

The sender line is `[from <me> <host> <cwd>]` (`$PWD` with `$HOME` as `~`). `<me>`, first match wins: the caller's own session (`PCTL_AGENT_ID`, as `session whoami`); the session holding `CODEX_THREAD_ID`'s conversation, else `codex:<first 8>`; a `door_services` value of `XPC_SERVICE_NAME` (the host name); any other `XPC_SERVICE_NAME` as `job:<name>` (without `job_prefix`); a caller under a `door_parents` job (the host name); `ssh` in an ssh login; else `?`. An A2A message without `--wait` gets ` · reply with tell` inside the brackets (`[tell] reply_hint` replaces the words). Telling yourself (`<me>` or `<host>:<me>`) prints and sends nothing, exit 0.

Output is JSON `{to, result, context?, reply?, queue?, note?|error?}` or, with `--plain`, one line (`reached: <to> …`, then the reply). Results: `reached`, `queued`, `typed_unconfirmed`, `accepted`, `replied`, `self`, `refused`, `no_token`, `no_connection`, `not_sent`, `blocked`, `queued_blocked`. Exit codes: 0 sent, 1 refused, 2 usage, 3 no token, 4 no connection, 5 could not send, 6 blocked. **A session that waits for a human decision (`blocked`)** is never typed into. The message is kept in `<state>/tell/queue/<session>/` (folder 0700, one file per message 0600; the text is in these files only) and `tell` answers `queued_blocked` with `queue` = how many are kept, exit 0. There is no daemon: the kept messages are typed in, in order and once each, through the same checks as `session prompt` (same run, not blocked), by whichever of these reads the session while it is `idle`: `session list` (the app polls it, so also on other computers through the bridge, and from any agent that lists sessions), `session get`, `session wait`, or the next `pctl tell` to it (which sends the kept ones first, so the order holds). The status such a read returns is the one read before it typed, so it can be one read behind. With nothing reading or telling, the messages wait; `session get` shows `tell_queue` (and `tell_dropped`) and the app shows `✉N` in the sidebar and `N kept for it` in the status line. A message kept for a run that was restarted is not typed into the new run: it is moved to `<state>/tell/dropped/<session>/` with the reason, and so is one whose sending was cut short (it may or may not have arrived); `tell_dropped` counts them. `tell.log` records `delivered` and `dropped` (as sender `queue`).

`<state>/tell.log` (mode 0600) gets one tab-separated line per send: time, sender, receiver, result, length of the text (never the text).

Configuration in `config.toml` (the small `[section]` / `name = "value"` reader the shortcuts use):

```toml
[tell]
host = "alpha"                  # this computer's name in sender lines (default: hostname -s)
door_services = "svc.door"      # XPC_SERVICE_NAME values meaning "this computer's front door", comma separated
job_prefix = "jobs."            # stripped from other XPC_SERVICE_NAME values: job:<rest>
door_parents = "svc.door"       # launchd job labels (macOS): a caller with no other marker running under that job is the front door
reply_hint = "reply with tell"  # how an A2A agent not waited on is told to answer

[tell.a2a.front]
url = "http://127.0.0.1:9900/"
token_file = "~/.config/secrets/api.env"
token_key = "FRONT_TOKEN"
```

An agent running inside a front door's own code tool has no marker in its environment. With `door_parents`, a caller with no other marker whose parent processes include one of those launchd jobs' process is the front door: the sender is the host name (macOS; elsewhere the setting does nothing).

## Explaining session status

`pctl session explain <name>` prints `{status,rule,row,text,considered_rows,pane_title}` without adding fields to `session list` or `session get`. Rows are 1-based positions in the visible screen. `considered_rows` contains the last 12 nonempty rows as `{row,text}` (ANSI styling removed); the OpenCode footer rule uses only the last four. A title-spinner match or no match has null `row` and `text`. Stopped, lost and exited sessions have no considered rows and a null title. A failed capture reports `unknown` with the error in `rule`.

Without a free `❯`/`›` input row, question words require dialog controls (a numbered choice, confirmation/cancellation hint, or Allow once/Reject choices). Quoted question words in ordinary answer or shell output do not block input.

## Worktree workers

`pctl session add --name <name> --cwd <repo> --worktree <branch> [--base <ref>] [--model <provider/model>] -- <argv>` gives a worker its own branch and checkout.

- The worktree is `<repo>-wt-<name>` beside the repository's main worktree. The session runs there. Its project defaults to the repository's folder name.
- `<branch>` is new and starts at `--base`. The base defaults to the branch checked out at `--cwd`. The branch has no upstream.
- These are refused before anything is created:
  - an existing branch;
  - an existing folder, or a path git still lists (locked or prunable);
  - uncommitted or untracked changes in the `--cwd` checkout when it is the base;
  - a detached HEAD without `--base`, or a bare repository;
  - a session name other than letters, digits, `-`, `_` and `.`.
- `--model` is for OpenCode. OpenCode 2's TUI has no model flag. Its shared background service ignores the caller's environment and keeps a folder's config from first use. pctl therefore launches `env OPENCODE_CONFIG_CONTENT={"model":"<model>"} <argv> --standalone`, a private OpenCode server. No file is written to the worktree. Other harnesses take their model flag in `<argv>`.
- New OpenCode conversation IDs use its native 12-hex descending timestamp plus 14 base62 characters after `ses_`. With OpenCode 2.0.32, the earlier PCTL UUID-length ID caused the free-tier refusal even in the official standalone TUI; native-format IDs work with the same model/config path. Existing stored or explicit IDs are preserved. If an older PCTL-created session still gets that refusal, explicitly use `session restart <name> --new` for a fresh native-format session (the old conversation is not deleted). Check the provider error if a refusal persists; PCTL does not alter provider headers, permissions, or model billing.

- `session list`/`get` show `branch` for these sessions.

`pctl worktree list` → `[{session,status,path,branch,base,exists,checked_out,dirty,ahead,merged,locked}]`
- `dirty` counts changed and untracked files.
- `ahead` counts the commits in `base..branch`.
- `merged` means the branch is an ancestor of the base.

`pctl worktree remove <name>` refuses unless all of these hold:
- the session has no run (`agent terminate` first);
- the worktree is unlocked, still on its branch, and clean;
- the branch is merged into its base.

It then:
1. moves the folder to `~/.Trash/<folder>-<unix time>`;
2. runs `git worktree prune`;
3. deletes the branch with `git branch -d` (kept, with git's reason, if git refuses);
4. unregisters the session.

There is no `--force`. Nothing is deleted outright.


## Documents (`pctl view`)

`pctl view <file|folder> [--project <name>] [--look portal|garage] [--dark]` opens the document reader (the `mdv` crate in `crates/mdv`) in the terminal it runs in. It needs an interactive terminal and works in any one: nothing in it is specific to a terminal program. The reader is built in; the `mdv` command is not needed. `--project` selects the project whose main receives questions; otherwise the reader uses the path under `$HOME/Projects`.

- Copy (`y`) puts the text on the clipboard of the computer it runs on (the same `PCTL_CLIP_WRITE_TEXT` seam and command search as the app) and writes OSC 52 to the terminal. The status line says `Copied` only when a command ran here or the terminal is known to honour OSC 52; over ssh no command runs and it says `paste to verify`; a VTE terminal (MATE Terminal, GNOME Terminal) ignores OSC 52 and the line says so.
- A question about a passage goes to the main agent of the document's project, through `tell` in the same process (the reader first checks that there is exactly one running `*-main` session in the project, and that it is still the run it picked). Delivery states are `tell`'s: reached, queued, typed but unconfirmed, and blocked (nothing is typed, the question is kept and can be retried). An agent answers with `pctl show --answer <request-id> <text>`.
- Questions and answers are kept in `~/.local/state/mdv/threads.jsonl` (`MDV_THREADS_FILE` moves it).

## Showing a document (`pctl show`)

`pctl show <file|folder> [--title T] [--note N] [--project P]` is how an agent tells the person "read this". It records the document in `<state>/docs.json` and prints the record as JSON. Nothing opens and no focus moves: the app lists the document under its project as `new` and sends one notice; the person opens it when they choose. Show a document only when the person must read it, not on every save.

- The project is the caller's own session project; `--project` (a name or ID) names another, and is required outside a session. The record keeps who showed it (session and run), the canonical path, the title (default: the file name), the note (one line, 500 characters), the time and the computer.
- The path is checked on the computer that owns the file: it must exist, must not be a private place (`~/.ssh`, hidden folders in the home folder other than `.config` and `.local`, `.env` and key files, `secret`/`token`/`credential` names, shell history, the pctl state folder), and is stored canonical.
- Showing the same path again keeps one record. If the content is unchanged it stays as it was (a read document stays read); if it changed it is `new` again. Each record keeps the latest content and the version before it under `<state>/docs/<id>/` (`latest`, `previous`, mode 0600), for files up to 20 MiB; bigger files and folders are kept by path only. At most 200 documents are kept.
- `pctl show --list [--project P]` prints the documents, newest first (JSON: `id`, `project`, `by`, `run`, `path`, `title`, `note`, `shown`, `host`, `new`, `previous`). `pctl show --read <id>` marks one read; it refuses an agent caller, because only the person opening a document does that.
- `pctl show --answer <request-id> <text>` answers a question asked in `pctl view` (`-` reads the text from standard input); an unknown request id is refused.
- The bridge `list` answer has a `docs` array, and the op `doc_read` (field `doc`) marks one read, so the app shows and reads another computer's documents over ssh.

### Documents in the app

- Each project's documents are listed under its agents as `📄 <title>`, with `new` until opened (the newest 8 per project). The list is read with the agents, so a document shown on another computer appears through that computer's bridge too.
- A document that arrives (or is shown again with new content) sends one notice through the same path as `done` and `needs you`: `<agent>: wants you to read <title>`. Nothing opens, no cell is created and the focus does not move: the agent's cell keeps taking the keys until the person clicks.
- Clicking a row (or Enter on it in the sidebar) opens the document in a new cell right of the cell that shows the agent which showed it, else of the focused cell, and marks it read. The cell is a session named `doc-<first 8 characters of the id>` in the document's project, running `pctl view <path>` on the computer that owns the file (another computer: through the bridge op `act` with `doc_open`, which looks the path up in that computer's `docs.json` and checks it again; the reader there is started with `--remote`, so a copy key says "drag to copy" instead of writing to that computer's clipboard). Keys, paste, the wheel and a mouse drag go to it like to an agent. The reader session is not an agent: it has no tab or sidebar row, and `pctl session list` marks it `"viewer": true`.
- Closing the cell (or putting another agent in it) ends the reader (`doc_close`), not the document; clicking the row again starts a new reader and the questions and answers come back. Quitting the app ends the readers on this computer. A reader that exits by itself (`q`) leaves its cell showing the last screen, marked `closed`, until the cell is closed.

## Installed harness builds and explicit refresh

`session list`, `get` and `explain` include `running_build`, `installed_build`, `stale` (true, false, or `"unknown"`) and `swap_blockers`. `protected` is stored per session; `pctl session protect NAME` holds it, and `--off` releases it. The app sidebar adds `↑` to a stale agent. This is a local installed-build comparison, with no network lookup or installation.

New run records contain the resolved executable path, size, device/inode and nanosecond mtime. A symlink target change and a replacement at the same path both count. Simple scripts record their entrypoint and shebang interpreter. PATH comes from the launching tmux session/server (or the caller when creating a private server), with the profile's `env PATH=…` override. Refresh pins its selected executable to the resolved path; ordinary starts keep their original command. Unsupported env/shebang options and unavailable metadata produce unknown; old runs without a recorded identity remain unknown until a manual restart. Dynamically loaded packages/plugins and opaque wrapper internals are not build identities.

`pctl session refresh NAME` is currently enabled only for Claude Code profiles whose conversation PCTL manages. Codex and OpenCode stay manual-only. Refresh requires a stale, idle, unprotected session with a known conversation and build, and refuses explicit conversation-selector arguments. A session in a visible app cell is held; viewer records bind to the app/bridge process generation and are cleared on Leave (dead viewers no longer hold sessions). These records cover PCTL app cells, not a person's gaze or arbitrary terminal viewers.

Refresh checks the evidence initially and again under the registry lock immediately before stopping the verified run. It checks that the pane and old process are gone before starting the new build, keeps the conversation ID, and applies the existing 500 ms startup survival check. If the target changes or startup fails it stops and reports the failure; it never installs, passes --force, creates a new conversation, retries another build or rolls back. An exited startup pane stays visible for diagnosis. `STATE/refresh.jsonl` records before/after run IDs, conversation, build identities and outcome; the command prints the final receipt (inside the error on failure).

There is no automatic refresh or shared-server update. Metadata comparison is not a content hash or proof of loaded modules. Concurrent writes preserving every recorded metadata field cannot be detected. Screen idle classification and a successful startup survival check do not prove that a harness has fully restored its conversation or finished background work.
