#!/usr/bin/env python3
"""msg send wakes a terminal-line recipient with one notice line; native recipients and retries are not woken."""
import argparse, json, os, pathlib, subprocess, tempfile, time
os.environ['HERDR_ENV'] = '1'  # Only disposable test servers bypass the Herdr startup gate.
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-wake-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
def cli(*args, env=None):
    q = subprocess.run([B, '--state', str(state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=20, env=env)
    assert q.returncode == 0, (args, q.stderr, q.stdout)
    return json.loads(q.stdout)
def tm(*args):
    return subprocess.run([T, '-S', str(state / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
def agent(project, name, input_mode):
    x = cli('agent', 'add', '--project', project, '--name', name, '--cwd', root, '--adapter', 'generic', '--', '/bin/cat')
    cli('agent', 'configure-collaboration', '--agent', x['id'], '--mode', 'mailbox')
    if input_mode: cli('agent', 'configure-input', '--agent', x['id'], '--mode', 'terminal-line')
    return x, cli('agent', 'start', '--agent', x['id'])
def caller(x, run):
    env = dict(os.environ); env.update(PCTL_BIN=B, PCTL_STATE=str(state), PCTL_TMUX=T, PCTL_PROJECT_ID=x['project_id'], PCTL_AGENT_ID=x['id'], PCTL_RUN_ID=run['id'])
    return env
def screen(run):
    return tm('capture-pane', '-p', '-t', run['pane'])
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
try:
    pr = cli('project', 'add', '--name', 'wake', '--root', root)
    sender, srun = agent(pr['id'], 'sender', False)
    woke, wrun = agent(pr['id'], 'woke', True)
    quiet, qrun = agent(pr['id'], 'quiet', False)
    time.sleep(0.5)
    env = caller(sender, srun)
    def wakes():
        out = cli('msg', 'send', 'woke', '--key', 'k1', '--text', 'hello', env=env)
        assert out['wake'].startswith('delivered'), out
        h = out['message']['handle']
        for _ in range(40):
            if f'[pctl] new message {h} from sender' in screen(wrun): break
            time.sleep(0.05)
        s = screen(wrun)
        assert f'[pctl] new message {h} from sender' in s, s
        assert 'hello' not in s, 'message body must stay in the mailbox'
    case('terminal-line recipient gets one notice line, not the body', wakes)
    def retry():
        before = screen(wrun).count('[pctl] new message')
        out = cli('msg', 'send', 'woke', '--key', 'k1', '--text', 'hello', env=env)
        assert out['deduplicated'] and out['wake'].startswith('skipped'), out
        time.sleep(0.7); assert screen(wrun).count('[pctl] new message') == before
    case('identical retry is not woken again', retry)
    def native():
        out = cli('msg', 'send', 'quiet', '--key', 'k2', '--text', 'hello', env=env)
        assert out['wake'] == 'skipped: recipient input mode is native', out
        time.sleep(0.7); assert '[pctl]' not in screen(qrun)
    case('native recipient is stored only', native)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
