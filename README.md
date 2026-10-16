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

The real-tmux suites start their own servers in temporary state directories. Each takes `--bin`, `--tmux` and `--report`, except `remote_quoting.py`, which takes only `--bin`:

```sh
B=target/release/pctl T=$(command -v tmux); mkdir -p test-results
for s in integration collaboration_integration msg_workflow_integration orchestration_integration \
         session_integration sidebar_integration sidebar_recovery snapshot_batch_integration \
         team_context_preflight tmux_encoding_integration wake_integration; do
  python3 tests/$s.py --bin $B --tmux $T --report test-results/$s.json >test-results/$s.out 2>&1; echo "$s exit $?"
done
python3 tests/remote_quoting.py --bin $B; echo "remote_quoting exit $?"
```

## Hosts without Herdr

pctl starts its tmux server only for a caller in a Herdr pane (`HERDR_ENV=1`) or with `PCTL_STANDALONE=1`. On a host without Herdr, set `PCTL_STANDALONE=1`.

## Lite package

`scripts/package-lite.sh [target]` builds `dist/pctl-lite-<version>-<target>.zip` for offline install. The ZIP contains:
- a static binary
- `install.sh` (copies the binary, no network)
- `QUICKSTART.md` and `pctl.env.example`
- `smoke.sh` (the same file as `scripts/lite-smoke.sh`)
- licenses, `BUILDINFO.txt` and `SHA256SUMS`

The default target is `x86_64-unknown-linux-musl`. It links with the toolchain's `rust-lld` and needs `rustup target add x86_64-unknown-linux-musl`.

`scripts/lite-smoke.sh target/release/pctl` runs the offline smoke test: two fake agents, session control, messages, duplicate and blocked refusals, and restart.

## Workflow

1. Every change starts from an issue that says what to change, when it is done, and how to test it.
2. Branch `<who>/<issue>-<slug>` from `main` and keep the change small.
3. Open a PR that links the issue and pastes the exact commands and results: unit tests, clippy, and the suites the change touches.
4. The maintainer re-runs the tests and merges. There is no CI and no auto-merge.
5. Contributors without repository access send `git format-patch` output against a named `main` commit. The maintainer applies it on a branch and opens the PR.

Do not commit secrets, tokens, host names, personal paths, or logs. Tests must use temporary state, never a real `~/.local/state/pctl`.
