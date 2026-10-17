#!/usr/bin/env python3
"""The pctl app (`pctl` alone) over real tmux, seen and driven like a person: the app runs inside an
outer "monitor" tmux; its screen is read with capture-pane, keys go in with send-keys, and mouse
clicks are SGR mouse sequences typed into it. Covers the sidebar, tabs and live terminal, typing,
clicks, prefix shortcuts, the new-agent form, the right-click menu with its confirmation (and that
clicks on a box never reach what is behind it; double-click and ctrl+b m open the same menu), Start for a stopped agent, wheel scroll-back,
drag-to-copy to the clipboard (OSC 52 and a local command, with honest wording where the terminal ignores it), desktop notices, Leave (stops nothing, reopens in place, never leaves window-size at manual, even when killed), shortcuts from a config file, the
empty state with no server (which starts nothing), and that agents cannot open the app."""
import argparse, json, os, pathlib, re, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-app-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); cfg = root / 'cfg'
(bindir / 'plain').write_text('#!/bin/bash\nif [ "$(basename "$PWD")" = b-block ]; then echo "Do you want to proceed?"; echo "❯ 1. Yes"; fi\necho "ready in $(basename "$PWD")"\necho "> "\nwhile IFS= read -r line; do echo "got: $line"; echo "> "; done\n')
(bindir / 'plain').chmod(0o755)
(bindir / 'clipw').write_text('#!/bin/sh\ncat > "$0.out"\n'); (bindir / 'clipw').chmod(0o755)  # PCTL_CLIP_WRITE_TEXT: keeps what it is given
(bindir / 'notify').write_text('#!/bin/sh\nprintf "%s|" "$@" >> "$0.out"; echo >> "$0.out"\n'); (bindir / 'notify').chmod(0o755)  # PCTL_NOTIFY_SEND
(bindir / 'flip').write_text('#!/bin/bash\necho "esc to interrupt"\nwhile [ ! -e "$PWD/../go" ]; do sleep 0.2; done\nclear; echo "Do you want to proceed?"; echo "❯ 1. Yes"; sleep 600\n')
(bindir / 'flip').chmod(0o755)  # works until a file appears, then asks a question
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'VTE_VERSION', 'SSH_CONNECTION', 'TERM_PROGRAM', 'WAYLAND_DISPLAY', 'DISPLAY', 'NO_COLOR'))}  # NO_COLOR (some agent shells set it) would turn the app's colours off
ENV['XDG_CONFIG_HOME'] = str(root / 'no-config')  # the person's own config.toml stays out
ENV.update(TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ')
def cli(*args, ok=True, st=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=30, env=dict(ENV, PCTL_STANDALONE='1'))
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
def app(st=None, env=None, ready=True):
    vt('kill-server')
    envs = ' '.join(f'{k}={v}' for k, v in (env or {}).items())
    cmd = f"env -u TMUX {envs} {B} --state {st or state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', cmd], env=ENV, check=True, timeout=10)
    if ready: assert until(lambda: '+ agent' in screen() or 'Welcome to pctl' in screen()), screen()
def st(*args): return subprocess.run([T, '-S', str(state / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout.strip()
def find(text, row=None, start=0):
    """1-based (column, row) of the middle of `text`; columns count characters as tmux shows them."""
    for i, line in enumerate(lines()):
        if (row is None or i + 1 == row) and text in line[start:]:
            return line.index(text, start) + 1 + len(text) // 2, i + 1
    raise AssertionError(f'{text!r} not on screen:\n{screen()}')
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m')
    time.sleep(0.4)
def drag(x1, y1, x2, y2):
    """A left press and a drag, the SGR sequences a terminal reports, without the release."""
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x1};{y1}M\x1b[<32;{x2};{y2}M')
    time.sleep(0.4)
def release(x, y):
    """The button release that ends a drag or a plain click."""
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x};{y}m')
    time.sleep(0.4)
def span(text):
    """1-based (first column, last column, row) of `text` on screen."""
    for i, line in enumerate(lines()):
        j = line.find(text)
        if j >= 0:
            return j + 1, j + len(text), i + 1
    raise AssertionError(f'{text!r} not on screen:\n{screen()}')
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def dclick(x, y):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m' * 2); time.sleep(0.4)
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
        big = root / 'paste.txt'; big.write_text(''.join(f'{i:04d} ' + 'x' * 995 + '\n' for i in range(20)) + 'END-OF-PASTE\n')  # 20 KB
        vt('load-buffer', '-b', 'big', big); vt('paste-buffer', '-p', '-b', 'big', '-t', 'view'); time.sleep(0.5)
        assert until(lambda: 'got: END-OF-PASTE' in cli('session', 'read', 'a-main', '--lines', '10')), status()
        assert 'Could not paste' not in status(), status()
    case('typing goes to the agent on screen; a 20 KB paste arrives whole', typing)
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
    def no_right_button():
        dclick(*side('b-main'))
        assert until(lambda: 'Restart (same conversation)' in screen() and 'beta / ● b-main' in status()), screen()
        keys('Escape'); assert until(lambda: 'Restart (same' not in screen()), screen()
        x, y = side('a-main'); click(x, y); time.sleep(0.3); click(x, y)
        assert 'Restart (same' not in screen() and 'alpha / ● a-main' in status(), 'two slow clicks just open the agent'
        dclick(*find('a-block', row=1))
        assert until(lambda: 'Restart (same conversation)' in screen() and 'a-block blocked' in status()), screen()
        keys('Escape'); keys('C-b', 'm')
        assert until(lambda: 'Restart (same conversation)' in screen()), screen()
        keys('Escape'); click(*side('a-main')); assert until(lambda: 'a-main idle' in status())
    case('without a right button: double-click an agent or tab, or ctrl+b m, opens its menu', no_right_button)
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
        # Wait for the shell's prompt: a login profile may still be starting and drop keys typed early.
        assert until(lambda: any(l.rstrip().endswith('$') for l in lines()[1:-1]), 15), screen()
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
        s = cli('session', 'get', 'a-shell')  # the agent restarts while the box is open
        cli('agent', 'restart', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        x, y = find('Stop    Cancel'); click(x - 4, y)
        assert until(lambda: 'Could not' in status() or 'stale' in status().lower()), status()
        assert cli('session', 'get', 'a-shell')['process'] == 'running', 'Stop does not stop a run the person never saw'
        click(*side('a-shell'), button=2); assert until(lambda: 'Stop…' in screen()), screen()
        click(*find('Stop…')); assert until(lambda: 'Confirm' in screen()), screen()
        x, y = find('Stop    Cancel'); click(x - 4, y)
        assert until(lambda: cli('session', 'get', 'a-shell')['status'] == 'stopped', 15)
        assert until(lambda: '○ a-shell' in screen() and 'Confirm' not in screen()), screen()
    case('right-click Stop asks first; a click on the box text does nothing; Stop stops the run it showed', stop)
    def start():
        click(*side('a-shell'))
        assert until(lambda: 'a-shell is stopped.' in screen()), screen()
        click(*find('Start a new conversation…'))
        assert until(lambda: 'new conversation?' in screen()), screen()
        x, y = find('Start new    Cancel'); click(x + 8, y)
        assert until(lambda: 'Confirm' not in screen()) and cli('session', 'get', 'a-shell')['status'] == 'stopped', 'Cancel starts nothing'
        x, y = find('Start', start=27); click(x, y)  # the first Start button; capture trims its trailing space
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
        pane = cli('session', 'get', 'a-shell')['pane']
        assert int(st('display-message', '-p', '-t', pane, '#{pane_width}')) > 50, 'the app sized the window it shows'
        assert st('show-options', '-wv', '-t', pane, 'window-size') == '', 'tmux keeps its own sizing while the app shows it'
        st('resize-window', '-t', pane, '-x', '50', '-y', '10'); st('set-option', '-wu', '-t', pane, 'window-size'); time.sleep(1.5)  # another viewer
        assert st('display-message', '-p', '-t', pane, '#{pane_width}') == '50', 'the app does not fight another size'
        click(*find('⏏ Leave'))
        assert until(lambda: 'EXITED 0' in screen()), screen()
        rows = {s['name']: s['status'] for s in cli('session', 'list')}
        assert rows['a-main'] == 'idle' and rows['a-shell'] in ('idle', 'unknown') and rows['a-block'] == 'blocked', rows
        app()
        assert until(lambda: 'alpha / ● a-shell' in status()), (here, status())
        assert until(lambda: int(st('display-message', '-p', '-t', pane, '#{pane_width}')) > 50), 'reopened, it sizes the window again'
        vt('kill-server'); time.sleep(0.5)  # the terminal goes away under the app (SIGHUP)
        assert st('show-options', '-wv', '-t', pane, 'window-size') == '', 'a killed app leaves no manual size behind'
        app(); assert until(lambda: 'alpha / ● a-shell' in status()), status()
    case('Leave exits the app and stops nothing, never leaves window-size manual; pctl reopens where it was left', leave)
    def attached():
        pane = cli('session', 'get', 'a-shell')['pane']; sess = st('display-message', '-p', '-t', pane, '#{session_name}')
        size = lambda f: int(st('display-message', '-p', '-t', pane, '#{' + f + '}'))
        C = root / 'c.sock'  # a person's `pctl attach`: a real tmux client, 100x40, showing a-shell's window
        subprocess.run([T, '-S', str(C), '-f', '/dev/null', 'new-session', '-d', '-x', '100', '-y', '40', '-s', 'c',
                        f"env -u TMUX {T} -S {state / 'tmux.sock'} attach -t {sess}"], env=ENV, check=True, timeout=10)
        try:
            assert until(lambda: size('session_attached') == 1), 'the client attached'
            st('select-window', '-t', pane)
            assert until(lambda: size('window_width') == 100 and size('pane_height') > 30, 20), 'the client took the window'
            text('echo BOTTOM-MARK'); keys('Enter')
            assert until(lambda: 'BOTTOM-MARK' in screen()), 'a taller pane shows its bottom rows (input line in sight)'
            # Scrolled back, the window stays anchored to the bottom like the live view: 12 lines
            # above the live bottom, so the last cell row is 12 rows above the prompt row (the shell prompt), not the pane's top.
            cell = lambda: [l.split('│', 1)[-1].rstrip() for l in lines()[1:25]]
            text('seq 1 100'); keys('Enter')
            assert until(lambda: cell()[-1].endswith('$') and cell()[-2] == '100'), cell()
            for _ in range(2): click(60, 12, button=64)  # a press and a release each: 4 notches of 3 lines
            assert until(lambda: '12 lines back' in screen()), screen()
            assert until(lambda: cell()[-1] == '89' and cell()[0].split()[0] == '66'), cell()
            keys('Escape'); assert until(lambda: 'lines back' not in screen() and cell()[-2] == '100'), screen()
            click(*side('a-main')); click(*side('a-shell'))  # opening it again asks for the app's size
            assert until(lambda: st('show-options', '-wv', '-t', pane, 'window-size') == 'manual' and size('window_width') < 100), 'with a client attached the size holds'
            w = size('window_width'); time.sleep(1.5); assert size('window_width') == w, 'and stays'
            click(*find('⏏ Leave')); assert until(lambda: 'EXITED 0' in screen()), screen()
            assert until(lambda: st('show-options', '-wv', '-t', pane, 'window-size') == '' and size('window_width') == 100), 'leaving hands the window back to the client'
        finally:
            subprocess.run([T, '-S', str(C), 'kill-server'], capture_output=True)
        app(); assert until(lambda: 'alpha / ● a-shell' in status()), status()
    case('with a client attached the app keeps its size while open, shows the bottom rows (also scrolled back), and leaving hands the window back', attached)
    def agents_refused():
        for env in ({'PCTL_AGENT_ID': 'x'}, {'PCTL_RUN_ID': 'x'}, {'TMUX': f'{state}/tmux.sock,1,0'}):
            app(env=env, ready=False)
            assert until(lambda: 'EXITED 1' in screen()) and 'agent_caller' in screen(), (env, screen())
        app()
    case('an agent, or a pane of this server, cannot open the app', agents_refused)
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
        assert until(lambda: 'server_start_not_allowed' in screen()), screen()
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'refused: no server'
        app(st=other, env={'PCTL_STANDALONE': '1'})
        assert until(lambda: '▾ gamma' in screen()), screen()
        click(*side('+ agent'))
        text('g-shell'); click(*find('Start agent'))
        assert until(lambda: '● g-shell' in screen() and 'New agent' not in screen(), 15), screen()
        assert cli('session', 'get', 'g-shell', st=other)['status'] in ('idle', 'unknown')
        subprocess.run([T, '-S', str(other / 'tmux.sock'), 'kill-server'], capture_output=True)
    case('first run: + Project, then the first agent; without leave to start a server the form says why, and PCTL_STANDALONE=1 starts it', first_run)
    def copy_drag():
        clipw = bindir / 'clipw'; out = pathlib.Path(str(clipw) + '.out'); out.unlink(missing_ok=True)
        app(env={'PCTL_CLIP_WRITE_TEXT': clipw})
        click(*side('a-main'))
        assert until(lambda: 'alpha / ● a-main' in status() and '>' in screen()), screen()
        vt('set-option', '-g', 'set-clipboard', 'on')  # the outer tmux then stores an OSC 52 write
        click(60, 12)  # a plain click in the terminal selects nothing
        assert vt('show-buffer').strip() == '' and 'Copied' not in status(), (status(), vt('show-buffer'))
        text('echo HELLO-WORLD'); keys('Enter')
        assert until(lambda: 'got: echo HELLO-WORLD' in screen()), screen()
        x1, x2, y = span('got: echo HELLO-WORLD')
        drag(x1, y, x2, y)
        hl = lambda: vt('capture-pane', '-p', '-e', '-t', 'view').split('\n')[y - 1]
        assert until(lambda: '\x1b[7m' in hl()), hl()  # the selection is painted while the drag is held
        release(x2, y)
        assert until(lambda: 'Copied 21 characters' in status()), status()
        assert until(lambda: vt('show-buffer').strip() == 'got: echo HELLO-WORLD'), vt('show-buffer')
        assert until(lambda: out.exists() and out.read_text() == 'got: echo HELLO-WORLD'), out  # the local command got the exact text too
        vt('delete-buffer')
        drag(x2, y, x1, y)  # dragging backwards copies the same text
        release(x1, y)
        assert until(lambda: vt('show-buffer').strip() == 'got: echo HELLO-WORLD'), vt('show-buffer')
    case('a left drag in the terminal copies the selection to the clipboard, either direction; a plain click copies nothing', copy_drag)
    def copy_wording():
        clipw = bindir / 'clipw'; out = pathlib.Path(str(clipw) + '.out')
        def copied(env):
            out.unlink(missing_ok=True)
            app(env=env)
            vt('resize-window', '-t', 'view', '-x', '150')  # the status line cuts a notice at the window edge
            assert until(lambda: any('a-main' in l[:26] for l in lines())), screen()
            click(*side('a-main'))
            assert until(lambda: 'alpha / ● a-main' in status() and '>' in screen()), screen()
            text('echo HELLO-WORLD'); keys('Enter')
            assert until(lambda: 'got: echo HELLO-WORLD' in screen()), screen()
            x1, x2, y = span('got: echo HELLO-WORLD')
            drag(x1, y, x2, y); release(x2, y)
            time.sleep(0.5)
            return status()
        s = copied({'PCTL_CLIP_WRITE_TEXT': clipw, 'SSH_CONNECTION': '1'})
        assert 'Sent 21 characters' in s and 'Shift+drag does' in s, s
        assert not out.exists(), 'over ssh this computer\'s clipboard is not the person\'s: the command is not run'
        s = copied({'PCTL_CLIP_WRITE_TEXT': 'false'})
        assert 'Sent 21 characters' in s and 'Copied' not in s, s
        s = copied({'PCTL_CLIP_WRITE_TEXT': 'false', 'VTE_VERSION': '6402'})
        assert 'This terminal ignores copying: Shift+drag, then ctrl+shift+c' in s, s
        s = copied({'PCTL_CLIP_WRITE_TEXT': clipw, 'VTE_VERSION': '6402'})
        assert 'Copied 21 characters' in s and out.read_text() == 'got: echo HELLO-WORLD', s  # a working command beats the VTE warning
        s = copied({'PCTL_CLIP_WRITE_TEXT': 'false', 'TERM_PROGRAM': 'ghostty'})
        assert 'Copied 21 characters' in s, s  # a terminal known to honour OSC 52
    case('a copy says Copied only when a command or a known terminal took it; over ssh the command is not run; VTE gets the Shift+drag hint', copy_wording)
    def notice_command():
        nsend = bindir / 'notify'; out = pathlib.Path(str(nsend) + '.out'); out.unlink(missing_ok=True)
        d = root / 'proj' / 'alpha' / 'flip'; d.mkdir(parents=True, exist_ok=True); (root / 'proj' / 'alpha' / 'go').unlink(missing_ok=True)
        cli('session', 'add', '--name', 'a-flip', '--cwd', d, '--project', 'alpha', '--', bindir / 'flip')
        assert until(lambda: cli('session', 'get', 'a-flip')['status'] == 'working'), cli('session', 'get', 'a-flip')
        app(env={'PCTL_NOTIFY_SEND': nsend})
        click(*side('a-main'))
        assert until(lambda: 'a-flip' in screen()), screen()
        time.sleep(3)  # the app has watched a-flip work
        (root / 'proj' / 'alpha' / 'go').write_text('')
        assert until(lambda: out.exists() and 'a-flip: needs you' in out.read_text(), 20), out.read_text() if out.exists() else 'no call'
        assert out.read_text().splitlines()[0] == '--|pctl|a-flip: needs you|', out.read_text()
        assert out.read_text().count('\n') == 1, 'one notice, not one per refresh'
    case('an agent that goes from working to waiting also gets a desktop notice (notify-send stand-in), once', notice_command)
    def help_line():
        app()
        click(*find('? Help'))
        assert until(lambda: 'keep the right button' in screen()), screen()
        keys('Escape')
    case('the help screen says some terminals keep the right button', help_line)
    def sidebar_scroll():
        for proj, n in (('pa', 14), ('pb', 13), ('pc', 13)):
            for i in range(1, n + 1):
                add('pc-last' if proj == 'pc' and i == n else f'{proj}-{i:02d}', proj)
        app()
        wheel = lambda up, n: (vt('send-keys', '-t', 'view', '-l', f'\x1b[<{64 if up else 65};5;10M' * n), time.sleep(0.6))
        top = lambda: lines()[1].startswith(' ▾ alpha')
        assert 'more' in screen() and '▼' in screen() and '▲' not in screen() and 'pc-last' not in screen(), screen()
        wheel(False, 3); assert '▲' in screen() and not top(), screen()  # a few notches scroll by rows, not by a page
        wheel(False, 60)
        assert 'pc-last' in screen() and '▲' in screen() and '▼' not in screen(), screen()
        click(*side('pc-last')); assert until(lambda: 'pc / ● pc-last' in status() and 'ready in pc-last' in screen()), screen()
        wheel(True, 80); assert top() and '▲' not in screen() and '▼' in screen(), screen()
        click(*find('▼')); assert '▲' in screen() and not top(), screen()  # a click on the marker pages
        keys('C-b', '1'); assert until(lambda: 'pc-01' in status()), status()
        assert until(lambda: any(l.startswith('   ') and 'pc-01' in l[:26] for l in lines())), screen()  # its row is brought into view
        wheel(True, 80); assert top()
        keys('C-b', 'w')
        assert not top() and any(l.startswith('   ') and 'pc-01' in l[:26] for l in lines()), screen()  # the selection starts on the current agent: scrolled to it
        keys(*['Down'] * 70)
        assert '▼' not in screen() and '▲' in screen(), screen()
        keys(*['Up'] * 90)
        assert top() and '▲' not in screen() and '▼' in screen(), screen()
        keys('Escape')
        for n in ('alpha', 'beta', 'pa', 'pb', 'pc'):  # a click on a project opens it, the next one collapses it
            wheel(True, 80); click(*find(f'▾ {n}'))
            if f'▾ {n}' in screen(): click(*find(f'▾ {n}'))
        assert 'more' not in screen() and '▸ pc' in screen(), screen()  # collapsed: all fit, no markers
    case('the sidebar scrolls: wheel, keyboard, the current agent, and the more lines', sidebar_scroll)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    vt('kill-server')
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
