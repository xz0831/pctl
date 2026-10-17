#!/usr/bin/env python3
"""Screens over tmux control mode. The bridge (and the app, for cells on this computer) keep one
`tmux -C` client per session, read a pane only after it wrote, and fall back to a process per read
when the client dies. Counted through a tmux wrapper that logs every process start. Also: the
control client is never taken for a person (clients list, refresh dry-run, window-size, `pctl
attach` from inside tmux, the detach key, typing by session prompt/keys/tell). Fake agents only."""
import argparse, json, os, pathlib, queue, shutil, signal, subprocess, tempfile, threading, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-ctl-', dir=os.path.realpath('/tmp')))
state, bindir, log, V = root / 's', root / 'bin', root / 'tmux.log', root / 'v.sock'
bindir.mkdir(); results = []
W = bindir / 'tmuxlog'  # the tmux pctl runs: logs every start, then is tmux
W.write_text(f'#!/bin/sh\necho "$@" >> {log}\nexec {T} "$@"\n'); W.chmod(0o755)
(bindir / 'echo').write_text('#!/bin/bash\necho "ready in $(basename "$PWD")"\nwhile IFS= read -r line; do echo "got: $line"; done\n')
(bindir / 'flood').write_text('#!/bin/bash\nwhile :; do echo flood flood flood flood flood; done\n')
(bindir / 'blank').write_text('#!/bin/bash\nsleep 600\n')
for n in ('echo', 'flood', 'blank'): (bindir / n).chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG'))}
ENV.update(XDG_CONFIG_HOME=str(root / 'no-config'), TERM='xterm-256color', PCTL_STANDALONE='1', PCTL_CONTROL_RETRY_SECS='2', PCTL_CONTROL_IDLE_SECS='3')
def cli(*args, ok=True, tmux=T, extra=None):
    q = subprocess.run([B, '--state', str(state), '--tmux', tmux, *map(str, args)], text=True, capture_output=True, timeout=60, env=dict(ENV, **(extra or {})))
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    if not ok: return q.stderr
    return json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout
def tm(*args): return subprocess.run([T, '-S', str(state / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout.strip()
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.05)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def starts(word=None):
    lines = log.read_text().splitlines() if log.exists() else []
    return len([l for l in lines if word is None or word in l])
def controls(): return [l for l in tm('list-clients', '-F', '#{client_control_mode} #{client_pid} #{client_flags}').splitlines() if l.startswith('1 ')]
def add(name, project, prog='echo'):
    d = root / 'proj' / project / name; d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / prog)
class Bridge:
    def __init__(self, extra=None):
        env = dict(ENV, PCTL_STATE=str(state), **(extra or {}))
        self.p = subprocess.Popen([B, '--state', str(state), '--tmux', str(W), 'bridge'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1, env=env)
        self.q = queue.Queue(); self.kept = []; self.n = 0; self.t = threading.Thread(target=lambda: [self.q.put((time.monotonic(), json.loads(l))) for l in self.p.stdout], daemon=True); self.t.start()
        assert self.take(lambda v: v.get('ev') == 'hello')
    def send(self, op): self.n += 1; self.p.stdin.write(json.dumps(dict(id=self.n, **op)) + '\n'); self.p.stdin.flush(); return self.n
    def take(self, pred, within=5):
        """The first line that fits, older lines first; the ones passed over stay for the next take."""
        end = time.monotonic() + within
        while True:
            for i, (t, v) in enumerate(self.kept):
                if pred(v): del self.kept[i]; return t, v
            if time.monotonic() >= end: return None
            try: self.kept.append(self.q.get(timeout=0.05))
            except queue.Empty: pass
    def ask(self, op): i = self.send(op); return self.take(lambda v: v.get('id') == i)[1]
    def drain(self, within=0.3, limit=2.0):
        end = time.monotonic() + limit
        while time.monotonic() < end and self.take(lambda v: True, within): pass
        self.kept.clear()
    def watch(self, cells): return self.ask({'op': 'watch', 'cells': cells})
    def close(self): self.p.stdin.close(); self.p.wait(timeout=10)
def cell(s, key='1', rev=1, w=80, h=24): return {'key': key, 'rev': rev, 'agent': s['name'], 'run': s['run_id'], 'width': w, 'height': h, 'scroll': 0}
def screen_with(b, text, within=2): return b.take(lambda v: v.get('ev') == 'screen' and text in v['rows'], within)
try:
    S = {n: add(n, p, prog) for n, p, prog in (('q1', 'p', 'echo'), ('q2', 'p', 'echo'), ('fl2', 'p', 'flood'), ('o1', 'o', 'echo'))}
    for n in ('q1', 'q2', 'o1'): assert until(lambda: 'ready in' in tm('capture-pane', '-p', '-t', S[n]['pane'])), n
    b = Bridge()
    def after_output():
        assert b.watch([cell(S['q1'])])['watching'] == 1
        assert b.take(lambda v: v.get('ev') == 'screen' and 'ready in q1' in v['rows'])
        assert until(lambda: len(controls()) == 1, 8), tm('list-clients')
        assert 'ignore-size' in controls()[0], controls()
        time.sleep(1.2); b.drain(); n0, c0 = starts(), starts('capture-pane -p -e -N')
        time.sleep(2.0); assert starts() == n0, f'no tmux process starts while idle: {log.read_text().splitlines()[n0:]}'
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'send-keys', '-t', S['q1']['pane'], '-l', 'zebra'])
        t0 = time.monotonic(); ev = screen_with(b, 'zebra')
        assert ev and ev[0] - t0 < 0.5, 'a change comes right after the output'
        assert starts('capture-pane -p -e -N') == c0, 'and was read over the connection: no capture-pane process'
    case('a watched cell is read after its pane wrote, over the control connection: no process start while idle or per change', after_output)
    def not_a_viewer():
        assert cli('client', 'list') == [], cli('client', 'list')
        pane = S['q1']['pane']
        assert tm('show-options', '-wv', '-t', pane, 'window-size') == '' and tm('display-message', '-p', '-t', pane, '#{pane_width}x#{pane_height}') == '80x24', 'the cell asked for its size and window-size is handed back'
        row = lambda v: next(s for pr in v['projects'] for s in pr['sessions'] if s['name'] == 'q1')
        with_ctl = cli('session', 'refresh', '--idle', '--dry-run'); expl = cli('session', 'explain', 'q1')
        assert not any('raw_client_attached' in r for r in row(with_ctl)['reasons']), row(with_ctl)
        b.close()
        assert until(lambda: len(controls()) == 0, 8), 'the control client leaves with the bridge'
        without = cli('session', 'refresh', '--idle', '--dry-run')
        keep = lambda v: [(pr['name'], pr['eligible'], sorted(pr['reasons']), [(s['name'], s['reasons']) for s in pr['sessions']]) for pr in v['projects']]
        assert keep(with_ctl) == keep(without), (keep(with_ctl), keep(without))
        assert cli('session', 'explain', 'q1') == expl
    case('the control client is no viewer: client list, window-size, refresh dry-run and explain are the same with and without it', not_a_viewer)
    def typing_with_control():
        b2 = Bridge(); b2.watch([cell(S['q1'])]); assert until(lambda: len(controls()) == 1, 8)
        cli('session', 'prompt', 'q1', 'prompted words'); assert until(lambda: any('got:' in l and 'prompted words' in l for l in cli('session', 'read', 'q1', '--lines', '10').splitlines())), cli('session', 'read', 'q1', '--lines', '10')
        cli('session', 'keys', 'q1', '7', 'Enter'); assert until(lambda: 'got: 7' in cli('session', 'read', 'q1', '--lines', '10'))
        t = cli('tell', 'q1', 'told words'); assert t['result'] in ('reached', 'queued', 'typed_unconfirmed'), t
        assert until(lambda: 'told words' in cli('session', 'read', 'q1', '--lines', '10'), 12)
        assert 'words' not in cli('session', 'read', 'q2', '--lines', '10'), 'nothing reached the other pane'
        assert screen_with(b2, 'prompted words', 3)
        b2.close(); assert until(lambda: len(controls()) == 0, 8)
    case('session prompt, session keys and tell land in the right pane with a control client attached', typing_with_control)
    def person():
        b3 = Bridge(); b3.watch([cell(S['q1']), cell(S['o1'], key='2')]); assert until(lambda: len(controls()) == 2, 10), controls()
        outer = root / 'outer.sock'
        sess_p = tm('display-message', '-p', '-t', S['q1']['pane'], '#{session_name}'); sess_o = tm('display-message', '-p', '-t', S['o1']['pane'], '#{session_name}')
        subprocess.run([T, '-S', str(outer), '-f', '/dev/null', 'new-session', '-d', '-x', '100', '-y', '30', '-s', 'o', f"env -u TMUX {T} -S {state / 'tmux.sock'} attach -t {sess_p}"], env=ENV, check=True)
        try:
            people = lambda: [l.split() for l in tm('list-clients', '-F', '#{client_control_mode} #{client_name} #{client_session}').splitlines() if l.startswith('0 ')]
            assert until(lambda: len(people()) == 1, 8)
            name = people()[0][1]
            ctl_before = sorted(c.split()[1] for c in controls())
            # pctl attach from inside that tmux: the person's client switches, not the control client
            inside = dict(ENV, TMUX=f"{state / 'tmux.sock'},1,0")
            q = subprocess.run([B, '--state', str(state), '--tmux', T, 'attach', '--project', 'o'], env=inside, capture_output=True, text=True, timeout=20)
            assert q.returncode == 0, q.stderr
            assert until(lambda: people() and people()[0][2] == sess_o, 5), (people(), sess_o)
            sess_of_ctl = sorted(l.split()[1] for l in tm('list-clients', '-F', '#{client_control_mode} #{client_session}').splitlines() if l.startswith('1 '))
            assert sess_of_ctl == sorted([sess_p, sess_o]), ('each control client stays on its own session', sess_of_ctl)
            # the workspace key ^b q detaches the person, not a control client
            if 'detach-client' not in tm('list-keys', '-T', 'prefix', 'q'): tm('bind-key', 'q', 'detach-client')  # the theme's binding, when no theme was applied
            subprocess.run([T, '-S', str(outer), 'send-keys', '-t', 'o', 'C-b', 'q'])
            assert until(lambda: len(people()) == 0, 5), people()
            assert len(controls()) == 2, 'both control clients stay'
        finally:
            subprocess.run([T, '-S', str(outer), 'kill-server'], capture_output=True)
        b3.close()
    case('pctl attach from inside tmux and the detach key act on the person\'s client, never on a control client', person)
    def keys_over_control():
        b7 = Bridge(); b7.watch([cell(S['q2'])]); assert until(lambda: len(controls()) == 1, 8)
        time.sleep(1.2); b7.drain(); c0 = starts()
        r = b7.ask({'op': 'keys', 'agent': 'q2', 'run': S['q2']['run_id'], 'keys': [{'literal': True, 'key': "it's;ok"}, {'literal': False, 'key': 'Enter'}]})
        assert r['ok'], r
        assert b7.take(lambda v: v.get('ev') == 'screen' and any('got:' in l and "it's;ok" in l for l in v['rows'].split('\n')), 2), 'quotes and semicolons arrive as typed'
        assert starts() == c0, 'typing over the connection starts no tmux process'
        pane = S['q2']['pane']; tm('set-option', '-p', '-t', pane, '@pctl_run', 'someone-else')
        r = b7.ask({'op': 'keys', 'agent': 'q2', 'run': S['q2']['run_id'], 'keys': [{'literal': True, 'key': 'never'}, {'literal': False, 'key': 'Enter'}]})
        assert r['ok'] is False and r['error'].startswith('changed:'), r
        time.sleep(0.4); assert 'never' not in tm('capture-pane', '-p', '-t', pane), 'nothing typed into a pane whose run changed'
        tm('set-option', '-p', '-t', pane, '@pctl_run', S['q2']['run_id'])
        assert b7.ask({'op': 'keys', 'agent': 'q2', 'run': S['q2']['run_id'], 'keys': [{'literal': True, 'key': 'back'}]})['ok']
        b7.close()
    case('keys typed over the control connection arrive as typed, and a changed run still gets nothing', keys_over_control)
    def killed():
        b4 = Bridge(); b4.watch([cell(S['q1'])]); assert until(lambda: len(controls()) == 1, 8)
        b4.take(lambda v: v.get('ev') == 'screen'); time.sleep(1.2); b4.drain()
        pid = int(controls()[0].split()[1]); os.kill(pid, signal.SIGKILL)
        assert until(lambda: len(controls()) == 0, 5)
        c0 = starts('capture-pane -p -e -N')
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'send-keys', '-t', S['q1']['pane'], '-l', 'fallback'])
        assert screen_with(b4, 'fallback', 3), 'still updated, by polling'
        assert starts('capture-pane -p -e -N') > c0, 'with process reads'
        assert until(lambda: len(controls()) == 1, 8), 'a new client after the wait'
        b4.drain(1.5); c1 = starts('capture-pane -p -e -N')
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'send-keys', '-t', S['q1']['pane'], '-l', 'again'])
        assert screen_with(b4, 'again', 2) and starts('capture-pane -p -e -N') == c1, 'and back to reading over the connection'
        logged = (state / 'tmux-control.log').read_text().splitlines()
        assert len(logged) == 1 and 'ended' in logged[0], logged
        b4.close()
    case('a killed control client falls back to process reads (one log line, no loop) and a new one takes over', killed)
    def chatty():
        b5 = Bridge(); b5.watch([cell(S['fl2'], key='f'), cell(S['q2'], key='q')]); assert until(lambda: len(controls()) >= 1, 8)
        time.sleep(1.5); b5.drain()
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'send-keys', '-t', S['q2']['pane'], '-l', 'quiet one'])
        t0 = time.monotonic(); ev = b5.take(lambda v: v.get('ev') == 'screen' and v['key'] == 'q' and 'quiet one' in v['rows'], 2)
        assert ev and ev[0] - t0 < 0.4, 'the quiet pane is not held up by the flooding one'
        n = 0; end = time.monotonic() + 2
        while time.monotonic() < end:
            if b5.take(lambda v: v.get('ev') == 'screen' and v['key'] == 'f', 0.2): n += 1
        assert 3 <= n <= 70, f'the flooding pane is pushed at a limited rate: {n} screens in 2 s'
        b5.close()
    case('a pane that prints all the time neither delays a quiet one nor floods the connection', chatty)
    def sessions_bounded():
        b6 = Bridge(); b6.watch([cell(S['q1']), cell(S['o1'], key='2')]); assert until(lambda: len(controls()) == 2, 10)
        b6.watch([cell(S['q1'])]); b6.drain()
        assert until(lambda: len(controls()) == 1, 12), 'the client of a session without a watched cell is detached'
        b6.close()
    case('a control client is detached once its session has no watched cell for a while', sessions_bounded)
    def app_cell():
        subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', f"env -u TMUX PCTL_CONTROL_IDLE_SECS=60 {B} --state {state} --tmux {W}; sleep 600"], env=ENV, check=True)
        vt = lambda *x: subprocess.run([T, '-S', str(V), *map(str, x)], text=True, capture_output=True).stdout
        scr = lambda: vt('capture-pane', '-p', '-t', 'view')
        assert until(lambda: '+ agent' in scr(), 10), scr()
        rows = scr().split('\n'); y = next(i for i, l in enumerate(rows) if ' q2' in l[:26]); x = rows[y].index('q2')
        vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m')
        assert until(lambda: 'ready in q2' in scr(), 10), scr()
        assert until(lambda: len(controls()) >= 1, 8), 'the app attached a control client for the shown cell'
        time.sleep(1.5); c0 = starts('capture-pane -p -e -N'); time.sleep(2.5)
        assert starts('capture-pane -p -e -N') == c0, 'an idle cell is not polled'
        vt('send-keys', '-t', 'view', '-l', 'typed in app'); vt('send-keys', '-t', 'view', 'Enter')
        assert until(lambda: any('got:' in l and 'typed in app' in l for l in scr().split('\n')), 5), scr()
        assert starts('capture-pane -p -e -N') == c0, 'and the echo came without a process read'
        vt('kill-server')
        assert until(lambda: len(controls()) == 0, 8), 'the app takes its control clients with it'
    case('a cell of the app is read after its pane wrote, with no polling; the control clients go with the app', app_cell)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    subprocess.run([T, '-S', str(V), 'kill-server'], capture_output=True)
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
