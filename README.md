# pctl

A tmux-based control plane for interactive coding-agent sessions (Claude Code, Codex). One tmux server per state directory. Sessions are addressed by name and can be listed, read, prompted, waited on, restarted on the same conversation, and restored after a reboot. Agents send each other messages through it.

- Origin: the PCTL 0.6.0 prototype by Pikk (MIT, "pctl prototype contributors"). This repository adds a macOS port, `--host`, msg wake-ups, and the `pctl session` commands.
- `CLI.md` is the command and JSON contract. `SKILL.md` is the agent-facing guide. Both are compiled into the binary, so keep them accurate.
- Private and under active development. It is not a sandbox: the state directory is trusted local state.

## Build

Requirements: Rust 1.88+ (edition 2024), tmux 3.x, and Python 3 for the integration suites. Runs on macOS arm64 and Linux.

```sh
cargo build --release --locked
```

## Test

```sh
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
```

The real-tmux suites start their own servers in temporary state directories. Each takes `--bin`, `--tmux` and `--report`, except `remote_quoting.py`, which takes only `--bin`. For now they need `HERDR_ENV=1` (see the first issue):

```sh
B=target/release/pctl T=$(command -v tmux); mkdir -p test-results
for s in integration collaboration_integration msg_workflow_integration orchestration_integration \
         session_integration sidebar_integration sidebar_recovery snapshot_batch_integration \
         team_context_preflight tmux_encoding_integration wake_integration; do
  HERDR_ENV=1 python3 tests/$s.py --bin $B --tmux $T --report test-results/$s.json >test-results/$s.out 2>&1; echo "$s exit $?"
done
HERDR_ENV=1 python3 tests/remote_quoting.py --bin $B; echo "remote_quoting exit $?"
```

## Workflow

1. Every change starts from an issue that says what to change, when it is done, and how to test it.
2. Branch `<who>/<issue>-<slug>` from `main` and keep the change small.
3. Open a PR that links the issue and pastes the exact commands and results: unit tests, clippy, and the suites the change touches.
4. The maintainer re-runs the tests and merges. There is no CI and no auto-merge.
5. Contributors without repository access send `git format-patch` output against a named `main` commit. The maintainer applies it on a branch and opens the PR.

Do not commit secrets, tokens, host names, personal paths, or logs. Tests must use temporary state, never a real `~/.local/state/pctl`.
