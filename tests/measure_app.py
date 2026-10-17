#!/usr/bin/env python3
"""How responsive the pctl app (`pctl` alone) feels, in numbers. The app runs inside its own viewer
tmux, like tests/app_integration.py, on an isolated state with 24 registered agents; one of them is on
screen and runs a small program that echoes every key and, after `~`, prints a timestamped line
every 10 ms. Measured, from the viewer's screen (what a person would see):
  keystroke-to-echo   one key at a time, p50/p95/max over --keys keys; also while the agent streams
  burst               --burst keys sent at once (fast typing, key repeat): time until the last echo
  output staleness    age of the newest streamed line on screen, and screen updates per second
  app CPU             the app and the tmux processes it starts, idle and streaming (1.0 = one core)
  tmux calls          per second, by tmux command (the --tmux binary is a logging wrapper for this pass)
With --cells 4 the app shows four cells (2x2): a-main (key echo, focused), a-stream (streams), two idle agents.
Works on macOS (libproc) and Linux (/proc). Poll cost of the harness itself is reported as `poll_ms`:
no latency below it can be seen."""
import argparse, ctypes, json, os, pathlib, platform, re, shutil, statistics, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True)
p.add_argument('--agents', type=int, default=24); p.add_argument('--keys', type=int, default=60); p.add_argument('--burst', type=int, default=40)
p.add_argument('--seconds', type=int, default=20); p.add_argument('--label', default='')
p.add_argument('--cells', type=int, default=1, help='1 or 4: with 4 the app shows four cells; the key echo is measured in the focused one while another streams')
a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-perf-', dir=os.path.realpath('/tmp'))); state = root / 's'; V = root / 'v.sock'
bindir = root / 'bin'; bindir.mkdir(); calls = root / 'calls.log'
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG'))}
ENV.update(XDG_CONFIG_HOME=str(root / 'no-config'), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ', PCTL_STANDALONE='1')
# The agent on screen: "k<n> <key>" for the n-th key; `~` starts and `^` stops "s<n> <monotonic ns>" every 10 ms.
(bindir / 'echoer.py').write_text('''import os, select, sys, time, tty
tty.setcbreak(0); n = s = 0; stream = False; due = 0
print("ready", flush=True)
while True:
    # Spins while streaming: sleeping 10 ms comes back after 40 ms or more on a macOS background process.
    if select.select([0], [], [], 0 if stream else 1)[0]:
        c = os.read(0, 1).decode(errors="replace")
        if c in "~^": stream = c == "~"; due = time.monotonic(); continue
        n += 1; print(f"k{n} {c}", flush=True)
    if stream and time.monotonic() >= due: s += 1; due += 0.01; print(f"s{s} {time.monotonic_ns()}", flush=True)
''')
# Counts the tmux commands the app and its observer start (the first word after -S/-f).
(bindir / 'tmuxlog').write_text(f'#!/bin/sh\necho "$6" >> {calls}\nexec {T} "$@"\n'); (bindir / 'tmuxlog').chmod(0o755)

def cli(*args, tmux=T):
    q = subprocess.run([B, '--state', str(state), '--tmux', tmux, *map(str, args)], text=True, capture_output=True, timeout=60, env=ENV)
    assert q.returncode == 0, (args, q.stderr); return q.stdout
def vt(*args): return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
VIEW_COLS, SIDE = 140, 27  # the viewer's width, and the columns of the sidebar and its edge
# With four cells the focused cell (a-main) is the top left and the streaming one the top right.
REGIONS = {'echo': (SIDE, SIDE + (VIEW_COLS - SIDE - 1) // 2), 'stream': (SIDE + (VIEW_COLS - SIDE - 1) // 2 + 1, VIEW_COLS)}
def lines(region=None):
    rows = vt('capture-pane', '-p', '-t', 'view').split('\n')
    if a.cells > 1 and region:
        x0, x1 = REGIONS[region]; return [l[x0:x1].rstrip() for l in rows]
    return [l.split('│', 1)[-1].rstrip() for l in rows]  # the agent's part of each row
def until(fn, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.05)
    return fn()
def pct(xs, q): return round(sorted(xs)[min(len(xs) - 1, int(len(xs) * q))], 1)
def dist(xs): return {'n': len(xs), 'p50': pct(xs, 0.5), 'p95': pct(xs, 0.95), 'max': round(max(xs), 1)}

if platform.system() == 'Darwin':
    _lib = ctypes.CDLL('/usr/lib/libproc.dylib')
    class _RU(ctypes.Structure): _fields_ = [('uuid', ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in 'user system pkg intr pageins wired resident foot start exit cuser csys cpkg cintr cpageins celapsed'.split()]
    class _TB(ctypes.Structure): _fields_ = [('n', ctypes.c_uint32), ('d', ctypes.c_uint32)]
    _tb = _TB(); ctypes.CDLL('/usr/lib/libSystem.dylib').mach_timebase_info(ctypes.byref(_tb))
    def cpu(pid):
        """(own, reaped children) CPU seconds."""
        r = _RU(); assert _lib.proc_pid_rusage(int(pid), 1, ctypes.byref(r)) == 0, pid
        k = _tb.n / _tb.d / 1e9; return (r.user + r.system) * k, (r.cuser + r.csys) * k
else:
    def cpu(pid):
        f = pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split(); hz = os.sysconf('SC_CLK_TCK')
        return (int(f[11]) + int(f[12])) / hz, (int(f[13]) + int(f[14])) / hz
def procs(): return {k: cpu(v) for k, v in pids.items()}

try:
    # 1 echoer on screen first (project alpha), the rest idle `sleep`s spread over alpha and beta.
    for i in range(a.agents):
        proj = 'alpha' if i < a.agents // 2 else 'beta'; name = 'a-main' if i == 0 else f'{proj[0]}-w{i:02d}'
        d = root / 'proj' / proj / name; d.mkdir(parents=True)
        if a.cells > 1 and i == 1: name = 'a-stream'
        echoer = i == 0 or (a.cells > 1 and i == 1)
        cli('session', 'add', '--name', name, '--cwd', d, '--project', proj, '--', *(['python3', '-u', bindir / 'echoer.py'] if echoer else ['sleep', '3600']))
    st = lambda *x: subprocess.run([T, '-S', str(state / 'tmux.sock'), *map(str, x)], text=True, capture_output=True, timeout=10).stdout.strip()
    server_pid = st('display-message', '-p', '#{pid}')

    def launch(tmux):
        vt('kill-server')
        cmd = f"env -u TMUX {B} --state {state} --tmux {tmux}; echo EXITED $?; sleep 600"
        subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '140', '-y', '40', '-s', 'view', cmd], env=ENV, check=True, timeout=10)
        assert until(lambda: any(l.startswith(('ready', 'k', 's')) for l in lines())), '\n'.join(lines())
        if a.cells > 1: make_cells()
        shell = vt('display-message', '-p', '-t', 'view', '#{pane_pid}').strip()
        ps = subprocess.run(['ps', '-A', '-o', 'pid=,ppid=,command='], text=True, capture_output=True).stdout.splitlines()
        app = [l.split()[0] for l in ps if l.split()[1] == shell and B in l][0]
        return app, vt('display-message', '-p', '#{pid}').strip()

    def sidebar_click(name):
        rows = vt('capture-pane', '-p', '-t', 'view').split('\n')
        y = next(i for i, l in enumerate(rows) if name in l[:26]); x = rows[y].index(name)
        vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m'); time.sleep(0.5)
    def make_cells():
        """2x2: a-main top left (focused at the end), a-stream top right, two idle agents below."""
        vt('send-keys', '-t', 'view', 'C-b', '%'); time.sleep(0.4); sidebar_click('a-stream')
        vt('send-keys', '-t', 'view', 'C-b', '"'); time.sleep(0.4); sidebar_click('a-w02')
        vt('send-keys', '-t', 'view', 'C-b', 'Left'); time.sleep(0.4)
        vt('send-keys', '-t', 'view', 'C-b', '"'); time.sleep(0.4); sidebar_click('a-w03')
        vt('send-keys', '-t', 'view', 'C-b', 'Up'); time.sleep(0.4)
        seen = lambda r: any(l.startswith(('ready', 'k', 's')) for l in lines(r))  # the second pass starts with the agents already used
        assert until(lambda: seen('echo') and seen('stream')), '\n'.join(lines())
    keys = [0]  # keys the agent has echoed so far, across passes
    def keyseq(n, char='x'):
        """Send one key; seconds until its echo line is on screen."""
        vt('send-keys', '-t', 'view', '-l', char); t0 = time.monotonic()
        want = f'k{n} '
        while time.monotonic() - t0 < 5:
            if any(l.startswith(want) for l in lines('echo')): return (time.monotonic() - t0) * 1000
        raise AssertionError(f'no echo for key {n}\n' + '\n'.join(lines()))

    def run_pass(tmux):
        out = {}; app, view_server = launch(tmux); global pids; pids = {'app': app, 'agents_tmux': server_pid, 'viewer_tmux': view_server}
        t = time.monotonic(); [lines() for _ in range(30)]; out['poll_ms'] = round((time.monotonic() - t) / 30 * 1000, 1)
        time.sleep(2)

        def window(seconds, work=None):
            """CPU (cores) of the app, its reaped children (the tmux clients it starts) and both tmux servers, and tmux calls/s."""
            c0, t0, k0 = procs(), time.monotonic(), (len(calls.read_text().split()) if calls.exists() else 0)
            (work or (lambda: time.sleep(seconds)))(); dt = time.monotonic() - t0; c1 = procs()
            r = {'app': round((c1['app'][0] - c0['app'][0]) / dt, 3), 'app_tmux_clients': round((c1['app'][1] - c0['app'][1]) / dt, 3),
                 'agents_tmux_server': round((c1['agents_tmux'][0] - c0['agents_tmux'][0]) / dt, 3), 'seconds': round(dt, 1)}
            if calls.exists(): r['tmux_calls_per_s'] = round((len(calls.read_text().split()) - k0) / dt, 1)
            return r

        out['idle'] = window(a.seconds)
        # Typed keys, one at a time with a pause so each is its own event.
        ms = []
        for i in range(a.keys): keys[0] += 1; ms.append(keyseq(keys[0], 'abcdefghij'[i % 10])); time.sleep(0.15)
        out['key_echo_ms'] = dist(ms)
        # Burst: all at once, as fast typing or key repeat arrives.
        word = ''.join('abcdefghij'[i % 10] for i in range(a.burst)); vt('send-keys', '-t', 'view', '-l', word); t0 = time.monotonic(); keys[0] += a.burst
        assert until(lambda: any(l.startswith(f'k{keys[0]} ') for l in lines('echo')), 30)
        out['burst_ms'] = {'keys': a.burst, 'until_last_echo': round((time.monotonic() - t0) * 1000)}
        # Streaming: one line every 10 ms.
        if a.cells > 1:  # the stream starts in the other cell, then the focus comes back
            vt('send-keys', '-t', 'view', 'C-b', 'Right'); time.sleep(0.4)
            vt('send-keys', '-t', 'view', '-l', '~'); time.sleep(0.4)
            vt('send-keys', '-t', 'view', 'C-b', 'Left'); time.sleep(0.4)
        else: vt('send-keys', '-t', 'view', '-l', '~')
        assert until(lambda: any(re.match(r's\d+ \d+', l) for l in lines('stream')))
        stale, seen = [], []
        def sample():
            end = time.monotonic() + a.seconds; last = None
            while time.monotonic() < end:
                ss = [l.split() for l in lines('stream') if re.match(r's\d+ \d+$', l)]
                if ss:
                    s, ts = ss[-1]; now = time.monotonic_ns(); stale.append((now - int(ts)) / 1e6)
                    if s != last: seen.append(time.monotonic()); last = s
        out['stream'] = window(a.seconds, sample)
        out['output_staleness_ms'] = dist(stale); out['screen_updates_per_s'] = round((len(seen) - 1) / (seen[-1] - seen[0]), 1)
        # Keys typed while the agent streams.
        ms = []
        for i in range(a.keys // 2): keys[0] += 1; ms.append(keyseq(keys[0], 'abcdefghij'[i % 10])); time.sleep(0.15)
        out['key_echo_while_streaming_ms'] = dist(ms)
        if a.cells > 1:
            vt('send-keys', '-t', 'view', 'C-b', 'Right'); time.sleep(0.4)
        vt('send-keys', '-t', 'view', '-l', '^'); time.sleep(1)
        return out

    res = {'label': a.label, 'binary': B, 'pctl': subprocess.run([B, '--version'], text=True, capture_output=True).stdout.strip(), 'tmux': subprocess.run([T, '-V'], text=True, capture_output=True).stdout.strip(),
           'platform': platform.platform(), 'agents': a.agents, 'cells': a.cells, 'viewer': '140x40'}
    res['timing'] = run_pass(T)
    # Same again through the logging wrapper (it adds a shell start to every call, so only its counts count).
    res['calls'] = run_pass(str(bindir / 'tmuxlog'))
    from collections import Counter
    res['calls']['by_command'] = dict(Counter(calls.read_text().split()).most_common())
    out = pathlib.Path(a.report); out.parent.mkdir(parents=True, exist_ok=True); out.write_text(json.dumps(res, indent=2) + '\n'); print(json.dumps(res, indent=2))
finally:
    vt('kill-server'); subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True); shutil.rmtree(root, ignore_errors=True)
