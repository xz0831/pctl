#!/usr/bin/env python3
"""The pctl app (`pctl` alone) over real tmux, seen and driven like a person: the app runs inside an
outer "monitor" tmux; its screen is read with capture-pane, keys go in with send-keys, and mouse
clicks are SGR mouse sequences typed into it. Covers the sidebar, tabs and live terminal, typing,
clicks, prefix shortcuts, the new-agent form, the right-click menu with its confirmation (and that
clicks on a box never reach what is behind it), Start for a stopped agent, wheel scroll-back,
Leave (stops nothing, reopens in place), shortcuts from a config file, and the empty state with no
server (which starts nothing)."""
import argparse, json, os, pathlib, re, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-app-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); cfg = root / 'cfg'
(bindir / 'plain').write_text('#!/bin/bash\nif [ "$(basename "$PWD")" = b-block ]; then echo "Do you want to proceed?"; echo "❯ 1. Yes"; fi\necho "ready in $(basename "$PWD")"\necho "> "\nwhile IFS= read -r line; do echo "got: $line"; echo "> "; done\n')
(bindir / 'plain').chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('HERDR_', 'PCTL_', 'TMUX', 'XDG_CONFIG'))}
ENV.update(TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ')
def cli(*args, ok=True, st=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=30, env=dict(ENV, HERDR_ENV='1'))
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    return (json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout) if ok else q.stderr
def vt(*args):
    return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
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
def app(st=None, env=None):
    vt('kill-server')
    envs = ' '.join(f'{k}={v}' for k, v in (env or {}).items())
    cmd = f"env -u TMUX -u HERDR_ENV {envs} {B} --state {st or state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', cmd], env=ENV, check=True, timeout=10)
    assert until(lambda: '+ agent' in screen() or 'Welcome to pctl' in screen()), screen()
def find(text, row=None, start=0):
    """1-based (column, row) of the middle of `text`; columns count characters as tmux shows them."""
    for i, line in enumerate(lines()):
        if (row is None or i + 1 == row) and text in line[start:]:
            return line.index(text, start) + 1 + len(text) // 2, i + 1
    raise AssertionError(f'{text!r} not on screen:\n{screen()}')
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m')
    time.sleep(0.4)
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def text(t): vt('send-keys', '-t', 'view', '-l', t); time.sleep(0.2)
def add(name, project, folder=None):
    d = root / 'proj' / project / (folder or name); d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / 'plain')
def side(name):
    """The sidebar row of an agent (indented under its project, within the 26-column sidebar)."""
    for i, line in enumerate(lines()):
        if line.startswith('   ') and name in line[:26]:
            return line.index(name) + 1, i + 1
    raise AssertionError(f'{name} not in the sidebar:\n{screen()}')
try:
    add('a-main', 'alpha'); add('a-block', 'alpha', 'b-block'); add('b-main', 'beta')
    assert until(lambda: cli('session', 'get', 'a-block')['status'] == 'blocked')
    def opens():
        app()
        s = screen()
        for t in ('PROJECTS', '▾ alpha !1', '● a-main', '! a-block', '▾ beta', '● b-main', '+ agent', '? Help', '⏏ Leave'):
            assert t in s, (t, s)
        assert re.search(r'│ ● a-main +! a-block +\+', lines()[0]), lines()[0]
        assert until(lambda: 'ready in a-main' in screen()), screen()
        assert 'alpha / ● a-main idle' in status() and '! 1 waiting for you' in status(), status()
    case('pctl alone opens the app: projects, agents and their state, tabs, the live terminal', opens)
    def typing():
        text('hello app'); keys('Enter')
        assert until(lambda: 'got: hello app' in cli('session', 'read', 'a-main', '--lines', '10'))
        assert until(lambda: 'got: hello app' in screen()), screen()
    case('typing goes to the agent on screen', typing)
    def identity():
        s0 = cli('session', 'get', 'a-main')
        sock = str(state / 'tmux.sock')
        subprocess.run([T, '-S', sock, 'set-option', '-p', '-t', s0['pane'], '@pctl_run', 'someone-else'], check=True)
        text('should not arrive'); keys('Enter')
        time.sleep(1.5)
        raw = subprocess.run([T, '-S', sock, 'capture-pane', '-p', '-t', s0['pane']], text=True, capture_output=True).stdout
        assert 'should not arrive' not in raw, 'nothing is typed into a pane whose run changed (pctl itself now calls it lost)'
        subprocess.run([T, '-S', sock, 'set-option', '-p', '-t', s0['pane'], '@pctl_run', s0['run_id']], check=True)
        assert until(lambda: 'a-main idle' in status(), 10), status()
        text('after restore'); keys('Enter')
        assert until(lambda: 'got: after restore' in cli('session', 'read', 'a-main', '--lines', '10'))
    case('a pane whose run changed gets no typing; it works again once the run matches', identity)
    def switching():
        click(*side('a-block'))
        assert until(lambda: 'a-block blocked' in status() and 'Do you want to proceed?' in screen()), screen()
        keys('C-b', 'n'); assert until(lambda: 'a-main idle' in status()), status()
        keys('C-b', '2'); assert until(lambda: 'a-block blocked' in status()), status()
        click(*side('b-main')); assert until(lambda: 'beta / ● b-main' in status() and 'ready in b-main' in screen()), screen()
        keys('C-b', '?'); assert until(lambda: 'Shortcuts' in screen() and 'ctrl+b c' in screen()), screen()
        keys('Escape'); assert until(lambda: 'Shortcuts' not in screen())
        click(*side('a-main')); assert until(lambda: 'a-main idle' in status())
    case('clicks, ctrl+b n and ctrl+b 2 switch agents; ctrl+b ? shows the shortcuts', switching)
    def new_tab():
        top = lines()[0]
        click(top.rindex('+') + 1, 1)
        assert until(lambda: 'alpha-bash1' in lines()[0] and 'alpha / ● alpha-bash1' in status(), 15), screen()
        cmd = cli('session', 'list')
        assert any(s['name'] == 'alpha-bash1' and s['harness'] == '' for s in cmd), cmd
        argv = next(x['argv'] for x in json.loads((state / 'registry.json').read_text())['agents'] if x['name'] == 'alpha-bash1')
        assert pathlib.Path(argv[0]).name == 'bash' and argv[1:] == ['-l'], argv
        text('echo $BASH_VERSION $((6*7))'); keys('Enter')
        assert until(lambda: re.search(r'\d+\.\d+\.\d+.* 42', screen())), screen()
    case('the + tab opens a bash tab at once, like a new terminal tab', new_tab)
    def form():
        click(*side('+ agent'))
        assert until(lambda: 'New agent in alpha' in screen() and '● bash' in screen()), screen()
        text('a-shell')
        click(*find('Start agent'))
        assert until(lambda: any(s['name'] == 'a-shell' and s['status'] in ('idle', 'unknown') for s in cli('session', 'list')), 15)
        assert until(lambda: 'a-shell' in lines()[0] and 'New agent' not in screen()), screen()
        text('echo $((6*7))'); keys('Enter')
        assert until(lambda: '42' in screen()), screen()
    case('+ agent opens the new-agent form with bash chosen; the agent started from it takes typing', form)
    def stop():
        click(*side('a-shell'), button=2)
        assert until(lambda: 'Stop…' in screen()), screen()
        click(*find('Stop…'))
        assert until(lambda: 'Confirm' in screen()), screen()
        before = status()
        x, y = find('Stop a-shell?'); click(x + 3, y)
        assert 'Confirm' in screen() and status() == before, 'a click on the box text reached nothing behind it'
        x, y = find('Stop    Cancel'); click(x - 4, y)
        assert until(lambda: cli('session', 'get', 'a-shell')['status'] == 'stopped', 15)
        assert until(lambda: '○ a-shell' in screen() and 'Confirm' not in screen()), screen()
    case('right-click Stop asks first; a click on the box text does nothing; Stop stops it', stop)
    def start():
        click(*side('a-shell'))
        assert until(lambda: 'a-shell is stopped.' in screen()), screen()
        x, y = find(' Start ', start=27); click(x, y)
        assert until(lambda: cli('session', 'get', 'a-shell')['status'] in ('idle', 'unknown'), 15)
        assert until(lambda: '● a-shell' in screen()), screen()
    case('a stopped agent shows Start; Start brings it back', start)
    def scroll():
        text('seq 1 200'); keys('Enter')
        assert until(lambda: '200' in screen()), screen()
        for _ in range(4): click(60, 12, button=64)
        assert until(lambda: 'lines back' in screen()), screen()
        keys('Escape')
        assert until(lambda: 'lines back' not in screen()), screen()
    case('the wheel scrolls back over the terminal; a key returns to live', scroll)
    def leave():
        here = status()
        click(*find('⏏ Leave'))
        assert until(lambda: 'EXITED 0' in screen()), screen()
        rows = {s['name']: s['status'] for s in cli('session', 'list')}
        assert rows['a-main'] == 'idle' and rows['a-shell'] in ('idle', 'unknown') and rows['a-block'] == 'blocked', rows
        app()
        assert until(lambda: 'alpha / ● a-shell' in status()), (here, status())
    case('Leave exits the app and stops nothing; pctl reopens where it was left', leave)
    def config():
        (cfg / 'pctl').mkdir(parents=True)
        (cfg / 'pctl' / 'config.toml').write_text('[keys]\nprefix = "ctrl+a"\nnext_tab = "alt+right"\nbogus = "x"\n')
        app(env={'XDG_CONFIG_HOME': cfg})
        assert until(lambda: 'unknown shortcut' in status() and status().rstrip().endswith('⏏ Leave')), status()
        keys('C-a', '?'); assert until(lambda: 'ctrl+a c' in screen() and 'alt+right' in screen()), screen()
        keys('Escape')
        before = status()
        keys('M-Right'); assert until(lambda: status() != before and 'a-main' in status()), (before, status())
    case('shortcuts come from the config file; bad names are reported', config)
    def themes():
        app()
        click(*find('◐ Theme'))
        assert until(lambda: 'Theme' in screen() and 'nebula' in screen() and 'mono' in screen()), screen()
        click(*find('nebula'))
        assert until(lambda: 'Theme: nebula' in status()), status()
        assert json.loads((state / 'appearance.json').read_text())['theme'] == 'nebula'
        colored = vt('capture-pane', '-p', '-e', '-t', 'view')
        assert '48;2;24;18;42' in colored, 'the terminal area takes the theme background'
        keys('C-b', 't'); assert until(lambda: '● nebula' in screen()), screen()
        keys('Escape')
        cli('theme', 'portal')
    case('the Theme button and ctrl+b t pick a theme; the terminal area follows it', themes)
    def own_theme():
        env = dict(ENV, XDG_CONFIG_HOME=str(cfg))
        q = subprocess.run([B, '--state', str(state), '--tmux', T, 'theme', '--new', 'sunrise', '--from', 'garage'], text=True, capture_output=True, env=env, timeout=30)
        made = pathlib.Path(json.loads(q.stdout)['created'])
        assert made == cfg / 'pctl' / 'themes' / 'sunrise.toml' and 'term_bg' in made.read_text() and 'ansi = [' in made.read_text(), q
        made.write_text(made.read_text().replace('term_bg  = "#1b1612"', 'term_bg  = "#203040"').replace('made from garage; say what makes it yours', 'early light'))
        q = subprocess.run([B, '--state', str(state), '--tmux', T, 'theme', '--list'], text=True, capture_output=True, env=env, timeout=30)
        listed = json.loads(q.stdout)
        assert {'name': 'sunrise', 'about': 'early light'} in listed['yours'] and not listed['warnings'], listed
        assert 'already exists' in subprocess.run([B, 'theme', '--new', 'sunrise'], text=True, capture_output=True, env=env).stderr
        app(env={'XDG_CONFIG_HOME': cfg})
        click(*find('◐ Theme'))
        assert until(lambda: 'sunrise' in screen() and '(yours)' in screen()), screen()
        click(*find('sunrise'))
        assert until(lambda: 'Theme: sunrise' in status()), status()
        assert '48;2;32;48;64' in vt('capture-pane', '-p', '-e', '-t', 'view'), 'the file colours the terminal'
        click(*find('⏏ Leave')); assert until(lambda: 'EXITED 0' in screen())
        app(env={'XDG_CONFIG_HOME': cfg})
        assert until(lambda: '48;2;32;48;64' in vt('capture-pane', '-p', '-e', '-t', 'view')), 'still chosen after reopening'
    case('pctl theme --new writes a theme file; edited, it shows in the picker and stays chosen', own_theme)
    def empty():
        other = root / 'e'
        app(st=other)
        assert until(lambda: 'No PCTL server is running yet' in screen() and '+ Project' in screen()), screen()
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server started'
    case('with no server the app explains, offers + Project and starts nothing', empty)
    def first_run():
        other, folder = root / 'e', root / 'proj' / 'gamma'
        folder.mkdir(parents=True)
        click(*find('+ Project', start=27))
        assert until(lambda: 'New project' in screen()), screen()
        text('gamma'); keys('Tab'); text(str(folder))
        click(*find('Add project'))
        assert until(lambda: 'New agent in gamma' in screen() and str(folder) in screen()), screen()
        text('g-shell'); click(*find('Start agent'))
        assert until(lambda: 'server_start_outside_herdr' in screen()), screen()
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'refused: no server'
        app(st=other, env={'PCTL_STANDALONE': '1'})
        assert until(lambda: '▾ gamma' in screen()), screen()
        click(*side('+ agent'))
        text('g-shell'); click(*find('Start agent'))
        assert until(lambda: '● g-shell' in screen() and 'New agent' not in screen(), 15), screen()
        assert cli('session', 'get', 'g-shell', st=other)['status'] in ('idle', 'unknown')
        subprocess.run([T, '-S', str(other / 'tmux.sock'), 'kill-server'], capture_output=True)
    case('first run: + Project, then the first agent; without Herdr the form says why, and PCTL_STANDALONE=1 starts it', first_run)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    vt('kill-server')
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
