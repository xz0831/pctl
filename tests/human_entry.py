#!/usr/bin/env python3
"""The human entry over real tmux, driven through a pseudo-terminal: `pctl` alone is a usage error
without a terminal (on one it opens the app: tests/app_integration.py); `pctl attach` without
--project opens nothing without a server and otherwise attaches to the most recently used project at
its last window; `attach --project` takes a name; tabs and the status
line carry static agent names without a background job, including a name ending in ';'; ^b s and ^b q
are bound; inside another tmux it refuses instead of nesting."""
import argparse, fcntl, json, os, pathlib, pty, struct, subprocess, tempfile, termios, threading, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-hu-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
bindir = root / 'bin'; bindir.mkdir()
(bindir / 'plain').write_text('#!/bin/bash\nif [ "$(basename "$PWD")" = b-block ]; then echo "Do you want to proceed?"; echo "❯ 1. Yes"; fi\necho "> "\nwhile IFS= read -r line; do echo "got: $line"; echo "> "; done\n')
(bindir / 'plain').chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX'))}
ENV.update(PCTL_STANDALONE='1', TERM='xterm-256color')
def cli(*args, ok=True, st=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=30, env=ENV)
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    return (json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout) if ok else q.stderr
def tm(*args, st=None):
    return subprocess.run([T, '-S', str((st or state) / 'tmux.sock'), *args], text=True, capture_output=True, timeout=10).stdout.strip()
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
class Tty:
    """Run pctl on a pseudo-terminal (160x40) and collect what it prints."""
    def __init__(self, *args, st=None, env=None):
        self.out = b''
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(B, [B, '--state', str(st or state), '--tmux', T, *map(str, args)], dict(ENV, **(env or {})))
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        threading.Thread(target=self._drain, daemon=True).start()
    def _drain(self):
        while True:
            try: chunk = os.read(self.fd, 65536)
            except OSError: return
            if not chunk: return
            self.out += chunk
    def wait(self, seconds=15):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid: return os.waitstatus_to_exitcode(status)
            time.sleep(0.05)
        os.kill(self.pid, 9); raise AssertionError('pctl did not exit')
def clients():
    rows = tm('list-clients', '-F', '#{client_name}\t#{@pctl_name}\t#{window_name}\t#{window_id}')
    return [r.split('\t') for r in rows.splitlines() if r]
def attached(tty):
    assert until(lambda: len(clients()) == 1), (clients(), tty.out[-300:])
    c = clients()[0]
    return c
def detach(tty, c):
    tm('detach-client', '-t', c[0])
    assert tty.wait() == 0, tty.out[-300:]
    assert until(lambda: not clients())
def add(name, project, folder=None):
    d = root / 'proj' / project / (folder or name); d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / 'plain')
try:
    other = root / 's2'
    def no_terminal():
        q = subprocess.run([B, '--state', str(other), '--tmux', T], text=True, capture_output=True, timeout=30, env=ENV, stdin=subprocess.DEVNULL)
        assert q.returncode == 2 and 'a command is required' in q.stderr, (q.returncode, q.stderr)
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0
        for old in [('ui',), ('sidebar', 'inspect'), ('session', 'marks')]:
            q = subprocess.run([B, '--state', str(other), *old], env=ENV, capture_output=True)
            assert q.returncode == 2 and not other.exists(), (old, q.stderr)
    case('without a terminal, pctl alone is the usage error and starts nothing', no_terminal)
    def no_server():
        t = Tty('attach', st=other)
        assert t.wait() == 2 and b'no PCTL server is running' in t.out, t.out
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server started'
    case('attach without a server explains and starts none', no_server)
    s = {n: add(n, p, f) for n, p, f in [('w-a1', 'alpha', None), ('w-a2', 'alpha', 'b-block'), ('w-semi;', 'alpha', 'semi'), ('w-b1', 'beta', None)]}
    for v in s.values(): v['window'] = tm('display-message', '-p', '-t', v['pane'], '#{window_id}')
    assert until(lambda: cli('session', 'get', 'w-a2')['status'] == 'blocked')
    def names():
        for n, v in s.items():
            assert tm('show-options', '-wqv', '-t', v['pane'], '@pctl_tab') == n, (n, tm('show-options', '-w', '-t', v['pane']))
        projects = {tm('show-options', '-qv', '-t', v['pane'], '@pctl_name') for v in s.values()}
        assert projects == {'alpha', 'beta'}, projects
        assert 'marks' not in tm('show-options', '-gv', 'status-right')
        assert ':manager' not in tm('list-keys', '-T', 'prefix')
        assert tm('show-options', '-sv', 'exit-empty') == 'off'
        assert tm('show-options', '-gv', 'status-interval') == '5'
        keys = tm('list-keys', '-T', 'prefix')
        assert any(' s ' in k and 'choose-tree' in k for k in keys.splitlines()), keys
        assert any(' q ' in k and 'detach-client' in k for k in keys.splitlines()), keys
    case('agent tabs and project sessions carry names (a trailing ";" too); static chrome and ^b s, ^b q are set', names)
    def by_name():
        assert 'project not found' in cli('attach', '--project', 'nope', ok=False)
        t = Tty('attach', '--project', 'beta'); c = attached(t)
        assert c[1] == 'beta' and c[3] == s['w-b1']['window'], c
        detach(t, c)
    case('attach --project takes a project name and opens its verified agent window', by_name)
    def alone():
        time.sleep(1.1)  # session_last_attached has one-second resolution
        t = Tty('attach', '--project', 'alpha'); c = attached(t)
        win = tm('display-message', '-p', '-t', s['w-a1']['pane'], '#{window_id}')
        tm('select-window', '-t', win)
        assert until(lambda: clients()[0][3] == win)
        detach(t, c)
        time.sleep(1.1)  # session_last_attached has one-second resolution
        t = Tty('attach'); c = attached(t)
        assert c[1] == 'alpha' and c[3] == win, ('most recent project, last window', c)
        detach(t, c)
        time.sleep(1.1)
        t = Tty('attach', '--project', 'beta'); detach(t, attached(t))
        time.sleep(1.1)
        t = Tty('attach'); c = attached(t)
        assert c[1] == 'beta', c
        detach(t, c)
    case('attach without --project goes to the most recently used project at its last window', alone)
    def nested():
        for args in [('attach',), ('attach', '--project', 'beta')]:
            t = Tty(*args, env={'TMUX': str(root / 'elsewhere.sock') + ',1,0'})
            assert t.wait() == 2 and b'already inside another tmux' in t.out, t.out
            assert not clients()
    case('inside another tmux it refuses instead of nesting', nested)
    def legacy_upgrade():
        # Legacy panes and metadata are test fixtures, never production state.
        session = s['w-a1']['pane']
        sid = tm('display-message', '-p', '-t', session, '#{session_id}')
        old = tm('new-window', '-d', '-t', sid, '-n', 'manager', '-P', '-F', '#{pane_id}', 'exec sleep 300')
        oldpid = tm('display-message', '-p', '-t', old, '#{pane_pid}')
        tm('bind-key', 'm', 'select-window', '-t', ':manager')
        job = '#(' + ' '.join("'" + x.replace("'", "'\\''") + "'" for x in ['/old/pctl', '--state', str(state), '--tmux', T, 'session', 'marks']) + ')'
        tm('set-option', '-g', 'status-right', 'custom #(date) ' + job)
        tm('select-window', '-t', old)
        before = {n: cli('session', 'get', n)['run_id'] for n in s}
        t = Tty('attach', '--project', 'alpha'); c = attached(t)
        assert c[3] in {v['window'] for n,v in s.items() if n != 'w-b1'} and c[3] != tm('display-message', '-p', '-t', old, '#{window_id}'), c
        detach(t, c)
        assert tm('display-message', '-p', '-t', old, '#{pane_pid}') == oldpid, 'legacy pane survived'
        assert {n: cli('session', 'get', n)['run_id'] for n in s} == before
        assert ':manager' not in tm('list-keys', '-T', 'prefix')
        assert tm('show-options', '-gv', 'status-right') == 'custom #(date)'
        tm('bind-key', 'm', 'display-message', 'custom-menu')
        tm('set-option', '-g', 'status-right', '#(echo custom)')
        t = Tty('attach', '--project', 'alpha'); detach(t, attached(t))
        assert 'custom-menu' in tm('list-keys', '-T', 'prefix', 'm')
        assert tm('show-options', '-gv', 'status-right') == '#(echo custom)'
        add('w-z1', 'zeta')
        assert 'custom-menu' in tm('list-keys', '-T', 'prefix', 'm')
        assert tm('show-options', '-gv', 'status-right') == '#(echo custom)', 'a new project does not reset custom chrome'
    case('upgrade leaves legacy panes and runs alive, avoids manager, retires only owned routes and preserves custom ones', legacy_upgrade)
    def foreign_server():
        marker = json.loads((state / 'registry.json').read_text())['server_instance']
        tm('bind-key', 'm', 'select-window', '-t', ':manager')
        before = tm('list-keys', '-T', 'prefix', 'm')
        status = tm('show-options', '-gv', 'status-right')
        tm('set-option', '-g', '@pctl_instance', 'foreign')
        try:
            t = Tty('attach', '--project', 'alpha')
            assert t.wait() != 0 and b'controls locked' in t.out, t.out
            assert tm('list-keys', '-T', 'prefix', 'm') == before
            assert tm('show-options', '-gv', 'status-right') == status
        finally:
            tm('set-option', '-g', '@pctl_instance', marker)
        t = Tty('attach', '--project', 'alpha'); detach(t, attached(t))
        assert ':manager' not in tm('list-keys', '-T', 'prefix')
    case('upgrade refuses a foreign server without changing its binding or status', foreign_server)
    def legacy_sidebar():
        v = s['w-a1']
        profile = cli('agent', 'add', '--project', cli('project', 'list')[0]['id'], '--name', 'old-nav-fixture', '--cwd', root, '--', '/bin/sleep', '300')
        run = cli('agent', 'start', '--agent', profile['id'])
        reg = json.loads((state / 'registry.json').read_text())
        reg['runs'] = [r for r in reg['runs'] if r['id'] != run['id']]
        (state / 'registry.json').write_text(json.dumps(reg))  # disposable legacy fixture only
        rec = {'id': 'legacy-nav', 'server_instance': reg['server_instance'], 'pane': run['pane'], 'pid': run['pid'], 'process_start': run['process_start']}
        (state / 'sidebar.json').write_text(json.dumps(rec))
        for key,value in [('@pctl_agent',''),('@pctl_sidebar',rec['id']),('@pctl_sidebar_instance',reg['server_instance'])]: tm('set-option','-p','-t',run['pane'],key,value)
        tm('join-pane','-d','-h','-s',run['pane'],'-t',v['pane'])
        before = tm('display-message','-p','-t',v['pane'],'#{window_id}')
        cli('layout','split','--agent',v['agent_id'],'--expected-run',v['run_id'])
        assert tm('display-message','-p','-t',v['pane'],'#{window_id}') == before, 'verified legacy navigation is not a second agent'
        assert tm('display-message','-p','-t',run['pane'],'#{pane_pid}') == run['pid']
        tm('set-option','-p','-t',run['pane'],'@pctl_sidebar','someone-else')
        cli('layout','split','--agent',v['agent_id'],'--expected-run',v['run_id'])
        assert tm('display-message','-p','-t',v['pane'],'#{window_id}') != before, 'unverified ordinary pane is not excluded'
        assert tm('display-message','-p','-t',run['pane'],'#{pane_pid}') == run['pid']
    case('native split excludes only a positively identified legacy sidebar and never kills its process', legacy_sidebar)
    def empty_project():
        p = cli('project', 'add', '--name', 'empty', '--root', root)
        t = Tty('attach', '--project', p['id'])
        assert t.wait() != 0 and b'no agent window' in t.out, t.out
        assert p['id'] not in tm('list-sessions', '-F', '#{@pctl_project}').splitlines()
    case('attach to an empty project refuses without creating a placeholder', empty_project)
    def last_agent():
        lone = root / 'lone'
        try:
            v = cli('session', 'add', '--name', 'only', '--cwd', root, '--', bindir / 'plain', st=lone)
            assert tm('list-panes', '-a', '-F', '#{pane_id}', st=lone) == v['pane']
            assert tm('display-message', '-p', '-t', v['pane'], '#{history_limit}', st=lone) == '20000'
            cli('agent', 'terminate', '--agent', v['agent_id'], '--expected-run', v['run_id'], st=lone)
            assert cli('agent', 'list', st=lone)['connected']
            assert cli('session', 'get', 'only', st=lone)['process'] == 'stopped'
            assert cli('session', 'start', 'only', st=lone)['process'] == 'running'
            q = Tty('attach', '--project', cli('project', 'list', st=lone)[0]['id'], st=lone)
            assert until(lambda: tm('list-clients', st=lone))
            client = tm('list-clients', '-F', '#{client_name}', st=lone).splitlines()[0]
            tm('detach-client', '-t', client, st=lone); assert q.wait() == 0
        finally:
            subprocess.run([T, '-S', str(lone / 'tmux.sock'), 'kill-server'], capture_output=True)
    case('first agent is the only first window; terminating the last agent keeps the server usable for start', last_agent)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
