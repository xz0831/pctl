#!/usr/bin/env python3
"""The document system: `pctl view` (the mdv reader inside pctl) with pctl's clipboard seam and a
question that goes to the project's main through `tell`. Fake harnesses only; the reader runs in a
private tmux server of its own."""
import argparse, json, os, pathlib, shlex, shutil, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-doc-', dir=os.path.realpath('/tmp')))
state, home, bindir = root / 'state', root / 'home', root / 'bin'
for d in (home, bindir): d.mkdir()
results = []
(bindir / 'agent').write_text(r'''#!/bin/bash
echo "ready in $(basename "$PWD")"
echo "> "
while IFS= read -r line; do
  case "$line" in
    *BLOCK*) echo "Do you want to proceed?"; echo "❯ 1. Yes" ;;
    *) echo "got: $line"; echo "> " ;;
  esac
done
''')
(bindir / 'agent').chmod(0o755)
BASE = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'XPC_', 'CODEX_', 'SSH_', 'MDV_'))}
BASE.update(HOME=str(home), PATH=f'{bindir}:{os.environ["PATH"]}', TERM='xterm-256color', SHELL='/bin/bash',
            PCTL_STANDALONE='1', PCTL_TMUX=T, MDV_THREADS_FILE=str(root / 'threads.jsonl'))
def run(*args, env=None, cwd=None):
    q = subprocess.run([B, '--state', str(state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=120, env=dict(BASE, **(env or {})), cwd=cwd)
    return q.returncode, q.stdout, q.stderr
def cli(*args, env=None):
    rc, out, err = run(*args, env=env); assert rc == 0, (args, rc, err, out)
    return json.loads(out) if out.strip().startswith(('{', '[')) else out
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def screen(name): return run('session', 'read', name, '--lines', '30')[1]

# A reader in a tmux server of its own (a short socket path), the way an app cell will run it.
rsock = root / 'r.sock'
def reader(*args, env=None):
    words = ['env', '-i', *[f'{k}={v}' for k, v in dict(BASE, **(env or {})).items()], B, '--state', state, '--tmux', T, 'view', *args]
    subprocess.run([T, '-S', str(rsock), '-f', '/dev/null', 'new-session', '-d', '-x', '100', '-y', '30',
                    ' '.join(shlex.quote(str(w)) for w in words) + '; echo EXIT=$?; sleep 60'], check=True, env=BASE)
def view_screen(): return subprocess.run([T, '-S', str(rsock), 'capture-pane', '-p'], text=True, capture_output=True).stdout
def view_keys(*keys): subprocess.run([T, '-S', str(rsock), 'send-keys', *keys], check=True)
def view_text(text): subprocess.run([T, '-S', str(rsock), 'send-keys', '-l', text], check=True)
def view_stop(): subprocess.run([T, '-S', str(rsock), 'kill-server'], capture_output=True); rsock.unlink(missing_ok=True)

proj = home / 'Projects' / 'demo'; proj.mkdir(parents=True)
doc = proj / 'note.md'
doc.write_text('# 제목\n\n복사할 한글 문장입니다.\n\n```\nlet x = 1;\n```\n')
try:
    def view_opens_and_quits():
        reader(doc, env={'PCTL_CLIP_WRITE_TEXT': 'cat > ' + str(root / 'clip.txt')})
        assert until(lambda: '복사할 한글 문장입니다.' in view_screen()), view_screen()
        view_keys('q')
        assert until(lambda: 'EXIT=0' in view_screen()), view_screen()
        view_stop()
    case('pctl view shows a file and quits cleanly', view_opens_and_quits)

    def copy_through_the_seam():
        (root / 'clip.txt').unlink(missing_ok=True)
        reader(doc, env={'PCTL_CLIP_WRITE_TEXT': 'cat > ' + str(root / 'clip.txt')})
        assert until(lambda: '복사할 한글 문장입니다.' in view_screen())
        view_keys('Tab', 'Tab'); view_keys('y')
        assert until(lambda: (root / 'clip.txt').exists() and (root / 'clip.txt').read_text().strip()), view_screen()
        assert '복사할 한글 문장입니다.' in (root / 'clip.txt').read_text(), (root / 'clip.txt').read_text()
        assert until(lambda: 'Copied' in view_screen() and 'paste to verify' not in view_screen()), view_screen()
        view_stop()
    case('copy goes through pctl\'s clipboard command and reports Copied', copy_through_the_seam)

    def copy_over_ssh_is_not_claimed():
        reader(doc, env={'SSH_CONNECTION': '10.0.0.1 1 10.0.0.2 22'})
        assert until(lambda: '복사할 한글 문장입니다.' in view_screen())
        view_keys('Tab', 'Tab'); view_keys('y')
        assert until(lambda: 'paste to verify' in view_screen()), view_screen()
        view_stop()
    case('over ssh no local command runs and the result says "paste to verify"', copy_over_ssh_is_not_claimed)

    def ask_goes_to_project_main():
        cli('session', 'add', '--name', 'demo-main', '--cwd', proj, '--project', 'demo', '--', bindir / 'agent')
        assert until(lambda: 'ready in' in screen('demo-main'))
        reader(doc)
        assert until(lambda: '복사할 한글 문장입니다.' in view_screen())
        view_keys('Tab', 'Tab', 'Enter')
        assert until(lambda: 'Ask the agent' in view_screen()), view_screen()
        view_keys('Down', 'Down', 'Down', 'Down', 'Enter')
        assert until(lambda: 'Enter send' in view_screen()), view_screen()
        view_text('why this sentence')
        view_keys('Enter')
        assert until(lambda: 'mdv 질문' in screen('demo-main') and 'why this sentence' in screen('demo-main')), screen('demo-main')
        assert 'show --answer' in screen('demo-main'), screen('demo-main')
        assert until(lambda: any(w in view_screen() for w in ('Reached', 'Queued', 'Not confirmed'))), view_screen()
        view_stop()
    case('a question reaches the project main through tell (fake session)', ask_goes_to_project_main)
    def ask_to_blocked_main_is_saved_not_sent():
        cli('session', 'prompt', 'demo-main', 'BLOCK')
        assert until(lambda: cli('session', 'get', 'demo-main')['status'] == 'blocked')
        before = screen('demo-main')
        reader(doc)
        assert until(lambda: '복사할 한글 문장입니다.' in view_screen())
        view_keys('Tab', 'Tab', 'Enter')
        assert until(lambda: 'Ask the agent' in view_screen())
        view_keys('Down', 'Down', 'Down', 'Down', 'Enter')
        assert until(lambda: 'Enter send' in view_screen())
        view_text('second question'); view_keys('Enter')
        assert until(lambda: '사람의 결정' in view_screen()), view_screen()
        assert 'second question' not in screen('demo-main') and screen('demo-main') == before, 'nothing was typed into a blocked session'
        assert 'second question' in (root / 'threads.jsonl').read_text(), 'the question is kept'
        view_stop()
    case('a blocked main gets nothing typed (exit 6 -> Blocked) and the question is kept', ask_to_blocked_main_is_saved_not_sent)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    view_stop()
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
