#!/usr/bin/env python3
"""`pctl session list` reads every agent's screen and title in one tmux call per 16 agents (the app's
observer runs it every 1.5 s). Same answer as asking one agent at a time (`session get`), also with more
than 16 agents; a pane that cannot be read fails the batch, and then each agent is asked on its own, so
one gone pane is `unknown` and never blanks the others. Stopped agents cost no call."""
import argparse, json, os, pathlib, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-obs-', dir=os.path.realpath('/tmp'))); state = root / 's'; log = root / 'calls.log'; results = []
wrap = root / 'tmuxwrap'
# Counts the tmux command of every call (6th word on). With BAD set, a capture-pane that names that pane fails as tmux does for a gone pane.
wrap.write_text(f'''#!/bin/bash
echo "$6" >> {log}
if [ -n "$BAD" ] && [ "$6" = capture-pane ]; then for a in "$@"; do if [ "$a" = "$BAD" ]; then echo "can't find pane: $BAD" >&2; exit 1; fi; done; fi
exec {T} "$@"
''')
wrap.chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'XDG_CONFIG', 'BAD'))}; ENV['PCTL_STANDALONE'] = '1'
def cli(*args, env=None):
    q = subprocess.run([B, '--state', str(state), '--tmux', str(wrap), *map(str, args)], text=True, capture_output=True, timeout=60, env=env or ENV)
    assert q.returncode == 0, (args, q.stderr); return json.loads(q.stdout)
def calls(): return log.read_text().split() if log.exists() else []
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
SHOW = {'idle': 'echo "> "', 'working': 'echo "✻ Thinking… (3s · esc to interrupt)"', 'blocked': 'echo "Do you want to proceed?"; echo "❯ 1. Yes"'}
try:
    kinds = {}
    for i in range(20):
        kind = ['working', 'blocked', 'idle', 'idle'][i % 4]; name = f'w{i:02d}'; kinds[name] = kind
        d = root / 'proj' / name; d.mkdir(parents=True)
        cli('session', 'add', '--name', name, '--cwd', d, '--project', 'alpha', '--', 'bash', '-c', f'{SHOW[kind]}; sleep 3600')
    s = cli('session', 'get', 'w02')
    cli('agent', 'terminate', '--agent', s['agent_id'], '--expected-run', s['run_id']); kinds['w02'] = 'stopped'
    def settled(): return {x['name']: x['status'] for x in cli('session', 'list')} == kinds
    end = time.monotonic() + 15
    while time.monotonic() < end and not settled(): time.sleep(0.2)
    assert settled(), {x['name']: x['status'] for x in cli('session', 'list')}
    def same():
        got = cli('session', 'list')
        assert got == [cli('session', 'get', x['name']) for x in got], 'list equals asking one by one'
        assert {x['status'] for x in got} == {'working', 'blocked', 'idle', 'stopped'}
    case('session list equals session get for every agent (19 running: two batches), stopped one included', same)
    def few_calls():
        n = len(calls()); cli('session', 'list'); made = calls()[n:]
        assert made.count('capture-pane') == 2, made   # 19 running agents: 16 + 3
        assert made.count('display-message') == 0, made
        assert len(made) <= 6, made
    case('one capture call per 16 running agents, no call per agent', few_calls)
    def one_gone():
        want = {x['name']: x['status'] for x in cli('session', 'list')}
        bad = cli('session', 'get', 'w05')['pane']
        got = {x['name']: x['status'] for x in cli('session', 'list', env=dict(ENV, BAD=bad))}
        assert got.pop('w05') == 'unknown'; want.pop('w05')
        assert got == want, 'the other agents keep their status'
    case('a pane that cannot be read is unknown; the others are not blanked', one_gone)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
    import shutil; shutil.rmtree(root, ignore_errors=True)
