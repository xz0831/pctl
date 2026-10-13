#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BIN=${PCTL_BIN:-"$ROOT/target/release/pctl"}
TMUX_BIN=${TMUX_BIN:-$(command -v tmux || true)}
STATE=${PCTL_STATE:-"$(mktemp -d /tmp/pctl-demo-$(id -u)-XXXXXX)"}
if [ -z "$TMUX_BIN" ]; then echo 'Set TMUX_BIN to an installed tmux executable.' >&2; exit 2; fi
if [ ! -x "$BIN" ]; then echo 'Build first: cargo build --release --locked' >&2; exit 2; fi
umask 077
mkdir -p "$STATE"
RESULT=$(mktemp "$STATE/demo-created.XXXXXX.json")
"$BIN" --state "$STATE" --tmux "$TMUX_BIN" --theme "${PCTL_THEME:-portal}" demo > "$RESULT"
PROJECT=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["projects"][0]["id"])' "$RESULT")
printf 'Demo registry: %s\nCtrl-b m: manager. Ctrl-b d: detach and keep agents running.\n' "$STATE"
exec "$BIN" --state "$STATE" --tmux "$TMUX_BIN" attach --project "$PROJECT"
