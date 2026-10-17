#!/usr/bin/env bash
# Offline smoke test: real tmux, two fake agents, no model, no network.
# Covers session create/list/read/input/attach, name-addressed send/reply with wake notice and
# delivery evidence, duplicate and blocked refusals, and restart. Leaves nothing running.
# Usage: scripts/lite-smoke.sh [path/to/pctl]   (tmux: $PCTL_TMUX, else tmux on PATH;
#        work folder: $PCTL_SMOKE_DIR, default /tmp)
set -u
B=${1:-$(command -v pctl)} || { echo "FAIL pctl not found"; exit 1; }
B=$(cd "$(dirname "$B")" && pwd)/$(basename "$B")
T=${PCTL_TMUX:-$(command -v tmux)} || { echo "FAIL tmux not found"; exit 1; }
base=${PCTL_SMOKE_DIR:-/tmp}
for v in $(compgen -e); do case $v in PCTL_* | TMUX | TMUX_PANE) unset "$v" ;; esac; done
root=$(mktemp -d "$base/pctl-smoke.XXXXXX") || { echo "FAIL cannot create a folder in $base"; exit 1; }
export XDG_CONFIG_HOME=$root/config  # no [server] start_env from the person's config
S=$root/s
n=0
cleanup() {
  "$T" -S "$root/outer.sock" kill-server 2>/dev/null
  "$T" -S "$S/tmux.sock" kill-server 2>/dev/null
}
trap cleanup EXIT
p() { "$B" --state "$S" --tmux "$T" "$@"; }
pass() { n=$((n + 1)); echo "PASS $n $1"; }
fail() { echo "FAIL $1"; echo "kept for inspection: $root"; exit 1; }
waitfor() { for _ in $(seq 1 100); do eval "$1" && return 0; sleep 0.1; done; return 1; }
field() { p session get "$1" | sed -n "s/^  \"$2\": \"\(.*\)\",\{0,1\}$/\1/p"; } # top-level string field
as() { # run `pctl msg` as session $1, with the context its own pane would carry
  local who=$1; shift
  env PCTL_BIN="$B" PCTL_STATE="$S" PCTL_TMUX="$T" PCTL_PROJECT_ID="$pid" \
    PCTL_AGENT_ID="$(field "$who" agent_id)" PCTL_RUN_ID="$(field "$who" run_id)" "$B" --state "$S" --tmux "$T" msg "$@"
}

echo "pctl $("$B" --version) | $("$T" -V) | $(uname -sm) | state $S"
cat >"$root/agent" <<'EOF'
#!/bin/sh
# Fake agent: echoes each line; "block" shows an approval prompt; "clear" redraws an idle prompt.
echo "> "
while IFS= read -r line; do
  case "$line" in
    block*) echo "Do you want to proceed?"; echo "❯ 1. Yes" ;;
    clear*) printf '\033[2J\033[H> \n' ;;
    *) echo "got: $line"; echo "> " ;;
  esac
done
EOF
mkdir -p "$root/alpha" "$root/beta"

out=$(p session add --name alpha --cwd "$root/alpha" --project lite -- /bin/sh "$root/agent" 2>&1)
grep -q server_start_not_allowed <<<"$out" && [ "$(p session list | tr -d ' \n')" = "[]" ] ||
  fail "without PCTL_STANDALONE=1 the first start must be refused and register nothing: $out"
pass "no server is started unless PCTL_STANDALONE=1; refusal registers nothing"
export PCTL_STANDALONE=1

for s in alpha beta; do
  p session add --name $s --cwd "$root/$s" --project lite -- /bin/sh "$root/agent" >/dev/null || fail "session add $s"
done
[ "$(field alpha process)" = running ] && [ "$(field beta process)" = running ] || fail "sessions not running"
pass "session add creates and starts two agent panes"
list=$(p session list)
grep -q '"name": "alpha"' <<<"$list" && grep -q '"name": "beta"' <<<"$list" || fail "session list: $list"
pass "session list shows both"
p session add --name alpha --cwd "$root/alpha" --project lite -- /bin/sh "$root/agent" 2>&1 | grep -q conflict ||
  fail "duplicate name accepted"
pass "duplicate session name is refused"

waitfor '[ "$(field alpha status)" = idle ]' || fail "alpha never idle: $(field alpha status)"
p session prompt alpha "hello lite" >/dev/null || fail "prompt"
waitfor 'p session read alpha --lines 5 | grep -q "got: hello lite"' || fail "prompt not echoed"
pass "session prompt types a line; session read shows the answer"

pid=$(p project list | sed -n 's/^    "id": "\(.*\)",$/\1/p' | head -1)
"$T" -S "$root/outer.sock" new-session -d -x 120 -y 40 \
  "env -u TMUX PCTL_STANDALONE=1 '$B' --state '$S' --tmux '$T' attach --project $pid"
waitfor '[ -n "$("$T" -S "$S/tmux.sock" list-clients 2>/dev/null)" ]' || fail "attach: no client"
active=$("$T" -S "$S/tmux.sock" list-clients -F '#{pane_id}')
[ "$active" = "$(field alpha pane)" ] || [ "$active" = "$(field beta pane)" ] || fail "attach landed on an unregistered pane: $active"
[ "$("$T" -S "$S/tmux.sock" list-panes -a -F '#{pane_id}' | wc -l | tr -d ' ')" = 2 ] || fail "unexpected control-panel/placeholder pane"
"$T" -S "$root/outer.sock" send-keys -t 0 'raw-smoke' Enter
waitfor 'p session read alpha --lines 10 | grep -q "got: raw-smoke" || p session read beta --lines 10 | grep -q "got: raw-smoke"' || fail "raw attach typing"
"$T" -S "$root/outer.sock" kill-server
pass "attach lands on an agent pane; raw typing reaches it; no manager exists"
"$T" -S "$root/outer.sock" new-session -d -x 120 -y 40 \
  "env -u TMUX -u NO_COLOR '$B' --state '$S' --tmux '$T'; echo APP-LEFT; sleep 60"
waitfor '"$T" -S "$root/outer.sock" capture-pane -p -t 0 | grep -q "+ agent"' || fail "app did not open"
"$T" -S "$root/outer.sock" send-keys -t 0 C-b q
waitfor '"$T" -S "$root/outer.sock" capture-pane -p -t 0 | grep -q APP-LEFT' || fail "app Leave"
[ "$(field alpha process)" = running ] && [ "$(field beta process)" = running ] || fail "Leave stopped an agent"
"$T" -S "$root/outer.sock" kill-server
pass "pctl alone opens the app; Leave exits and both agents survive"

as alpha peers | grep -q '"role": "beta"' || fail "peers"
sent=$(as alpha send beta --key k1 --text "please review" 2>&1)
grep -q '"handle": "m1"' <<<"$sent" && grep -q '"wake": "delivered' <<<"$sent" || fail "send: $sent"
waitfor 'p session read beta --lines 10 | grep -q "\[pctl\] new message m1 from alpha"' || fail "no wake notice"
p session read beta --lines 20 | grep -q "please review" && fail "message body leaked into the terminal"
pass "send by name stores the message and wakes the idle recipient with a notice line only"
as alpha send beta --key k1 --text "please review" | grep -q '"deduplicated": true' || fail "retry not deduplicated"
pass "retry with the same key is deduplicated"
as beta inbox | grep -q '"text": "please review"' || fail "inbox"
as beta consume m1 >/dev/null && as beta ack m1 >/dev/null || fail "consume/ack"
st=$(as alpha status m1)
grep -q '"acknowledged_at": [0-9]' <<<"$st" && grep -q '"recorded_at": [0-9]' <<<"$st" || fail "delivery state: $st"
pass "recipient inbox/consume/ack; sender status shows retrieval and acknowledgment"
rep=$(as beta reply m1 --key r1 --text "approved" 2>&1)
grep -q '"handle": "m2"' <<<"$rep" && grep -q '"in_reply_to": "m1"' <<<"$rep" || fail "reply: $rep"
waitfor 'p session read alpha --lines 10 | grep -q "\[pctl\] new message m2 from beta"' || fail "no reply notice"
as alpha inbox | grep -q '"text": "approved"' || fail "reply not in sender inbox"
as beta complete m1 --summary "reviewed" >/dev/null || fail "complete"
as alpha status m1 | grep -q '"summary": "reviewed"' || fail "completion not visible to sender"
pass "reply returns to the sender; completion report is visible to the sender"

p session prompt beta "block" >/dev/null
waitfor '[ "$(field beta status)" = blocked ]' || fail "blocked not detected: $(field beta status)"
p session prompt beta "yes" 2>&1 | grep -q blocked || fail "prompt into a blocked session was accepted"
p session restart beta --force 2>&1 | grep -q blocked || fail "restart of a blocked session was accepted"
pass "a session waiting for a human decision is reported and never typed into or restarted"

old=$(field alpha run_id)
p session restart alpha >/dev/null 2>&1 || p session restart alpha --new >/dev/null || fail "restart"
waitfor '[ "$(field alpha process)" = running ] && [ "$(field alpha run_id)" != "$old" ]' || fail "restart run"
[ "$(p session list | grep -c '"name": ')" = 2 ] || fail "restart duplicated a session"
pass "restart replaces the run under the same name"

echo "OK $n checks"
