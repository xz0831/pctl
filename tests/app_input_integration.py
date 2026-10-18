#!/usr/bin/env python3
"""What the pctl app types into the agent on screen, byte for byte: keys read together go out as one tmux
call and still arrive in order, quotes and tmux format characters in the text stay text, a pane whose
run changed gets neither typing nor a paste (the person is told, nothing is left in tmux's buffers)."""
import argparse, json, os, pathlib, shutil, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-input-', dir=os.path.realpath('/tmp'))); state = root / 's'; V = root / 'v.sock'; log = root / 'typed'; results = []
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'NO_COLOR'))}
ENV.update(XDG_CONFIG_HOME=str(root / 'no-config'), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ', PCTL_STANDALONE='1')
def vt(*args): return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
def st(*args): return subprocess.run([T, '-S', str(state / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout.strip()
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def typed(): return log.read_bytes().decode() if log.exists() else ''
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
try:
    d = root / 'proj'; d.mkdir()
    q = subprocess.run([B, '--state', str(state), '--tmux', T, 'session', 'add', '--name', 'r-main', '--cwd', d, '--project', 'alpha', '--', 'bash', '-c', f'stty raw -echo; exec cat > {log}'], text=True, capture_output=True, env=ENV, timeout=60)
    assert q.returncode == 0, q.stderr
    run = json.loads(subprocess.run([B, '--state', str(state), '--tmux', T, 'session', 'get', 'r-main'], text=True, capture_output=True, env=ENV).stdout)
    pane, run_id = run['pane'], run['run_id']
    # Full-screen programs like today's agents: one reads the mouse (SGR), one does not.
    for name, mouse in (('r-mouse', '\x1b[?1000h\x1b[?1006h'), ('r-keys', '')):
        prog = root / f'{name}.py'
        prog.write_text(f"""import os, sys, tty
sys.stdout.write('\\x1b[?1049h{mouse}READY-{name}\\r\\n'); sys.stdout.flush(); tty.setraw(0)
f = open({str(root / name)!r}, 'ab', buffering=0)
while True:
    b = os.read(0, 1024)
    if not b: break
    f.write(b)
""")
        q = subprocess.run([B, '--state', str(state), '--tmux', T, 'session', 'add', '--name', name, '--cwd', d, '--project', 'alpha', '--', 'python3', prog], text=True, capture_output=True, env=ENV, timeout=60)
        assert q.returncode == 0, q.stderr
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', f"env -u TMUX {B} --state {state} --tmux {T}; sleep 600"], env=ENV, check=True, timeout=10)
    assert until(lambda: '+ agent' in vt('capture-pane', '-p', '-t', 'view')), vt('capture-pane', '-p', '-t', 'view')
    time.sleep(1)
    def tricky():
        text = "it's \"q\" #{pane_id} #(echo x) %1 $HOME a;b; \\ 한글 ; end"
        vt('send-keys', '-t', 'view', '-l', text); vt('send-keys', '-t', 'view', '-l', '\\;'); text += ';'  # a lone ";" is escaped for the viewer's own tmux
        vt('send-keys', '-t', 'view', 'Enter'); vt('send-keys', '-t', 'view', '-l', 'tail')
        assert until(lambda: typed() == text + '\r' + 'tail'), repr(typed())
    case('quotes, format characters and semicolons in typed text arrive as typed, in order with Enter', tricky)
    def burst():
        before = len(typed()); word = ''.join(chr(97 + i % 26) for i in range(300))
        vt('send-keys', '-t', 'view', '-l', word)
        assert until(lambda: typed()[before:] == word), typed()[before:]
    case('a long run of keys arrives whole', burst)
    def paste_between_keys():
        # One write, 3 KB bracketed paste between two keys: crossterm's epoll reader would hand over the first key and keep the rest until the next input.
        before = len(typed()); body = ''.join(chr(97 + i % 26) for i in range(3000))
        vt('send-keys', '-t', 'view', '-l', '\x1b[200~' + body + '\x1b[201~'); time.sleep(0.3)
        vt('send-keys', '-t', 'view', '-l', 'a' + '\x1b[200~' + body + '\x1b[201~' + 'b')
        assert until(lambda: typed()[before:] == body + 'a' + body + 'b', 1), len(typed()[before:])
    case('a 3 KB bracketed paste, alone or between two keys, arrives whole within 1 s with no more input', paste_between_keys)
    def changed():
        before = typed(); st('set-option', '-p', '-t', pane, '@pctl_run', 'someone-else')
        vt('send-keys', '-t', 'view', '-l', 'nope')
        assert until(lambda: 'r-main changed (restarted or moved); not typing' in vt('capture-pane', '-p', '-t', 'view')), vt('capture-pane', '-p', '-t', 'view')
        big = root / 'paste.txt'; big.write_text('pasted text\n' * 3); vt('load-buffer', '-b', 'big', big); vt('paste-buffer', '-p', '-b', 'big', '-t', 'view'); time.sleep(1)
        assert typed() == before, 'nothing typed or pasted into a pane whose run changed'
        assert 'pctl-app-' not in st('list-buffers'), st('list-buffers')
        st('set-option', '-p', '-t', pane, '@pctl_run', run_id)
        assert until(lambda: 'r-main idle' in vt('capture-pane', '-p', '-t', 'view').split('\n')[25]), 'the list catches up'
        vt('send-keys', '-t', 'view', '-l', 'back')
        assert until(lambda: typed() == before + 'back'), repr(typed()[len(before):])
    case('a pane whose run changed gets no typing and no paste, the person is told, and it works again once the run matches', changed)
    def got(name): return (root / name).read_bytes() if (root / name).exists() else b''
    def open_agent(name):
        rows = vt('capture-pane', '-p', '-t', 'view').split('\n')
        y = next(i for i, l in enumerate(rows) if name in l[:26]); x = rows[y].index(name)
        vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m')
        assert until(lambda: f'READY-{name}' in vt('capture-pane', '-p', '-t', 'view') or name == 'r-main'), vt('capture-pane', '-p', '-t', 'view')
    def wheel(up=True): vt('send-keys', '-t', 'view', '-l', f'\x1b[<{64 if up else 65};70;12M')
    def wheel_to_mouse():
        assert until(lambda: 'r-mouse' in vt('capture-pane', '-p', '-t', 'view')), 'the list shows the new agents'
        open_agent('r-mouse'); time.sleep(0.5); wheel(); wheel(False)
        assert until(lambda: got('r-mouse').count(b'M') >= 2), got('r-mouse')
        b = got('r-mouse'); assert b.startswith(b'\x1b[<64;') and b'\x1b[<65;' in b, b
    case('the wheel over a full-screen program that reads the mouse reaches it as SGR wheel events', wheel_to_mouse)
    def wheel_to_keys():
        open_agent('r-keys'); time.sleep(0.5); wheel()
        assert until(lambda: len(got('r-keys')) >= 9), got('r-keys')
        assert got('r-keys') in (b'\x1b[A' * 3, b'\x1bOA' * 3), got('r-keys')
    case('the wheel over a full-screen program without the mouse reaches it as three arrow keys', wheel_to_keys)
    def wheel_scrolls_history():
        open_agent('r-main'); time.sleep(0.5); before = typed(); wheel(); wheel(False); time.sleep(1)
        assert typed() == before, 'the wheel over an ordinary screen scrolls tmux history and types nothing'
    case('the wheel over an ordinary screen still scrolls history and types nothing', wheel_scrolls_history)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server'); st('kill-server'); shutil.rmtree(root, ignore_errors=True)
