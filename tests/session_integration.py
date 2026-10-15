#!/usr/bin/env python3
"""Name-addressed sessions (msg/Hermes surface): no Herdr identity leaks into panes, list/get/prompt/
read/wait/whoami, blocked sessions are never typed into or restarted, conversation IDs survive
restart and restore, and the SessionStart hook records Codex threads. Fake harnesses only."""
import argparse, json, os, pathlib, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-sess-', dir=os.path.realpath('/tmp'))); state = root / 's'; home = root / 'home'; results = []
bindir = root / 'bin'; bindir.mkdir()
FAKE = r'''#!/bin/bash
echo "$(basename "$0") $*" >> argv.log
env > env.txt
"$PCTL_BIN" --state "$PCTL_STATE" --tmux "$PCTL_TMUX" session whoami > whoami.json 2>&1
echo "> "
while IFS= read -r line; do
  case "$line" in
    work*) echo "✻ Thinking… (3s · esc to interrupt)"; sleep 2; printf '\033[2J\033[Hdone\n> \n' ;;  # real harnesses redraw the footer away
    block*) echo "Do you want to proceed?"; echo "❯ 1. Yes" ;;
    clear*) printf '\033[2J\033[H> \n' ;;
    *) echo "got: $line"; echo "> " ;;
  esac
done
'''
for name in ('claude', 'codex', 'plain'):
    (bindir / name).write_text(FAKE); (bindir / name).chmod(0o755)
# The caller looks like a Herdr pane inside a Codex command: none of it may reach PCTL panes.
ENV = dict(os.environ, HOME=str(home), HERDR_ENV='1', HERDR_PANE_ID='w9:p9', HERDR_SOCKET_PATH='/nonexistent', CODEX_THREAD_ID='leak')
def cli(*args, ok=True, env=None, stdin=None):
    q = subprocess.run([B, '--state', str(state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=30, env=env or ENV, input=stdin)
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    return (json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout) if ok else q.stderr
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def cwd(n):
    d = root / 'proj' / n; d.mkdir(parents=True, exist_ok=True); return d
try:
    def add():
        for n, exe in (('c-main', 'claude'), ('x-work', 'codex'), ('p-shell', 'plain')):
            v = cli('session', 'add', '--name', n, '--cwd', cwd(n), '--project', 'demo', '--', bindir / exe)
            assert v['process'] == 'running' and v['project'] == 'demo', v
        assert 'conflict' in cli('session', 'add', '--name', 'c-main', '--cwd', cwd('c-main'), '--', bindir / 'plain', ok=False)
        names = [s['name'] for s in cli('session', 'list')]
        assert names == ['c-main', 'x-work', 'p-shell'], names
    case('session add registers, opts in and starts; names are unique', add)
    def no_leak():
        env = cwd('p-shell') / 'env.txt'
        assert until(env.exists)
        text = env.read_text()
        leaked = [l.split('=')[0] for l in text.splitlines() if l.startswith('HERDR_') or l.startswith('CODEX_THREAD_ID=')]
        assert leaked == [], leaked
        assert 'PCTL_AGENT_ID=' in text
    case('PCTL panes carry no HERDR_* or CODEX_THREAD_ID from the server starter', no_leak)
    def whoami():
        f = cwd('c-main') / 'whoami.json'
        assert until(lambda: f.exists() and f.read_text().strip())
        v = json.loads(f.read_text()); assert v['name'] == 'c-main' and v['current_run'], v
    case('whoami names the caller from inside its pane', whoami)
    def prompt_read():
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'idle')
        v = cli('session', 'prompt', 'p-shell', 'hello 한글 "q" $HOME')
        assert v['status_before'] == 'idle', v
        assert until(lambda: 'got: hello 한글 "q" $HOME' in cli('session', 'read', 'p-shell', '--lines', '5'))
        cli('session', 'prompt', 'p-shell', 'line one\nline two')
        assert until(lambda: 'got: line two' in cli('session', 'read', 'p-shell', '--lines', '5'))
        assert len(cli('session', 'read', 'p-shell', '--lines', '3').strip().splitlines()) <= 3
    case('prompt delivers text (multi-line too) and read shows the screen tail', prompt_read)
    def working_wait():
        cli('session', 'prompt', 'p-shell', 'work')
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'working', 3)
        assert 'busy' in cli('session', 'restart', 'p-shell', ok=False)
        v = cli('session', 'wait', 'p-shell', '--timeout-ms', '10000')
        assert v['status'] == 'idle', v
        assert 'timeout' in cli('session', 'wait', 'p-shell', '--until', 'working', '--timeout-ms', '600', ok=False)
    case('working is detected, restart waits for it, wait returns when idle', working_wait)
    def blocked():
        cli('session', 'prompt', 'p-shell', 'block')
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'blocked', 3)
        assert 'blocked' in cli('session', 'prompt', 'p-shell', 'yes', ok=False)
        assert 'blocked' in cli('session', 'restart', 'p-shell', '--force', ok=False)
        assert 'got: yes' not in cli('session', 'read', 'p-shell')
        s = cli('session', 'get', 'p-shell')
        cli('input', 'paste', '--agent', s['agent_id'], '--expected-run', s['run_id'], '--text', 'clear')
        cli('input', 'submit', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'idle')
    case('blocked sessions are reported, never typed into or restarted', blocked)
    def claude_resume():
        c = cli('session', 'get', 'c-main')['conversation']; assert c, 'claude gets a fixed session id'
        log = cwd('c-main') / 'argv.log'
        assert until(lambda: log.exists() and log.read_text().strip()), 'first start logged'
        assert log.read_text().splitlines()[0] == f'claude --session-id {c}'
        cli('session', 'restart', 'c-main')
        assert until(lambda: len(log.read_text().splitlines()) == 2)
        assert log.read_text().splitlines()[1] == f'claude --resume {c}'
    case('claude: fixed --session-id on first start, --resume on restart', claude_resume)
    def moved():
        cli('session', 'add', '--name', 'c-moved', '--cwd', cwd('c-moved'), '--conversation', 'herdr-convo-1', '--', bindir / 'claude')
        log = cwd('c-moved') / 'argv.log'
        assert until(lambda: log.exists() and log.read_text().strip())
        assert log.read_text().splitlines()[0] == 'claude --resume herdr-convo-1'
        assert cli('session', 'get', 'c-moved')['conversation'] == 'herdr-convo-1'
        assert 'needs a claude or codex' in cli('session', 'add', '--name', 'p-moved', '--cwd', cwd('p-moved'), '--conversation', 'x', '--', bindir / 'plain', ok=False)
    case('add --conversation starts a moved session in its existing conversation', moved)
    def remove():
        assert 'has_run' in cli('session', 'remove', 'c-moved', ok=False)
        s = cli('session', 'get', 'c-moved')
        cli('agent', 'terminate', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        assert cli('session', 'remove', 'c-moved')['removed'] == 'c-moved'
        assert 'c-moved' not in [x['name'] for x in cli('session', 'list')]
        assert 'not_found' in cli('session', 'remove', 'c-moved', ok=False)
    case('remove unregisters only a session without a run', remove)
    def codex_hook():
        assert cli('session', 'get', 'x-work')['conversation'] is None, 'no rollout: unknown, no guess'
        s = cli('session', 'get', 'x-work')
        # Silent always: Claude adds SessionStart hook output to the conversation.
        assert cli('session', 'hook', stdin='{"session_id":"t-outside"}') == ''
        assert cli('session', 'hook', stdin='not json') == ''
        env = dict(ENV, PCTL_AGENT_ID=s['agent_id'], PCTL_RUN_ID=s['run_id'])
        assert cli('session', 'hook', env=env, stdin='{"hook_event_name":"SessionStart","session_id":"01a1-thread"}') == ''
        stale = dict(env, PCTL_RUN_ID='old')
        cli('session', 'hook', env=stale, stdin='{"session_id":"other"}')
        assert cli('session', 'get', 'x-work')['conversation'] == '01a1-thread'
        cli('session', 'restart', 'x-work')
        log = cwd('x-work') / 'argv.log'
        assert until(lambda: log.exists() and 'codex resume 01a1-thread' in log.read_text())
    case('codex: hook records the thread for the current run only; restart resumes it', codex_hook)
    def codex_fallback():
        cli('session', 'add', '--name', 'x-two', '--cwd', cwd('x-two'), '--', bindir / 'codex')
        day = home / '.codex/sessions/2026/10/15'; day.mkdir(parents=True, exist_ok=True)
        os.utime(home / '.codex/sessions', (0, 0)); os.utime(home / '.codex/sessions/2026', (0, 0))  # old year folders, as on a real host
        meta = lambda i: json.dumps({'type': 'session_meta', 'payload': {'id': i, 'cwd': str(cwd('x-two'))}}) + '\n'
        (day / 'rollout-1.jsonl').write_text(meta('only-one'))
        assert cli('session', 'get', 'x-two')['conversation'] == 'only-one'
        cli('session', 'add', '--name', 'x-three', '--cwd', cwd('x-three'), '--', bindir / 'codex')
        meta3 = lambda i: json.dumps({'type': 'session_meta', 'payload': {'id': i, 'cwd': str(cwd('x-three'))}}) + '\n'
        (day / 'rollout-2.jsonl').write_text(meta3('twin-a')); (day / 'rollout-3.jsonl').write_text(meta3('twin-b'))
        assert cli('session', 'get', 'x-three')['conversation'] is None, 'two candidates: unknown'
    case('codex fallback: one rollout candidate is taken, two are left unknown', codex_fallback)
    def unknown_conversation():
        log3 = cwd('x-three') / 'argv.log'; assert until(log3.exists)
        assert 'conversation_unknown' in cli('session', 'restart', 'x-three', ok=False)
        assert len((cwd('x-three') / 'argv.log').read_text().splitlines()) == 1, 'not restarted'
    case('restart refuses a session whose conversation ID is unknown', unknown_conversation)
    def gate():
        outside = {k: v for k, v in ENV.items() if not k.startswith('HERDR_')}
        other = root / 's2'
        q = subprocess.run([B, '--state', str(other), '--tmux', T, 'session', 'add', '--name', 'g', '--cwd', str(cwd('g')), '--', str(bindir / 'plain')], text=True, capture_output=True, timeout=30, env=outside)
        assert q.returncode != 0 and json.loads(q.stderr)['error_code'] == 'server_start_outside_herdr', q.stderr
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server created'
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
        err = json.loads(cli('session', 'restore', ok=False, env=outside))
        assert err['error_code'] == 'server_start_outside_herdr', err
        assert subprocess.run([T, '-S', str(state / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server created'
        assert all(s['process'] == 'lost' for s in cli('session', 'list', env=outside)), 'refused restore keeps every run'
    case('outside Herdr (no HERDR_ENV) no server is created: add and restore are refused', gate)
    def restore():
        before = {s['name']: s['conversation'] for s in cli('session', 'list')}
        out = {o['name']: o for o in cli('session', 'restore')['restored']}
        assert sorted(out) == sorted(before), out
        assert 'conversation_unknown' in out['x-three']['error'], out['x-three']
        assert all(o['error'] is None for n, o in out.items() if n != 'x-three'), out
        after = {s['name']: s for s in cli('session', 'list')}
        assert after['x-three']['process'] == 'lost', 'skipped session stays visible as lost'
        assert all(s['process'] == 'running' for n, s in after.items() if n != 'x-three'), after
        assert {n: s['conversation'] for n, s in after.items()} == before
        log = cwd('c-main') / 'argv.log'
        assert until(lambda: log.read_text().splitlines()[-1] == f"claude --resume {before['c-main']}")
        assert until(lambda: (cwd('x-work') / 'argv.log').read_text().splitlines()[-1] == 'codex resume 01a1-thread')
        assert 'conversation_unknown' in cli('session', 'restore')['restored'][0]['error'], 'live runs are left alone'
        fresh = cli('session', 'restore', '--new')['restored']
        assert [o['name'] for o in fresh] == ['x-three'] and fresh[0]['error'] is None, fresh
        assert cli('session', 'get', 'x-three')['process'] == 'running'
        assert cli('session', 'restore')['restored'] == []
    case('restore resumes lost sessions, skips unknown conversations unless --new', restore)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
