#!/usr/bin/env python3
"""What the pctl app types into the agent on screen, byte for byte: keys read together go out as one tmux
call and still arrive in order, quotes and tmux format characters in the text stay text, a pane whose
run changed gets neither typing nor a paste (the person is told, nothing is left in tmux's buffers)."""
import argparse, json, os, pathlib, shutil, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-input-', dir=os.path.realpath('/tmp'))); state = root / 's'; V = root / 'v.sock'; log = root / 'typed'; results = []
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG'))}
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
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server'); st('kill-server'); shutil.rmtree(root, ignore_errors=True)
