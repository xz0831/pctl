#!/usr/bin/env python3
"""The pctl app with saved machines, seen and driven like a person (as app_integration.py does): this
computer first and the saved machine after it, an agent of the other machine opened, typed into, pasted
into, restarted, stopped and given a bash tab, the same agent name on both, a machine that goes down
(the local group keeps working) and comes back, and an app with no server of its own. The "other
computer" is a second isolated state reached through a fake ssh (PCTL_SSH) that runs the bridge locally."""
import argparse, json, os, pathlib, re, shutil, signal, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-mach-', dir=os.path.realpath('/tmp')))
state, remote, none, cfg = root / 'local', root / 'remote', root / 'none', root / 'cfg'
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); results = []; marker = root / 'ssh-mode'; pids = root / 'pids'; reqlog = root / 'requests'
HOST = subprocess.run(['hostname', '-s'], text=True, capture_output=True).stdout.strip()
hello = json.dumps({"ev": "hello", "protocol": 1, "host": "other-mac", "version": "9.9.9"})
ssh = bindir / 'ssh'
ssh.write_text(f'''#!/bin/sh
# Test ssh: ignore options and the target, run the last argument with the other state.
echo $$ >> {pids}
case "$(cat {marker} 2>/dev/null || true)" in
  silent) exit 0 ;;
  slow) printf '%s\\n' '{hello}'; exec sleep 60 ;;
  tee) for a in "$@"; do line="$a"; done
       exec python3 -c 'import os,subprocess,sys
c = subprocess.Popen(["/bin/sh", "-c", sys.argv[1]], stdin=subprocess.PIPE, env=dict(os.environ, PCTL_STATE=sys.argv[2]))
for l in sys.stdin.buffer:
    open(sys.argv[3], "ab").write(l); c.stdin.write(l); c.stdin.flush()' "$line" {remote} {reqlog} ;;
esac
for a in "$@"; do line="$a"; done
PCTL_STATE={remote} exec /bin/sh -c "$line"
''')
ssh.chmod(0o755)
# A full-screen agent that reads the mouse like Claude Code and shows how many wheel ticks came.
(bindir / 'wheel').write_text('''#!/usr/bin/env python3
import os, re, sys, tty
tty.setraw(0); out = sys.stdout
out.write('\\x1b[?1049h\\x1b[?1000h\\x1b[?1006h\\x1b[2J\\x1b[HPOS=0\\r\\n'); out.flush()
pos, buf = 0, b''
while True:
    buf += os.read(0, 65536)
    ticks = re.findall(rb'\\x1b\\[<64;\\d+;\\d+M', buf); pos += len(ticks)
    buf = buf[buf.rfind(b'M') + 1:] if ticks else buf
    out.write(f'\\x1b[2J\\x1b[HPOS={pos}\\r\\n'); out.flush()
''')
(bindir / 'wheel').chmod(0o755)
for tag in ('L', 'R'):  # a fake agent: says which computer it runs on
    (bindir / f'plain{tag}').write_text(f'#!/bin/bash\necho "ready {tag} in $(basename "$PWD")"\necho "> "\nwhile IFS= read -r line; do echo "got {tag}: $line"; echo "> "; done\n')
    (bindir / f'plain{tag}').chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'NO_COLOR'))}
ENV.update(XDG_CONFIG_HOME=str(cfg), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ')
machines = cfg / 'pctl' / 'machines'; machines.parent.mkdir(parents=True)
machines.write_text('rm\tfake\tenabled\n')
def cli(*args, st=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=60, env=dict(ENV, PCTL_STANDALONE='1'))
    assert q.returncode == 0, (args, q.returncode, q.stderr, q.stdout)
    return json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout
def rcli(*args): return cli(*args, st=remote)
def vt(*args): return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
def rt(*args): return subprocess.run([T, '-S', str(remote / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout.strip()
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def screen(): return vt('capture-pane', '-p', '-t', 'view')
def lines(): return screen().split('\n')
def status(): return lines()[25]
def app(st=None):
    vt('kill-server')
    envs = f'PCTL_SSH={ssh} PCTL_REMOTE_BIN={B} PCTL_TMUX={T}'
    cmd = f"env -u TMUX {envs} {B} --state {st or state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', cmd], env=ENV, check=True, timeout=10)
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m'); time.sleep(0.4)
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def text(t): vt('send-keys', '-t', 'view', '-l', t); time.sleep(0.2)
def find(t, row=None, start=0):
    for i, line in enumerate(lines()):
        if (row is None or i + 1 == row) and t in line[start:]:
            return line.index(t, start) + 1 + len(t) // 2, i + 1
    raise AssertionError(f'{t!r} not on screen:\n{screen()}')
def side(name, under):
    """The sidebar row of an agent listed below the project row `under`."""
    ls = lines(); start = next(i for i, l in enumerate(ls) if under in l[:26] and ('▾' in l[:26] or '▸' in l[:26]))
    for i in range(start + 1, len(ls)):
        if name in ls[i][:26] and ls[i].startswith('    '): return ls[i].index(name) + 1, i + 1
        if ('▾' in ls[i][:26] or '▸' in ls[i][:26]): break
    raise AssertionError(f'{name} not under {under}:\n{screen()}')
def add(st, name, project, prog, folder=None):
    d = root / 'proj' / project / (folder or name); d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / prog, st=st)
def kill_bridge():
    for pid in pids.read_text().split()[::-1]:
        try: os.kill(int(pid), signal.SIGKILL); return
        except ProcessLookupError: continue
try:
    add(state, 'l-main', 'alpha', 'plainL'); add(state, 'same', 'alpha', 'plainL')
    add(remote, 'r-main', 'rproj', 'plainR'); add(remote, 'same', 'rproj', 'plainR')
    def both():
        app()
        assert until(lambda: 'ready L in l-main' in screen() and 'rproj' in screen() and 'r-main' in screen()), screen()
        s = screen()
        assert HOST in s and ' rm' in s, s
        order = [l for l in lines() if any(t in l[:26] for t in (HOST, ' rm', 'alpha', 'rproj'))]
        assert HOST in order[0] and 'alpha' in order[1] and ' rm' in order[2] and 'rproj' in order[3], order
        assert f'{HOST} · alpha / ● l-main idle' in status(), status()
    case('this computer comes first, then the saved machine, each with its projects and agents', both)
    def open_remote():
        click(*side('r-main', 'rproj'))
        assert until(lambda: 'ready R in r-main' in screen() and 'rm · rproj / ● r-main idle' in status()), screen()
    case('an agent of the other machine opens, with its screen and its machine in the status line', open_remote)
    def typing():
        text('hello remote'); keys('Enter')
        assert until(lambda: 'got R: hello remote' in rcli('session', 'read', 'r-main', '--lines', '10')), rcli('session', 'read', 'r-main')
        assert until(lambda: 'got R: hello remote' in screen()), screen()
        big = root / 'paste.txt'; big.write_text(''.join(f'{i:04d} ' + 'x' * 995 + '\n' for i in range(20)) + 'END-OF-PASTE\n')
        vt('load-buffer', '-b', 'big', big); vt('paste-buffer', '-p', '-b', 'big', '-t', 'view'); time.sleep(0.5)
        assert until(lambda: 'got R: END-OF-PASTE' in rcli('session', 'read', 'r-main', '--lines', '10')), status()
        assert 'got R' not in cli('session', 'read', 'l-main', '--lines', '10'), 'nothing reached this computer'
    case('typing and a 20 KB paste reach the other machine\'s pane', typing)
    def changed():
        s = rcli('session', 'get', 'r-main'); pane = s['pane']
        rt('set-option', '-p', '-t', pane, '@pctl_run', 'someone-else')
        text('should not arrive'); keys('Enter')
        assert until(lambda: 'r-main changed (restarted or moved)' in status()), status()
        time.sleep(1)
        assert 'should not arrive' not in rt('capture-pane', '-p', '-t', pane)
        rt('set-option', '-p', '-t', pane, '@pctl_run', s['run_id'])
        assert until(lambda: 'r-main idle' in status(), 10), status()
        text('after restore'); keys('Enter')
        assert until(lambda: 'got R: after restore' in rcli('session', 'read', 'r-main', '--lines', '10'))
    case('a pane of the other machine whose run changed gets no typing, with the same message', changed)
    def same_name():
        click(*side('same', 'alpha')); assert until(lambda: 'ready L in same' in screen() and f'{HOST} · alpha / ● same' in status()), screen()
        text('to local'); keys('Enter')
        assert until(lambda: 'got L: to local' in cli('session', 'read', 'same', '--lines', '10'))
        click(*side('same', 'rproj')); assert until(lambda: 'ready R in same' in screen() and 'rm · rproj / ● same' in status()), screen()
        text('to remote'); keys('Enter')
        assert until(lambda: 'got R: to remote' in rcli('session', 'read', 'same', '--lines', '10'))
        assert 'to remote' not in cli('session', 'read', 'same', '--lines', '20') and 'to local' not in rcli('session', 'read', 'same', '--lines', '20')
    case('the same agent name on both machines stays two agents', same_name)
    def restart_stop():
        before = rcli('session', 'get', 'r-main')['run_id']
        click(*side('r-main', 'rproj'), button=2)
        assert until(lambda: 'Restart (same conversation)' in screen()), screen()
        click(*find('Restart (same conversation)'))
        assert until(lambda: rcli('session', 'get', 'r-main')['run_id'] != before, 15), 'restarted on the other machine'
        assert until(lambda: 'Restarted r-main' in status() or 'r-main idle' in status(), 15), status()
        click(*side('r-main', 'rproj'), button=2); assert until(lambda: 'Stop…' in screen()), screen()
        click(*find('Stop…')); assert until(lambda: 'Confirm' in screen()), screen()
        x, y = find('Stop    Cancel'); click(x - 4, y)
        assert until(lambda: rcli('session', 'get', 'r-main')['status'] == 'stopped', 15)
        assert until(lambda: '○ r-main' in screen() and 'Confirm' not in screen()), screen()
        click(*side('r-main', 'rproj')); assert until(lambda: 'r-main is stopped.' in screen()), screen()
        x, y = find('Start', start=27); click(x, y)
        assert until(lambda: rcli('session', 'get', 'r-main')['status'] in ('idle', 'unknown'), 15)
    case('restart and stop (of the run shown) and start work on an agent of the other machine', restart_stop)
    def tab():
        click(*side('r-main', 'rproj'))
        assert until(lambda: 'rm · rproj / ● r-main' in status()), status()
        top = lines()[0]; click(top.rindex('+') + 1, 1)
        assert until(lambda: 'rproj-bash1' in lines()[0] and 'rm · rproj / ● rproj-bash1' in status(), 15), screen()
        got = rcli('session', 'list')
        assert any(s['name'] == 'rproj-bash1' for s in got) and not any(s['name'] == 'rproj-bash1' for s in cli('session', 'list')), got
        text('echo $((6*7))'); keys('Enter')
        assert until(lambda: '42' in screen()), screen()
    case('the + tab opens a bash tab on the other machine, not here', tab)
    def form():
        click(*side('+ agent', 'rproj'))
        assert until(lambda: 'New agent in rproj on rm' in screen()), screen()
        text('r-shell')
        click(*find('Start agent'))
        assert until(lambda: any(s['name'] == 'r-shell' for s in rcli('session', 'list')), 15)
        assert until(lambda: 'New agent' not in screen() and 'r-shell' in lines()[0]), screen()
        assert not any(s['name'] == 'r-shell' for s in cli('session', 'list'))
    case('the new-agent form starts an agent on the other machine', form)
    def scroll_remote():
        click(*side('rproj-bash1', 'rproj')); assert until(lambda: 'rm · rproj / ● rproj-bash1' in status()), status()
        text('seq 1 200'); keys('Enter')
        assert until(lambda: '200' in screen()), screen()
        for _ in range(4): click(70, 12, button=64)
        assert until(lambda: 'lines back' in screen()), screen()
        keys('Escape')
        assert until(lambda: 'lines back' not in screen()), screen()
    case('the wheel scrolls back over an agent of the other machine; a key returns to live', scroll_remote)
    def wheel_burst():
        marker.write_text('tee'); kill_bridge()
        w = add(remote, 'r-wheel', 'rproj', 'wheel')
        assert until(lambda: 'r-wheel' in screen() and reqlog.exists(), 20), screen()
        click(*side('r-wheel', 'rproj')); assert until(lambda: 'POS=0' in screen()), screen()
        x, y = find('POS=')
        before = reqlog.read_text().count('"keys"')
        vt('send-keys', '-t', 'view', '-l', f'\x1b[<64;{x};{y + 2}M' * 100)
        assert until(lambda: 'POS=100' in screen()), screen()
        sent = reqlog.read_text().count('"keys"') - before
        assert 1 <= sent <= 5, f'100 wheel ticks took {sent} keys requests'
        marker.write_text(''); kill_bridge()
        rcli('agent', 'terminate', '--agent', w['agent_id'], '--expected-run', w['run_id']); rcli('session', 'remove', 'r-wheel')
        assert until(lambda: 'r-wheel' not in screen() and 'rm · rproj' in status(), 20), screen()
    case('a burst of wheel ticks over an agent of the other machine goes as a few requests, not one each', wheel_burst)
    def comes_back():
        click(*side('r-shell', 'rproj')); assert until(lambda: 'rm · rproj / ● r-shell' in status()), status()
        click(*find('⏏ Leave')); assert until(lambda: 'EXITED 0' in screen()), screen()
        place = json.loads((state / 'app-view.json').read_text())
        assert place['machine'] == 'rm' and place['project'] == 'rproj' and ['rm', 'rproj', 'r-shell'] in place['remote_tabs'], place
        assert place['tabs'] == {'alpha': 'same'}, place
        app(); assert until(lambda: 'rm · rproj / ● r-shell' in status()), status()
    case('the app reopens on the other machine\'s agent, and remembers each machine\'s tab', comes_back)
    def down():
        click(*side('r-main', 'rproj')); assert until(lambda: 'ready R in r-main' in screen()), screen()
        marker.write_text('silent'); kill_bridge()
        assert until(lambda: 'down:' in screen(), 15), screen()
        assert 'r-main' in screen(), 'the last list stays'
        assert until(lambda: 'rm is down' in screen()), screen()
        text('nope'); assert until(lambda: 'rm is down' in status()), status()
        click(*side('l-main', 'alpha')); assert until(lambda: 'ready L in l-main' in screen()), screen()
        text('still here'); keys('Enter')
        assert until(lambda: 'got L: still here' in screen(), 5), 'the local group works while the other machine is down'
        marker.write_text('')
        x, y = next((l.index('rm') + 1, i + 1) for i, l in enumerate(lines()) if '▾ rm' in l[:26])
        click(x, y, button=2); assert until(lambda: 'Reconnect' in screen()), screen()
        click(*find('Reconnect'))
        assert until(lambda: 'down:' not in screen() and 'r-main' in screen(), 15), screen()
        click(*side('r-main', 'rproj')); assert until(lambda: 'ready R in r-main' in screen() and 'rm · rproj' in status()), screen()
        text('back again'); keys('Enter')
        assert until(lambda: 'got R: back again' in rcli('session', 'read', 'r-main', '--lines', '10'))
    case('a machine that goes down keeps its list muted, refuses typing, leaves the local group working, and reconnects from its menu', down)
    def slow():
        marker.write_text('slow'); kill_bridge()
        x, y = next((l.index('rm') + 1, i + 1) for i, l in enumerate(lines()) if '▾ rm' in l[:26])
        assert until(lambda: 'down:' in screen(), 15)
        click(x, y, button=2); assert until(lambda: 'Reconnect' in screen()), screen()
        click(*find('Reconnect')); time.sleep(1)  # connected and silent: its list never comes
        click(*side('l-main', 'alpha'))
        text('quick'); keys('Enter')
        assert until(lambda: 'got L: quick' in screen(), 3), 'a silent machine does not hold up the local group'
        marker.write_text(''); kill_bridge()
        assert until(lambda: 'down:' not in screen() and 'r-main' in screen(), 25), 'it comes back on its own'
    case('a machine that answers nothing never freezes the app; it recovers by itself', slow)
    def no_server():
        app(st=none)
        assert until(lambda: 'no pctl server here' in screen() and 'rproj' in screen() and ' rm' in screen()), screen()
        assert '[+]' not in lines()[0] and 'No PCTL server' not in screen(), 'no + Project, no welcome screen'
        assert subprocess.run([T, '-S', str(none / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server started here'
        click(*side('r-main', 'rproj')); assert until(lambda: 'ready R in r-main' in screen()), screen()
    case('an app with no server of its own shows the saved machine and offers no + Project', no_server)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server')
    for st in (state, remote):
        subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
