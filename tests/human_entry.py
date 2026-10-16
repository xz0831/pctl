#!/usr/bin/env python3
"""The human entry over real tmux, driven through a pseudo-terminal: `pctl` alone is a usage error
without a terminal (on one it opens the app: tests/app_integration.py); `pctl attach` without
--project opens nothing without a server and otherwise attaches to the most recently used project at
its last window; `attach --project` takes a name; tabs and the status
line carry agent names and states (`session marks`), including a name ending in ';'; ^b s and ^b q
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
    case('without a terminal, pctl alone is the usage error and starts nothing', no_terminal)
    def no_server():
        t = Tty('attach', st=other)
        assert t.wait() == 2 and b'no PCTL server is running' in t.out, t.out
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server started'
    case('attach without a server explains and starts none', no_server)
    s = {n: add(n, p, f) for n, p, f in [('w-a1', 'alpha', None), ('w-a2', 'alpha', 'b-block'), ('w-semi;', 'alpha', 'semi'), ('w-b1', 'beta', None)]}
    assert until(lambda: cli('session', 'get', 'w-a2')['status'] == 'blocked')
    def names():
        for n, v in s.items():
            assert tm('show-options', '-wqv', '-t', v['pane'], '@pctl_tab') == n, (n, tm('show-options', '-w', '-t', v['pane']))
        projects = {tm('show-options', '-qv', '-t', v['pane'], '@pctl_name') for v in s.values()}
        assert projects == {'alpha', 'beta'}, projects
        assert 'session' in tm('show-options', '-gv', 'status-right') and 'marks' in tm('show-options', '-gv', 'status-right')
        assert tm('show-options', '-gv', 'status-interval') == '5'
        keys = tm('list-keys', '-T', 'prefix')
        assert any(' s ' in k and 'choose-tree' in k for k in keys.splitlines()), keys
        assert any(' q ' in k and 'detach-client' in k for k in keys.splitlines()), keys
    case('agent tabs and project sessions carry names (a trailing ";" too); status job and ^b s, ^b q are set', names)
    def marks():
        out = cli('session', 'marks')
        assert out.strip() == '! 1 blocked', out
        st = {n: tm('show-options', '-wqv', '-t', v['pane'], '@pctl_status') for n, v in s.items()}
        assert st == {'w-a1': 'idle', 'w-a2': 'blocked', 'w-semi;': 'idle', 'w-b1': 'idle'}, st
        assert tm('show-options', '-wqv', '-t', s['w-semi;']['pane'], '@pctl_tab') == 'w-semi;'
    case('session marks writes each tab state and prints the totals', marks)
    def by_name():
        assert 'project not found' in cli('attach', '--project', 'nope', ok=False)
        t = Tty('attach', '--project', 'beta'); c = attached(t)
        assert c[1] == 'beta' and c[2] == 'manager', c
        detach(t, c)
    case('attach --project takes a project name and opens its manager', by_name)
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
        t = Tty('attach', env={'TMUX': str(root / 'elsewhere.sock') + ',1,0'})
        assert t.wait() == 2 and b'already inside another tmux' in t.out, t.out
        assert not clients()
    case('inside another tmux it refuses instead of nesting', nested)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
