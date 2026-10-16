# pctl lite: quick start

pctl runs interactive coding agents (any terminal CLI) in tmux panes, addressed by name, and lets them send each other messages. It works on one computer and needs no network.

## Requirements
- Linux x86_64. The binary is statically linked, so it needs no glibc or other libraries.
- tmux 3.0a or newer, on PATH or set with `PCTL_TMUX`
- bash, only for `smoke.sh`

## Your part (people, not pctl)
- Get your organization's approval before you copy, install or run pctl, and follow its software rules.
- pctl contains no AI model and no credentials. Install each agent CLI you start (for example claude or codex) and log in to it yourself, using the setup your organization allows.
- tmux must be installed. Ask your administrator if it is missing.

## Install (offline)
1. Unzip the ZIP.
2. Run `sh install.sh`.
   - It checks `SHA256SUMS` and copies `bin/pctl` to `~/.local/bin`.
   - To install somewhere else, give the directory: `sh install.sh /some/bin`.
   - Copying `bin/pctl` onto your PATH by hand works too.
3. Add `export PCTL_STANDALONE=1` to your shell profile (see `pctl.env.example`). Without it, pctl refuses to start its tmux server.
4. Check the install:
   - `pctl doctor`
   - `bash smoke.sh ~/.local/bin/pctl`
   - The smoke test takes about 30 seconds and uses fake agents in a new folder under `/tmp`. Its own tmux server is stopped at the end.

## Sessions (run from any terminal)
| Do | Command |
|---|---|
| Start an agent | `pctl session add --name alpha --cwd ~/work/app -- <agent command and args>` |
| List | `pctl session list` |
| One session | `pctl session get alpha` |
| Read the screen | `pctl session read alpha --lines 40` |
| Type a line | `pctl session prompt alpha "text"` |
| Wait until it stops working | `pctl session wait alpha` |
| Restart | `pctl session restart alpha` |
| Watch and type yourself | `pctl attach --project <id>` |
| Stop | `pctl agent terminate --agent <agent_id> --expected-run <run_id>` |

Notes:
- **Start:** the folder name becomes the project unless you pass `--project <name>`. Session names are unique.
- **Type a line:** refused while the agent waits for a human decision.
- **Restart:** claude and codex resume the same conversation. If pctl does not know the conversation ID, add `--new` to start a fresh one. Other commands simply start again. Restart is refused while the agent waits for a human decision.
- **Watch:** the `<id>` comes from `pctl project list`. Run it from a plain terminal, not inside tmux. Detach with the tmux prefix, then `d`.
- **Stop:** the IDs come from `pctl session get alpha`. Start a stopped session again with `pctl session start alpha`.

## Agent to agent (run inside an agent's pane)
pctl knows who is sending from the pane it runs in. Agents in the same project address each other by session name.

| Do | Command |
|---|---|
| Who can I message | `pctl msg peers` |
| Send | `pctl msg send beta --key <unique-key> --text "..."` |
| Read my messages | `pctl msg inbox` |
| Take one | `pctl msg consume m1` |
| Mark it received | `pctl msg ack m1` |
| Answer | `pctl msg reply m1 --key <unique-key> --text "..."` |
| Report done | `pctl msg complete m1 --summary "..."` |
| Delivery state | `pctl msg status m1` |

- **Send:** the message is stored first.
  - If the recipient is idle, its pane gets one notice line: `[pctl] new message m1 from alpha`. The message text stays in the mailbox.
  - Sending again with the same key does not send twice.
- **Delivery state:** shows when the message was stored, taken, acknowledged and reported done.

## Files and state
- `~/.local/state/pctl` holds one tmux server, the registry and the mailboxes for all projects.
- **Another place:** set `PCTL_STATE` to a folder with more space or a different filesystem.
  - Keep that path short. The tmux socket path `$PCTL_STATE/tmux.sock` must be under 104 bytes.
  - The folder must allow Unix sockets and file locks.
- **noexec:** `pctl` must sit on a filesystem that allows running programs (not mounted `noexec`).
  - `smoke.sh` works under `/tmp` by default.
  - If `/tmp` is mounted `noexec` or is full, set `PCTL_SMOKE_DIR=/some/short/dir`.
- **Uninstall:**
  1. Stop the server: `tmux -S ~/.local/state/pctl/tmux.sock kill-server`.
  2. Delete the `pctl` binary and that folder.

## Not covered here
- The binary also has sidebar, theme, team and demo commands. They are not part of this quick start.
- `pctl --host` uses ssh only when you pass it.
- `pctl web` (an optional page on loopback or a Tailscale address) is not part of this quick start. pctl makes no outgoing network calls.
- Agents use their own existing install and login. pctl never stores credentials.

`BUILDINFO.txt` says which source commit and target this ZIP was built from. `LICENSE` covers pctl (MIT), and `THIRD_PARTY_NOTICES.txt` lists the bundled Rust crates and musl libc.
