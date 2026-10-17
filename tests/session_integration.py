#!/usr/bin/env python3
"""Name-addressed sessions (msg/Hermes surface): no starter-terminal identity leaks into panes, list/get/prompt/
read/wait/whoami, blocked sessions are never typed into or restarted, conversation IDs survive
restart and restore, and the SessionStart hook records Codex threads. Fake harnesses only."""
import argparse, json, os, pathlib, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); p.add_argument('--before-bin'); a = p.parse_args()
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
    later*) sleep 1; echo "the-text-you-wait-for"; echo "> " ;;
    many*) for i in $(seq 1 150); do echo "line-$i"; done; echo "> " ;;
    *) echo "got: $line"; echo "> " ;;
  esac
done
'''
for name in ('claude', 'codex', 'plain'):
    (bindir / name).write_text(FAKE); (bindir / name).chmod(0o755)
# Raw mode: nothing is buffered or echoed, so every byte a key sends shows as its hex value.
# It announces itself first: keys sent before the pane is raw land in the line buffer instead.
RAWKEYS = r'''#!/usr/bin/env python3
import os, sys, tty
tty.setraw(0)
sys.stdout.write('raw-ready\r\n'); sys.stdout.flush()
while True:
    b = os.read(0, 1)
    if not b: break
    sys.stdout.write('%02x|' % b[0]); sys.stdout.flush()
'''
(bindir / 'rawkeys').write_text(RAWKEYS); (bindir / 'rawkeys').chmod(0o755)
# The caller is a pane of a permission-carrying terminal app (config: [server] start_env) inside a
# Codex command: none of that identity may reach PCTL panes.
(home / '.config' / 'pctl').mkdir(parents=True, exist_ok=True)
(home / '.config' / 'pctl' / 'config.toml').write_text('[server]\nstart_env = "TERMAPP_ENV"\n')
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'XDG_CONFIG'))}
ENV.update(CODEX_HOME=str(home / '.codex'), HOME=str(home), TERMAPP_ENV='1', TERMAPP_PANE_ID='w9:p9', TERMAPP_SOCKET_PATH='/nonexistent', CODEX_THREAD_ID='leak')
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
    def explain():
        assert until(lambda: '>' in cli('session', 'read', 'p-shell'))
        def output(binary, *args):
            q = subprocess.run([binary, '--state', str(state), '--tmux', T, *args],
                               env=ENV, capture_output=True, check=True)
            return q.stdout
        before_list = output(B, 'session', 'list')
        before_get = output(B, 'session', 'get', 'p-shell')
        e = cli('session', 'explain', 'p-shell')
        assert e['status'] == 'idle' and e['rule'] == 'idle: no rule matched', e
        assert e['row'] is None and e['text'] is None and e['considered_rows'], e
        assert isinstance(e['pane_title'], str), e
        assert output(B, 'session', 'list') == before_list
        assert output(B, 'session', 'get', 'p-shell') == before_get
        if a.before_bin:
            assert output(str(pathlib.Path(a.before_bin).resolve()), 'session', 'list') == before_list
            assert output(str(pathlib.Path(a.before_bin).resolve()), 'session', 'get', 'p-shell') == before_get
    case('explain shares status and leaves list/get byte-identical on a fixed fixture (optional pre-change binary)', explain)
    def no_leak():
        env = cwd('p-shell') / 'env.txt'
        assert until(env.exists)
        text = env.read_text()
        leaked = [l.split('=')[0] for l in text.splitlines() if l.startswith('TERMAPP_') or l.startswith('CODEX_THREAD_ID=')]
        assert leaked == [], leaked
        assert 'PCTL_AGENT_ID=' in text
    case('PCTL panes carry no start_env-prefixed variables or CODEX_THREAD_ID from the server starter', no_leak)
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
        e = cli('session', 'explain', 'p-shell')
        assert e['status'] == 'blocked' and e['row'] > 0 and e['text'], e
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
        cli('session', 'add', '--name', 'c-moved', '--cwd', cwd('c-moved'), '--conversation', 'moved-convo-1', '--', bindir / 'claude')
        log = cwd('c-moved') / 'argv.log'
        assert until(lambda: log.exists() and log.read_text().strip())
        assert log.read_text().splitlines()[0] == 'claude --resume moved-convo-1'
        assert cli('session', 'get', 'c-moved')['conversation'] == 'moved-convo-1'
        assert 'needs a claude or codex' in cli('session', 'add', '--name', 'p-moved', '--cwd', cwd('p-moved'), '--conversation', 'x', '--', bindir / 'plain', ok=False)
    case('add --conversation starts a moved session in its existing conversation', moved)
    def start_stopped():
        assert 'has_run' in cli('session', 'start', 'c-moved', ok=False)
        s = cli('session', 'get', 'c-moved')
        cli('agent', 'terminate', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        assert cli('session', 'get', 'c-moved')['process'] == 'stopped'
        e = cli('session', 'explain', 'c-moved')
        assert e['status'] == 'stopped' and e['considered_rows'] == [] and e['pane_title'] is None, e
        v = cli('session', 'start', 'c-moved')
        assert v['process'] == 'running' and v['conversation'] == 'moved-convo-1', v
        log = cwd('c-moved') / 'argv.log'
        assert until(lambda: log.read_text().splitlines()[-1] == 'claude --resume moved-convo-1')
    case('start runs a stopped session in its saved conversation', start_stopped)
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
        day = home / '.codex/sessions/2026/10/15'; day.mkdir(parents=True, exist_ok=True)
        (day / 'rollout-01a1-thread.jsonl').write_text(json.dumps({'type': 'session_meta', 'payload': {'id': '01a1-thread', 'cwd': str(cwd('x-work'))}}) + '\n')
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
        (day / 'rollout-only-one.jsonl').write_text(meta('only-one'))
        assert cli('session', 'get', 'x-two')['conversation'] == 'only-one'
        cli('session', 'add', '--name', 'x-three', '--cwd', cwd('x-three'), '--', bindir / 'codex')
        meta3 = lambda i: json.dumps({'type': 'session_meta', 'payload': {'id': i, 'cwd': str(cwd('x-three'))}}) + '\n'
        (day / 'rollout-twin-a.jsonl').write_text(meta3('twin-a')); (day / 'rollout-twin-b.jsonl').write_text(meta3('twin-b'))
        assert cli('session', 'get', 'x-three')['conversation'] is None, 'two candidates: unknown'
    case('codex fallback: one rollout candidate is taken, two are left unknown', codex_fallback)
    def unknown_conversation():
        log3 = cwd('x-three') / 'argv.log'; assert until(log3.exists)
        assert 'conversation_unknown' in cli('session', 'restart', 'x-three', ok=False)
        assert len((cwd('x-three') / 'argv.log').read_text().splitlines()) == 1, 'not restarted'
    case('restart refuses a session whose conversation ID is unknown', unknown_conversation)
    def gate():
        outside = {k: v for k, v in ENV.items() if not k.startswith('TERMAPP_')}
        other = root / 's2'
        bare = root / 'bare-home'; bare.mkdir(exist_ok=True)  # no config: the terminal's variable means nothing
        for env in (outside, dict(ENV, HOME=str(bare))):
            q = subprocess.run([B, '--state', str(other), '--tmux', T, 'session', 'add', '--name', 'g', '--cwd', str(cwd('g')), '--', str(bindir / 'plain')], text=True, capture_output=True, timeout=30, env=env)
            assert q.returncode != 0 and json.loads(q.stderr)['error_code'] == 'server_start_not_allowed', q.stderr
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server created'
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
        err = json.loads(cli('session', 'restore', ok=False, env=outside))
        assert err['error_code'] == 'server_start_not_allowed', err
        assert subprocess.run([T, '-S', str(state / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server created'
        assert all(s['process'] == 'lost' for s in cli('session', 'list', env=outside)), 'refused restore keeps every run'
        e = cli('session', 'explain', 'p-shell')
        assert e['status'] == 'lost' and e['row'] is None, e
    case('without start_env set to 1 (or with no [server] start_env) no server is created: add and restore are refused', gate)
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
    def keys_reach_pane():
        cli('session', 'add', '--name', 'k-raw', '--cwd', cwd('k-raw'), '--project', 'demo', '--', bindir / 'rawkeys')
        assert until(lambda: 'raw-ready' in cli('session', 'read', 'k-raw', '--lines', '3')), 'the pane is raw before any key'
        v = cli('session', 'keys', 'k-raw', 'Escape', 'Up', '5')
        assert v['name'] == 'k-raw' and v['keys'] == ['Escape', 'Up', '5'] and v['was_blocked'] is False, v
        assert until(lambda: '35|' in cli('session', 'read', 'k-raw', '--lines', '3')), cli('session', 'read', 'k-raw', '--lines', '3')
        raw = cli('session', 'read', 'k-raw', '--lines', '3')
        arrow = '1b|4f|41|' if '1b|4f|41|' in raw else '1b|5b|41|'  # Up: application or normal cursor mode
        assert raw.strip().endswith('1b|' + arrow + '35|'), raw  # Escape, Up and the digit 5, in order, as their own bytes
    case('session keys presses Escape, Up and a digit: the bytes arrive in the pane', keys_reach_pane)
    def keys_refused():
        def err(*args):
            return json.loads(cli('session', 'keys', *args, ok=False))['error']
        before = cli('session', 'read', 'k-raw', '--lines', '3')
        for bad in ('F5', 'hello', 'esc', 'ctrl-c', ''):
            assert err('k-raw', bad).startswith('unsupported key'), err('k-raw', bad)
        assert 'unsupported key' in err('k-raw', 'Escape', 'F5'), 'one bad name refuses the whole call'
        assert 'at most 32 keys' in err('k-raw', *['Escape'] * 33)
        assert 'required arguments were not provided' in cli('session', 'keys', 'k-raw', ok=False), 'a call needs at least one key'
        assert cli('session', 'read', 'k-raw', '--lines', '3') == before, 'nothing was sent'
    case('an unknown key name is refused and nothing is sent; at most 32 keys', keys_refused)
    def keys_stale_run():
        s = cli('session', 'get', 'k-raw')
        sock = str(state / 'tmux.sock')
        cap = lambda: subprocess.run([T, '-S', sock, 'capture-pane', '-p', '-t', s['pane']], text=True, capture_output=True).stdout
        subprocess.run([T, '-S', sock, 'set-option', '-p', '-t', s['pane'], '@pctl_run', 'someone-else'], check=True)
        before = cap()
        e = json.loads(cli('session', 'keys', 'k-raw', 'Escape', ok=False))['error']
        assert 'controls locked' in e, e
        assert cap() == before, 'a key never lands in a pane whose run changed'
        subprocess.run([T, '-S', sock, 'set-option', '-p', '-t', s['pane'], '@pctl_run', s['run_id']], check=True)
        cli('session', 'keys', 'k-raw', 'n')
        assert until(lambda: '6e|' in cli('session', 'read', 'k-raw', '--lines', '3'))
    case('a pane whose run changed refuses keys; they work again once the run matches', keys_stale_run)
    def keys_blocked():
        cli('session', 'prompt', 'p-shell', 'block')
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'blocked', 3)
        v = cli('session', 'keys', 'p-shell', 'Down', 'Enter')
        assert v['was_blocked'] is True and v['status_before'] == 'blocked', v
        assert until(lambda: 'got:' in cli('session', 'read', 'p-shell', '--lines', '6')), 'the blocked pane read a line'
        e = json.loads(cli('session', 'prompt', 'p-shell', 'yes', ok=False))['error']
        assert e.startswith('blocked:'), e
        assert 'got: yes' not in cli('session', 'read', 'p-shell', '--lines', '6'), 'pasted text is still never an answer'
        s = cli('session', 'get', 'p-shell')
        cli('input', 'paste', '--agent', s['agent_id'], '--expected-run', s['run_id'], '--text', 'clear')
        cli('input', 'submit', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'idle')
    case('session keys answers a blocked session (was_blocked); session prompt still refuses it', keys_blocked)
    def wait_match():
        cli('session', 'prompt', 'p-shell', 'later')
        v = cli('session', 'wait', 'p-shell', '--match', 'the-text-you-wait-for', '--timeout-ms', '15000')
        assert v['matched'] == 'the-text-you-wait-for' and v['name'] == 'p-shell', v
        cli('session', 'prompt', 'p-shell', 'many')
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'idle', 5)
        vis = subprocess.run([T, '-S', str(state / 'tmux.sock'), 'capture-pane', '-p', '-t', cli('session', 'get', 'p-shell')['pane']], text=True, capture_output=True).stdout
        assert 'line-5' not in vis, 'line-5 is above the visible screen: only the scrollback can match it'
        v = cli('session', 'wait', 'p-shell', '--match', 'line-5\nline-6', '--timeout-ms', '15000')
        assert v['matched'] == 'line-5\nline-6', v
    case('wait --match returns when the text arrives, from the screen or the scrollback', wait_match)
    def wait_match_errors():
        e = json.loads(cli('session', 'wait', 'p-shell', '--match', 'never-shown-xyz', '--timeout-ms', '600', ok=False))['error']
        assert e.startswith('timeout:') and 'never-shown-xyz' in e, e
        e = cli('session', 'wait', 'p-shell', '--match', 'x', '--until', 'idle', '--timeout-ms', '600', ok=False)
        assert 'cannot be used with' in e, e
    case('wait --match times out with timeout:; --match with --until is a usage error', wait_match_errors)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
