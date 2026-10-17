#!/usr/bin/env python3
"""Codex resume validation and startup outcomes, using fake Codex and fake HOME trees only."""
import argparse, json, os, pathlib, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-resume-', dir=os.path.realpath('/tmp')))
state = root / 's'; home = root / 'home'; store = home / '.codex'; results = []; sockets = []
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'CODEX_', 'XDG_CONFIG', 'NO_COLOR'))}
ENV.update(HOME=str(home), PCTL_STANDALONE='1')
bindir = root / 'bin'; bindir.mkdir(); home.mkdir()
exe = bindir / 'codex'
exe.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys, time
cwd = pathlib.Path.cwd()
with (cwd / 'argv.log').open('a') as log: log.write(json.dumps(sys.argv[1:]) + '\\n')
if (cwd / 'fail').exists():
    time.sleep(.15)
    print('No saved session found: fake resume failure', flush=True)
    raise SystemExit(int((cwd / 'fail').read_text()))
print('ready >', flush=True)
for line in sys.stdin: print('got: ' + line.strip(), flush=True)
'''); exe.chmod(0o755)
def cli(*args, ok=True, env=ENV, st=state):
    if st not in sockets: sockets.append(st)
    q = subprocess.run([B, '--state', str(st), '--tmux', T, *map(str, args)], env=env, text=True, capture_output=True, timeout=30)
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stdout, q.stderr)
    return json.loads(q.stdout) if ok and q.stdout.strip() else q.stderr

def cwd(name):
    d = root / name; d.mkdir(exist_ok=True); return d

def add(name, *args, **kw):
    return cli('session', 'add', '--name', name, '--cwd', cwd(name), *args, '--', exe, **kw)

def rollout(id, name, base=store, archived=False, filename=None):
    day = base / ('archived_sessions' if archived else 'sessions/2026/10/17'); day.mkdir(parents=True, exist_ok=True)
    f = day / (filename or f'rollout-2026-10-17-{id}.jsonl')
    f.write_text(json.dumps({'type': 'session_meta', 'payload': {'id': id, 'cwd': str(cwd(name))}}) + '\n')
    return f

def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic()-t, 3)}); print('PASS', name, flush=True)

def terminate(name):
    s = cli('session', 'get', name)
    cli('agent', 'terminate', '--agent', s['agent_id'], '--expected-run', s['run_id'])

def stash(f): f.rename(root / ('held-' + f.name))
try:
    def pick():
        add('pick')
        wrong = rollout('invented', 'pick', filename='rollout-actual.jsonl')
        assert cli('session', 'get', 'pick')['conversation'] is None, 'filename/header mismatch cannot supply an ID'
        stash(wrong)
        f = rollout('verified', 'pick')
        assert cli('session', 'get', 'pick')['conversation'] == 'verified'
        audits = [json.loads(l) for l in (state / 'audit.jsonl').read_text().splitlines()]
        evidence = [json.loads(e['result']) for e in audits if e['action'] == 'codex_conversation'][-1]
        assert evidence['path'] == str(f) and evidence['conversation'] == 'verified' and evidence['cwd'] == str(cwd('pick'))
        assert evidence['source'] == 'cwd/time fallback' and evidence['run_started_at'] > 0
    case('first-turn pick verifies filename, header and cwd and records rollout evidence', pick)

    def missing():
        f = next((store / 'sessions/2026/10/17').glob('*-verified.jsonl')); stash(f)
        before = cli('session', 'get', 'pick')
        log = (cwd('pick') / 'argv.log').read_text()
        err = cli('session', 'restart', 'pick', ok=False)
        assert 'conversation_unknown' in err and '--new' in err and str(store) in err, err
        assert cli('session', 'get', 'pick')['run_id'] == before['run_id']
        assert (cwd('pick') / 'argv.log').read_text() == log, 'refusal never kills or launches'
        terminate('pick')
        assert 'conversation_unknown' in cli('session', 'start', 'pick', ok=False)
        assert cli('session', 'get', 'pick')['process'] == 'stopped'
        assert cli('session', 'start', 'pick', '--new')['process'] == 'running'
        assert json.loads((cwd('pick') / 'argv.log').read_text().splitlines()[-1]) == []
        assert cli('session', 'restart', 'pick', '--new')['process'] == 'running'
        assert json.loads((cwd('pick') / 'argv.log').read_text().splitlines()[-1]) == []
    case('missing rollout refuses restart before killing and start before launch; --new starts fresh', missing)

    def archive():
        rollout('archived', 'archive', archived=True)
        s = add('archive', '--conversation', 'archived')
        assert s['process'] == 'running'
        assert cli('session', 'restart', 'archive')['process'] == 'running'
        assert json.loads((cwd('archive') / 'argv.log').read_text().splitlines()[-1]) == ['resume', 'archived']
    case('saved archived rollout is accepted by add and restart', archive)

    def failed_launch():
        (cwd('archive') / 'fail').write_text('42')
        e = cli('session', 'restart', 'archive', ok=False)
        assert 'start_failed' in e and 'exit status 42' in e and 'No saved session found' in e, e
        assert cli('session', 'get', 'archive')['status'] == 'exited'
        terminate('archive')
        e = cli('session', 'start', 'archive', ok=False)
        assert 'start_failed' in e and 'exit status 42' in e and 'No saved session found' in e, e
        terminate('archive')
        (cwd('archive') / 'fail').write_text('0')
        e = cli('session', 'start', 'archive', '--new', ok=False)
        assert 'start_failed' in e and 'exit status 0' in e, e
    case('delayed early exit fails restart/start with exit status and tail, including exit zero', failed_launch)

    def hooks():
        s = add('hook')
        env = dict(ENV, PCTL_AGENT_ID=s['agent_id'], PCTL_RUN_ID=s['run_id'])
        def hook(id):
            q = subprocess.run([B, '--state', str(state), '--tmux', T, 'session', 'hook'], input=json.dumps({'session_id': id}), env=env, text=True, capture_output=True, check=True)
            assert q.stdout == '', 'hook stays silent'
        hook('missing-hook')
        assert cli('session', 'get', 'hook')['conversation'] is None
        f = rollout('wrong-cwd', 'elsewhere'); hook('wrong-cwd')
        assert cli('session', 'get', 'hook')['conversation'] is None
        stash(f)
        rollout('hook-good', 'hook'); hook('hook-good')
        assert cli('session', 'get', 'hook')['conversation'] == 'hook-good'
        audits = [json.loads(l) for l in (state / 'audit.jsonl').read_text().splitlines()]
        assert any(e['action'] == 'codex_conversation' and json.loads(e['result'])['source'] == 'session hook' for e in audits)
    case('hook cannot record an absent or wrong-cwd rollout; valid hook is auditable', hooks)

    def stale_and_dead():
        add('stale')
        f = rollout('old', 'stale'); os.utime(f, (0, 0))
        assert cli('session', 'get', 'stale')['conversation'] is None
        (cwd('dead') / 'fail').write_text('0')
        assert 'start_failed' in add('dead', ok=False)
        rollout('after-exit', 'dead')
        s = cli('session', 'get', 'dead')
        assert s['status'] == 'exited' and s['conversation'] is None, s
    case('stale files and files appearing after process exit cannot supply a first-turn ID', stale_and_dead)

    def custom_home():
        custom = root / 'custom'; other_state = root / 's2'; env = dict(ENV, CODEX_HOME=str(custom))
        rollout('custom-id', 'custom-agent', base=custom)
        v = add('custom-agent', '--conversation', 'custom-id', st=other_state, env=env)
        assert v['conversation'] == 'custom-id' and v['process'] == 'running'
        assert cli('session', 'restart', 'custom-agent', st=other_state, env=env)['process'] == 'running'
        e = add('absent', '--conversation', 'absent-id', ok=False)
        assert 'conversation_unknown' in e and 'absent' not in [s['name'] for s in cli('session', 'list')]
        e = cli('session', 'add', '--name', 'explicit', '--cwd', cwd('explicit'), '--', exe, 'resume', 'absent-id', ok=False)
        assert 'conversation_unknown' in e and 'explicit' not in [s['name'] for s in cli('session', 'list')]
        f = rollout('raw-id', 'raw')
        v = cli('session', 'add', '--name', 'raw', '--cwd', cwd('raw'), '--', exe, 'resume', 'raw-id')
        stash(f)
        e = cli('session', 'restart', 'raw', '--new', ok=False)
        assert 'conversation_unknown' in e and cli('session', 'get', 'raw')['run_id'] == v['run_id']

    case('CODEX_HOME selects isolated storage; absent explicit resume/add leaves no registration', custom_home)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=2))
finally:
    for st in sockets:
        subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
