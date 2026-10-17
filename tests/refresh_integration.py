#!/usr/bin/env python3
"""Installed build observations and explicit Claude swaps; fake harnesses/private HOME only."""
import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--bin', required=True)
p.add_argument('--tmux', required=True)
p.add_argument('--report', required=True)
a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve())
T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-refresh-', dir=os.path.realpath('/tmp')))
state = root / 's'
home = root / 'home'
home.mkdir()
env = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'CODEX_', 'XDG_'))}
env.update(HOME=str(home), PCTL_STANDALONE='1', NO_COLOR='1')
results = []
FAKE = '''#!/bin/bash
printf '%s\\n' "$*" >> launches
printf '> \\n'
while IFS= read -r line; do
 case "$line" in
 work) printf '\\033[2J\\033[H✻ Thinking… (3s · esc to interrupt)\\n' ;;
 block) printf '\\033[2J\\033[HDo you want to proceed?\\n❯ 1. Yes\\n' ;;
 clear) printf '\\033[2J\\033[H> \\n' ;;
 esac
done
'''

def write(path, text=FAKE):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)
    return path

v1 = write(root / 'v1' / 'claude')
v2 = write(root / 'v2' / 'claude', FAKE + '# version two\n')
link = root / 'bin' / 'claude'
link.parent.mkdir()
link.symlink_to(v1)
(link.parent / 'python3').symlink_to(sys.executable)
env['PATH'] = str(link.parent) + ':/usr/bin:/bin'

def cli(*args, ok=True, tmux=T, extra=None):
    e = dict(env, **(extra or {}))
    q = subprocess.run([B, '--state', str(state), '--tmux', tmux, *map(str, args)],
                       env=e, capture_output=True, text=True, timeout=30)
    assert (q.returncode == 0) == ok, (args, q.stdout, q.stderr)
    return json.loads(q.stdout) if ok else json.loads(q.stderr)['error']

def tm(*args):
    return subprocess.run([T, '-S', str(state / 'tmux.sock'), *args], capture_output=True, text=True, check=True).stdout

def get(name='fresh'):
    return cli('session', 'get', name)

def until(fn):
    end = time.monotonic() + 5
    while time.monotonic() < end:
        if fn(): return
        time.sleep(.05)
    assert fn()

def case(name, fn):
    start = time.monotonic()
    fn()
    results.append(dict(test=name, passed=True, seconds=round(time.monotonic()-start, 3)))
    print('PASS', name, flush=True)

def add(name='fresh', command='claude'):
    cwd = root / name
    cwd.mkdir(exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', cwd, '--', command)

def retarget(target):
    new = link.with_name('next')
    new.symlink_to(target)
    new.replace(link)

try:
    def identity():
        s = add()
        assert s['stale'] is False and s['running_build'] == s['installed_build'], s
        assert s['running_build']['resolved_path'] == str(v1), s
        assert s['running_build']['interpreter']['resolved_path'] == os.path.realpath('/bin/bash'), s  # /bin is a symlink to /usr/bin on many Linux systems
        retarget(v2)
        s = get()
        assert s['stale'] is True and not s['swap_blockers'], s
        assert s['running_build']['resolved_path'] == str(v1)
        assert s['installed_build']['resolved_path'] == str(v2)
        assert cli('session', 'list')[0]['stale'] is True
        assert cli('session', 'explain', 'fresh')['stale'] is True
    case('launch identity, symlink retarget, list/get/explain agree', identity)

    def launcher_path():
        wrong = write(root / 'wrong' / 'claude', '#!/bin/sh\nexit 66\n')
        altered = cli('session', 'get', 'fresh', extra={'PATH': str(wrong.parent) + ':/usr/bin:/bin'})
        assert altered['installed_build']['resolved_path'] == str(v2), altered
        wrapped_link = root / 'wrapped-bin' / 'claude'
        wrapped_link.parent.mkdir()
        wrapped_link.symlink_to(v1)
        cwd = root / 'wrapped'
        cwd.mkdir()
        before = cli('session', 'add', '--name', 'wrapped', '--cwd', cwd,
                     '--', 'env', 'PATH=' + str(wrapped_link.parent) + ':/usr/bin:/bin', 'claude')
        assert before['running_build']['resolved_path'] == str(v1), before
        next_link = wrapped_link.with_name('next')
        next_link.symlink_to(v2)
        next_link.replace(wrapped_link)
        receipt = cli('session', 'refresh', 'wrapped')
        assert receipt['after']['conversation'] == before['conversation']
        assert receipt['after']['running_build']['resolved_path'] == str(v2)
    case('server PATH beats client PATH; env PATH override resolves and refreshes its own command', launcher_path)

    def session_path_after_stop():
        custom = root / 'session-bin' / 'claude'
        custom.parent.mkdir()
        custom.symlink_to(v1)
        before = add('session-path')
        session = next(r['session'] for r in json.loads((state / 'registry.json').read_text())['runs'] if r['id'] == before['run_id'])
        tm('new-window', '-d', '-t', session, '/bin/sleep 60')
        tm('set-environment', '-t', session, 'PATH', str(custom.parent) + ':/usr/bin:/bin')
        cli('agent', 'terminate', '--agent', before['agent_id'], '--expected-run', before['run_id'])
        stopped = get('session-path')
        assert stopped['installed_build']['resolved_path'] == str(v1), stopped
        started = cli('session', 'start', 'session-path')
        assert started['running_build']['resolved_path'] == str(v1), started
        assert started['conversation'] == before['conversation']
    case('session-local PATH remains authoritative with only a retained window and no run', session_path_after_stop)

    def swap():
        before = get()
        receipt = cli('session', 'refresh', 'fresh')
        after = get()
        assert after['conversation'] == before['conversation']
        assert after['run_id'] != before['run_id'] and after['stale'] is False
        assert receipt['outcome'] == 'refreshed' and receipt['before']['run_id'] == before['run_id']
        log = [json.loads(l) for l in (state / 'refresh.jsonl').read_text().splitlines()]
        assert log[-2]['outcome'] == 'prepared' and log[-1] == receipt
        assert '--resume ' + before['conversation'] in (root / 'fresh' / 'launches').read_text()
        assert 'already_current' in cli('session', 'refresh', 'fresh', ok=False)
    case('explicit swap changes run, preserves conversation, logs receipt', swap)

    def overwrite():
        write(v2, FAKE + '# replaced in place; metadata must differ\n')
        assert get()['stale'] is True
    case('same executable path replaced in place is stale', overwrite)

    def refuse_status():
        before = get()['run_id']
        for command, status in [('work', 'working'), ('block', 'blocked')]:
            cli('session', 'prompt', 'fresh', command)
            until(lambda: get()['status'] == status)
            assert 'status:' + status in cli('session', 'refresh', 'fresh', ok=False)
            assert get()['run_id'] == before
            tm('send-keys', '-t', get()['pane'], 'clear', 'Enter')
            until(lambda: get()['status'] == 'idle')
    case('working and blocked refusals keep original run', refuse_status)

    def protected():
        before = get()['run_id']
        assert cli('session', 'protect', 'fresh')['protected']
        assert get()['protected'] and 'protected' in cli('session', 'refresh', 'fresh', ok=False)
        assert get()['run_id'] == before
        assert not cli('session', 'protect', 'fresh', '--off')['protected']
    case('persistent protection and --off', protected)

    def unknown():
        # Simulate a legacy registry: old runs lack launch identities and a conversation.
        file = state / 'registry.json'
        saved = file.read_text()
        r = json.loads(saved)
        r['agents'][0].pop('conversation')
        file.write_text(json.dumps(r))
        try:
            assert get()['stale'] is True
            assert 'conversation_unknown' in cli('session', 'refresh', 'fresh', ok=False)
            next(run for run in r['runs'] if run['agent_id'] == r['agents'][0]['id']).pop('running_build')
            file.write_text(json.dumps(r))
            s = get()
            assert s['stale'] == 'unknown' and s['running_build'] is None
            error = cli('session', 'refresh', 'fresh', ok=False)
            assert 'conversation_unknown' in error and 'build_unknown' in error
        finally:
            file.write_text(saved)
    case('legacy missing build/conversation are unknown and refuse', unknown)

    def unverified():
        for name in ['codex', 'opencode']:
            path = write(root / 'bin' / name)
            s = add(name, str(path))
            write(path, FAKE + '# new\n')
            assert 'harness_not_verified' in cli('session', 'refresh', name, ok=False)
            assert get(name)['run_id'] == s['run_id']
    case('Codex and OpenCode stay manual-only', unverified)

    def race():
        count = root / 'captures'
        shim = write(root / 'tmux-race', '''#!/usr/bin/env python3
import os, pathlib, subprocess, sys, time
args = sys.argv[1:]
f = pathlib.Path(os.environ['COUNT'])
if 'capture-pane' in args:
 n = int(f.read_text()) + 1 if f.exists() else 1
 f.write_text(str(n))
 if n == 2:
  subprocess.run([os.environ['REAL_TMUX'], '-S', os.environ['SOCKET'], 'send-keys', '-t', os.environ['PANE'], 'work', 'Enter'], check=True)
  time.sleep(.2)
os.execv(os.environ['REAL_TMUX'], [os.environ['REAL_TMUX'], *args])
''')
        before = get()
        e = cli('session', 'refresh', 'fresh', ok=False, tmux=str(shim),
                extra=dict(COUNT=str(count), REAL_TMUX=T, SOCKET=str(state / 'tmux.sock'), PANE=before['pane']))
        assert 'status:working' in e and get()['run_id'] == before['run_id'], e
        tm('send-keys', '-t', before['pane'], 'clear', 'Enter')
        until(lambda: get()['status'] == 'idle')
    case('status changes between checks: old run survives', race)

    def unknown_status():
        shim = write(root / 'tmux-unknown', "#!/bin/sh\nfor arg; do [ \"$arg\" = capture-pane ] && exit 1; done\nexec " + T + " \"$@\"\n")
        before = get()['run_id']
        error = cli('session', 'refresh', 'fresh', ok=False, tmux=str(shim))
        assert 'status:unknown' in error and get()['run_id'] == before
    case('unreadable screen means unknown and refuses', unknown_status)

    def missing_installed():
        before = get()['run_id']
        retarget(root / 'absent' / 'claude')
        try:
            assert get()['installed_build'] is None and get()['stale'] == 'unknown'
            assert 'build_unknown' in cli('session', 'refresh', 'fresh', ok=False)
            assert get()['run_id'] == before
        finally:
            retarget(v2)
    case('missing installed executable is unknown; live run is kept', missing_installed)

    def busy_receipt():
        import fcntl
        before = get()['run_id']
        with (state / 'refresh.jsonl').open('a') as f:
            fcntl.flock(f, fcntl.LOCK_EX)
            start = time.monotonic()
            error = cli('session', 'refresh', 'fresh', ok=False)
            assert 'receipt log busy' in error and time.monotonic() - start < 5, error
            assert get()['run_id'] == before
    case('busy receipt log refuses promptly before stopping', busy_receipt)

    def failure():
        bad = write(root / 'bad' / 'claude', '#!/bin/bash\necho resume-fixture-failed\nexit 23\n')
        retarget(bad)
        before = get()
        error = cli('session', 'refresh', 'fresh', ok=False)
        after = get()
        assert 'exit status 23' in error and 'resume-fixture-failed' in error, error
        assert after['process'] == 'exited' and after['run_id'] != before['run_id']
        assert after['conversation'] == before['conversation']
        log = [json.loads(l) for l in (state / 'refresh.jsonl').read_text().splitlines()]
        assert log[-1]['outcome'] == 'failed' and log[-1]['after']['process'] == 'exited'
        assert 'resume-fixture-failed' not in json.dumps(log[-1]), 'screen tails are not persisted in metadata receipts'
    case('startup failure leaves dead pane visible, logs exit and last lines, no retry', failure)

    def surviving_process():
        import signal
        fake = write(root / 'stubborn' / 'claude', "#!/usr/bin/env python3\nimport signal,time\nsignal.signal(signal.SIGHUP, signal.SIG_IGN)\nprint('> ', flush=True)\nwhile True: time.sleep(1)\n")
        before = add('stubborn', str(fake))
        registry = json.loads((state / 'registry.json').read_text())
        pid = int(next(r['pid'] for r in registry['runs'] if r['id'] == before['run_id']))
        try:
            with fake.open('a') as f: f.write('# replacement\n')
            error = cli('session', 'refresh', 'stubborn', ok=False)
            assert 'old_process_survived' in error, error
            assert get('stubborn')['run_id'] == before['run_id']
            assert get('stubborn')['process'] == 'lost'
        finally:
            try: os.kill(pid, signal.SIGTERM)
            except ProcessLookupError: pass
    case('old process survives pane removal: no replacement is launched', surviving_process)

    def slow_exit():
        # Claude Code needs ~1.7 s after SIGHUP before it exits.
        slow = write(root / 'slow' / 'claude', "#!/usr/bin/env python3\nimport signal,sys,time\nsignal.signal(signal.SIGHUP, lambda *_: (time.sleep(1.7), sys.exit(0)))\nprint('> ', flush=True)\nwhile True: time.sleep(1)\n")
        before = add('slow', str(slow))
        after = cli('session', 'restart', 'slow')
        assert after['run_id'] != before['run_id'] and get('slow')['process'] == 'running', after
    case('old process that takes 1.7 s to exit is waited for and replaced', slow_exit)

    pathlib.Path(a.report).write_text(json.dumps(dict(root=str(root), passed=len(results), results=results), indent=2))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
