#!/bin/bash
# Run in the permitted native terminal, not the socket-restricted exec context.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/pctl}"
TMUX_BIN="${2:-$ROOT/../prototype-runtime/bin/tmux}"
mkdir -p "$ROOT/test-results"
python3 "$ROOT/tests/sidebar_integration.py" --bin "$BIN" --tmux "$TMUX_BIN" --report "$ROOT/test-results/v04-sidebar-integration.json" > "$ROOT/test-results/v04-sidebar-integration.log" 2>&1
sidebar=$?
printf '%s\n' "$sidebar" > "$ROOT/test-results/v04-sidebar-integration.exit"
python3 "$ROOT/tests/integration.py" --bin "$BIN" --tmux "$TMUX_BIN" --report "$ROOT/test-results/v04-integration.json" > "$ROOT/test-results/v04-integration.log" 2>&1
core=$?
printf '%s\n' "$core" > "$ROOT/test-results/v04-integration.exit"
python3 "$ROOT/tests/collaboration_integration.py" --bin "$BIN" --tmux "$TMUX_BIN" --report "$ROOT/test-results/v04-collaboration-integration.json" > "$ROOT/test-results/v04-collaboration-integration.log" 2>&1
mail=$?
printf '%s\n' "$mail" > "$ROOT/test-results/v04-collaboration-integration.exit"
python3 "$ROOT/tests/sidebar_recovery.py" --bin "$BIN" --tmux "$TMUX_BIN" --report "$ROOT/test-results/v04-sidebar-recovery.json" > "$ROOT/test-results/v04-sidebar-recovery.log" 2>&1
recovery=$?
printf '%s\n' "$recovery" > "$ROOT/test-results/v04-sidebar-recovery.exit"
exit $((sidebar || core || mail || recovery))
