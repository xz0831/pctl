#!/usr/bin/env python3
"""Splitting the pctl app's terminal area into cells, seen and driven like a person (as app_integration.py
does): the app runs inside an outer tmux, keys and SGR mouse sequences go in, its screen is read with
capture-pane. Covers split right and down, an empty cell, two agents in two cells with typing landing in
the right pane only, one agent never shown twice, focus by key and by click, resizing by key and by dragging
a divider (the agent's tmux window follows the cell), swap, zoom, closing a cell (the agent keeps running,
the last cell stays), the layout coming back when the app is reopened (and an old file without one loading),
and, through a fake ssh as in app_machines_integration.py, an agent of another computer in one cell and one
of this computer in another."""
import argparse, json, os, pathlib, re, shutil, signal, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-split-', dir=os.path.realpath('/tmp')))
state, remote, cfg = root / 'local', root / 'remote', root / 'cfg'
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); results = []
HOST = subprocess.run(['hostname', '-s'], text=True, capture_output=True).stdout.strip()
hello = json.dumps({"ev": "hello", "protocol": 1, "host": "other-mac", "version": "9.9.9"})
pids = root / 'pids'
(bindir / 'ssh').write_text(f'#!/bin/sh\necho $$ >> {pids}\nfor a in "$@"; do line="$a"; done\nPCTL_STATE={remote} exec /bin/sh -c "$line"\n')
(bindir / 'ssh').chmod(0o755)
for tag in ('L', 'R'):  # a fake agent: says which computer it runs on, echoes what it is given
    (bindir / f'plain{tag}').write_text(f'#!/bin/bash\necho "ready {tag} in $(basename "$PWD")"\necho "> "\nwhile IFS= read -r line; do echo "got {tag}: $line"; echo "> "; done\n')
    (bindir / f'plain{tag}').chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG'))}
ENV.update(XDG_CONFIG_HOME=str(cfg), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ')
machines = cfg / 'pctl' / 'machines'; machines.parent.mkdir(parents=True)
COLS, ROWS = 110, 26
SIDE = 27  # columns taken by the sidebar and its edge; the cells start at column 28
def cli(*args, st=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=60, env=dict(ENV, PCTL_STANDALONE='1'))
    assert q.returncode == 0, (args, q.returncode, q.stderr, q.stdout)
    return json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout
def rcli(*args): return cli(*args, st=remote)
def vt(*args): return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
def tm(st, *args): return subprocess.run([T, '-S', str(st / 'tmux.sock'), *map(str, args)], text=True, capture_output=True, timeout=10).stdout.strip()
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
def cells(): return [l[SIDE:] for l in lines()]  # only the terminal area
def status(): return lines()[ROWS - 1]
def tpos(name): m = re.search(rf' (?:\S+ · )?{re.escape(name)} [●!×○?]', '\n'.join(cells()[1:])); return m.start() if m else -1  # where a cell's title for the agent starts, in reading order
def tcount(name): return len(re.findall(rf' (?:\S+ · )?{re.escape(name)} [●!×○?]', '\n'.join(cells()[1:])))
def titles(): return cells()[1]  # the first row of the cells: the tab row is row 1
def app(machines_on=False):
    vt('kill-server')
    if machines_on: machines.write_text('rm\tfake\tenabled\n')
    elif machines.exists(): machines.unlink()
    envs = f'PCTL_SSH={bindir}/ssh PCTL_REMOTE_BIN={B} PCTL_TMUX={T}'
    cmd = f"env -u TMUX {envs} {B} --state {state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', str(COLS), '-y', str(ROWS), '-s', 'view', cmd], env=ENV, check=True, timeout=10)
    assert until(lambda: '+ agent' in screen()), screen()
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m'); time.sleep(0.4)
def drag(x1, y1, x2, y2):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x1};{y1}M'); time.sleep(0.2)
    for x in range(x1, x2, 1 if x2 > x1 else -1): vt('send-keys', '-t', 'view', '-l', f'\x1b[<32;{x};{y2}M')
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<32;{x2};{y2}M'); time.sleep(0.4)
def release(x, y): vt('send-keys', '-t', 'view', '-l', f'\x1b[<0;{x};{y}m'); time.sleep(0.4)
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def text(t): vt('send-keys', '-t', 'view', '-l', t); time.sleep(0.2)
def side(name, under=None):
    """1-based (column, row) of an agent in the sidebar, under the project row `under` when given."""
    ls = lines(); start = 0
    if under: start = next(i for i, l in enumerate(ls) if under in l[:26] and ('▾' in l[:26] or '▸' in l[:26]))
    for i in range(start, len(ls)):
        if ls[i].startswith('   ') and name in ls[i][:26]: return ls[i].index(name) + 1, i + 1
    raise AssertionError(f'{name} not in the sidebar:\n{screen()}')
def dividers(row=6): return [i + SIDE for i, c in enumerate(cells()[row - 1]) if c == '│']  # 0-based screen columns
def read(name, st=None, n=12): return cli('session', 'read', name, '--lines', n, st=st)
def leaves(node): return leaves(node['a']) + leaves(node['b']) if 'split' in node else [node]
def say(name, word, st=None):
    """Type a word and Enter, and wait until the agent got it."""
    text(word); keys('Enter')
    assert until(lambda: 'got' in read(name, st) and word in read(name, st)), (name, word, read(name, st))
def size(name, st=None): return tm(st or state, 'display-message', '-p', '-t', cli('session', 'get', name, st=st)['pane'], '#{pane_width}x#{pane_height}')
def add(st, name, project, prog):
    d = root / 'proj' / project / name; d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / prog, st=st)
try:
    add(state, 'a-main', 'alpha', 'plainL'); add(state, 'a-two', 'alpha', 'plainL'); add(state, 'b-main', 'beta', 'plainL')
    add(remote, 'r-main', 'rproj', 'plainR')
    def one_cell():
        app()
        assert until(lambda: 'ready L in a-main' in screen()), screen()
        assert size('a-main') == f'{COLS - SIDE}x{ROWS - 2}', size('a-main')
        assert '│' not in cells()[5] and tpos('a-main') < 0, 'a single cell has no title row and no divider'
    case('a single cell looks as before: no title row, no divider, the pane has the whole area', one_cell)
    def split_right():
        keys('C-b', '%')
        assert until(lambda: 'Pick an agent in the sidebar' in screen()), screen()
        d = dividers(); assert len(d) == 1, (d, screen())
        assert tpos('a-main') >= 0 and ' no agent' in titles(), titles()
        w = (COLS - SIDE - 1) // 2
        assert until(lambda: size('a-main') == f'{w}x{ROWS - 3}'), (size('a-main'), w)
        assert 'ready L in a-main' in cells()[2], 'the first cell keeps its agent'
    case('ctrl+b % splits right: a new empty cell with its placeholder, the agent resized to its cell', split_right)
    def two_agents():
        click(*side('b-main'))
        assert until(lambda: tpos('b-main') >= 0 and 'ready L in b-main' in screen()), screen()
        say('b-main', 'to-b')
        assert 'to-b' not in read('a-main'), 'typed into the focused cell only'
        keys('C-b', 'Left'); say('a-main', 'to-a')
        assert 'to-a' not in read('b-main') and 'to-b' not in read('a-main')
        click(*find_in_cells('ready L in b-main'))
        say('b-main', 'again-b'); assert 'again-b' not in read('a-main')
        assert 'alpha / ● b-main' in status() or 'b-main' in status(), status()
    def find_in_cells(t):
        for i, l in enumerate(lines()):
            if t in l[SIDE:]: return l.index(t, SIDE) + 1, i + 1
        raise AssertionError(f'{t!r} not in the cells:\n{screen()}')
    case('two agents in two cells: typing lands in the focused one only; a click focuses a cell', two_agents)
    def never_twice():
        click(*side('a-main'))
        time.sleep(0.5)
        assert tcount('a-main') == 1 and tcount('b-main') == 1, titles()
        say('a-main', 'focus-moved-to-a'); assert 'focus-moved-to-a' not in read('b-main')
        click(*side('a-two'))  # goes into the focused cell, replacing a-main there
        assert until(lambda: tpos('a-two') >= 0 and tpos('a-main') < 0), titles()
        click(*side('a-main'))
        assert until(lambda: tcount('a-main') == 1 and tpos('b-main') >= 0), titles()
    case('an agent already in a cell is focused, never shown twice; another agent replaces the focused cell', never_twice)
    def split_down_and_focus():
        app(); assert until(lambda: 'ready L in a-main' in screen())
        keys('C-b', '%'); click(*side('b-main')); assert until(lambda: tpos('b-main') >= 0), titles()
        keys('C-b', '"'); click(*side('a-two'))
        assert until(lambda: any(re.search(r' a-two [●!]', l) for l in cells()[3:])), screen()
        say('a-two', 'in-a-two')
        keys('C-b', 'Up'); say('b-main', 'in-b-main')
        keys('C-b', 'Left'); say('a-main', 'in-a-main')
        keys('C-b', 'Right'); say('b-main', 'right-again')
        keys('C-b', 'Down'); say('a-two', 'down-again')
        for n in ('a-main', 'b-main', 'a-two'):
            others = {'a-main': 'in-b-main in-a-two', 'b-main': 'in-a-main in-a-two', 'a-two': 'in-a-main in-b-main'}[n].split()
            assert not any(o in read(n) for o in others), (n, read(n))
    case('ctrl+b " splits down; ctrl+b arrows move the focus across three cells', split_down_and_focus)
    def resize_keys():
        w0 = size('a-main'); keys('C-b', 'Left'); keys('C-b', 'M-Right'); keys('C-b', 'M-Right')
        assert until(lambda: int(size('a-main').split('x')[0]) > int(w0.split('x')[0]) + 6), (w0, size('a-main'))
        keys('C-b', 'M-Left'); keys('C-b', 'M-Left'); keys('C-b', 'M-Left')
        assert until(lambda: int(size('a-main').split('x')[0]) < int(w0.split('x')[0])), (w0, size('a-main'))
    case('ctrl+b alt+arrow moves the divider in 5% steps and the windows follow', resize_keys)
    def resize_drag():
        d = dividers(row=6)[0]; before = size('a-main')
        drag(d + 1, 8, d - 9, 8); release(d - 9, 8)
        assert until(lambda: int(size('a-main').split('x')[0]) == int(before.split('x')[0]) - 10), (before, size('a-main'))
        assert dividers(row=6)[0] == d - 10, (d, dividers(row=6))
        assert until(lambda: int(size('b-main').split('x')[0]) == (COLS - SIDE) - 1 - int(size('a-main').split('x')[0])), size('b-main')
        # The lower divider is the title row of the lower cell.
        y = next(i for i, l in enumerate(cells()) if re.search(r' a-two [●!]', l)) + 1
        h0 = int(size('b-main').split('x')[1]); x = SIDE + dividers(row=6)[0] - SIDE + 8
        drag(x, y, x, y - 3); release(x, y - 3)
        assert until(lambda: int(size('b-main').split('x')[1]) == h0 - 3), (h0, size('b-main'))
    case('dragging a divider resizes both sides and the agents\' windows', resize_drag)
    def swap_zoom():
        keys('C-b', 'Up'); keys('C-b', 'Left')  # a-main, the first cell
        first = titles()
        keys('C-b', '}')
        assert until(lambda: tpos('b-main') < tpos('a-main')), (first, titles())
        say('a-main', 'after-swap'); assert 'after-swap' not in read('b-main'), 'the focus went along with the agent'
        keys('C-b', '{'); assert until(lambda: 0 <= tpos('a-main') < tpos('b-main')), titles()
        keys('C-b', 'z')
        assert until(lambda: 'zoomed' in status() and size('a-main') == f'{COLS - SIDE}x{ROWS - 2}'), (status(), size('a-main'))
        assert '│' not in cells()[5] and tpos('a-main') < 0 and tpos('b-main') < 0, titles()
        say('a-main', 'while-zoomed'); keys('C-b', 'z')
        assert until(lambda: 'zoomed' not in status() and tpos('b-main') >= 0), (status(), titles())
        assert size('a-main') != f'{COLS - SIDE}x{ROWS - 2}'
    case('ctrl+b { and } swap with the neighbour; ctrl+b z zooms the cell full-size and back', swap_zoom)
    def keep_layout():
        before = (titles(), [dividers(r) for r in (6, 20)], size('a-main'), size('b-main'), size('a-two'))
        keys('C-b', 'q'); assert until(lambda: 'EXITED' in screen()), screen()
        saved = json.loads((state / 'app-view.json').read_text())
        assert saved['layouts'] and saved['project'] == 'alpha', saved
        app()
        assert until(lambda: any(re.search(r' a-two [●!]', l) for l in cells())), screen()
        time.sleep(1.5)
        after = (titles(), [dividers(r) for r in (6, 20)], size('a-main'), size('b-main'), size('a-two'))
        assert after == before, (before, after)
        say('a-main', 'after-reopen'); assert 'after-reopen' not in read('b-main')  # the focus came back too
    case('the layout, with the agent in each cell, comes back when the app is opened again', keep_layout)
    def close_cells():
        alive = lambda: all(cli('session', 'get', n)['status'] in ('idle', 'working') for n in ('a-main', 'b-main', 'a-two'))
        keys('C-b', '&'); assert until(lambda: tpos('a-main') < 0 and tpos('b-main') >= 0 and tpos('a-two') >= 0), titles()
        assert alive(), 'the closed cell\'s agent still runs'
        keys('C-b', '&')
        assert until(lambda: '│' not in cells()[5] and tpos('b-main') < 0), screen()  # one cell left: no titles
        assert alive()
        keys('C-b', '&')
        assert until(lambda: 'The last cell cannot be closed' in status()), status()
        assert until(lambda: size('a-two') == f'{COLS - SIDE}x{ROWS - 2}' or size('b-main') == f'{COLS - SIDE}x{ROWS - 2}'), (size('a-two'), size('b-main'))
    case('ctrl+b & closes the cell, never the agent; the last cell cannot be closed and fills the area', close_cells)
    def old_file():
        keys('C-b', 'q'); assert until(lambda: 'EXITED' in screen())
        (state / 'app-view.json').write_text(json.dumps({"machine": "", "project": "alpha", "tabs": {"alpha": "a-two"}, "remote_tabs": [], "theme": None}))
        app(); assert until(lambda: 'ready L in a-two' in screen()), screen()
        assert '│' not in cells()[5]
    case('a view file from before cells (no layout) still opens, on the tab it remembered', old_file)

    def wheel_per_cell():
        # A full-screen program that reads the mouse (SGR), as today's agents do, in the cell that is not focused.
        prog = root / 'w-mouse.py'; got = root / 'w-mouse.bytes'
        prog.write_text(f"""import os, sys, tty
sys.stdout.write('\\x1b[?1049h\\x1b[?1000h\\x1b[?1006hREADY-w-mouse\\r\\n'); sys.stdout.flush(); tty.setraw(0)
f = open({str(got)!r}, 'ab', buffering=0)
while True:
    b = os.read(0, 1024)
    if not b: break
    f.write(b)
""")
        d = root / 'proj' / 'alpha' / 'w-mouse'; d.mkdir(parents=True)
        cli('session', 'add', '--name', 'w-mouse', '--cwd', d, '--project', 'alpha', '--', 'python3', prog)
        app(); assert until(lambda: 'w-mouse' in screen()), screen()
        keys('C-b', '%'); click(*side('w-mouse')); assert until(lambda: 'READY-w-mouse' in screen()), screen()
        keys('C-b', 'Left'); before = read('a-two')
        vt('send-keys', '-t', 'view', '-l', '\x1b[<64;80;12M'); vt('send-keys', '-t', 'view', '-l', '\x1b[<65;80;12M')
        assert until(lambda: got.exists() and got.read_bytes().count(b'M') >= 2), got.read_bytes() if got.exists() else b''
        w = got.read_bytes()
        assert w.startswith(b'\x1b[<64;11;10M') and b'\x1b[<65;11;10M' in w, w
        assert read('a-two') == before, 'the focused cell got nothing from a wheel over the other'
        say('a-two', 'still-focused-left'); assert 'still-focused-left' not in w.decode(errors='replace')
    case('the wheel goes to the cell under the pointer (here the one without the focus), with cell-relative coordinates', wheel_per_cell)

    # ---- an agent of another computer next to one of this computer ----
    def remote_cells():
        app(machines_on=True)
        assert until(lambda: 'rproj' in screen() and 'r-main' in screen()), screen()
        keys('C-b', '%')
        click(*side('r-main'))
        assert until(lambda: tpos('r-main') >= 0 and 'rm · r-main' in titles() and f'{HOST} · a-two' in titles() and 'ready R in r-main' in screen()), titles() + '\n' + screen()
        say('r-main', 'to-remote', st=remote); assert 'to-remote' not in read('a-two')
        keys('C-b', 'Left'); say('a-two', 'to-local'); assert 'to-local' not in read('r-main', st=remote)
        # The remote cell's computer goes down: that cell says so and takes no typing; the local cell works on.
        for pid in pids.read_text().split():
            try: os.kill(int(pid), signal.SIGKILL)
            except ProcessLookupError: pass
        keys('C-b', 'Right')
        assert until(lambda: 'rm is down' in screen() or 'rm is still connecting' in screen(), 15), screen()
        text('typed-while-down'); keys('Enter')
        keys('C-b', 'Left'); say('a-two', 'local-while-down')
        assert until(lambda: 'rm is down' not in screen() and 'ready R in r-main' in screen(), 30), 'it comes back on its own'
        time.sleep(1); assert 'typed-while-down' not in read('r-main', st=remote), 'nothing typed while it was down arrives later'
        keys('C-b', 'Right'); say('r-main', 'after-down', st=remote)
        keys('C-b', 'q'); assert until(lambda: 'EXITED' in screen())
        saved = json.loads((state / 'app-view.json').read_text())
        assert {'machine': 'rm', 'agent': 'r-main'} in leaves(saved['layouts'][0]['tree']), saved
        app(machines_on=True)
        assert until(lambda: 'rm · r-main' in titles() and 'ready R in r-main' in screen()), titles()
        keys('C-b', 'Right'); say('r-main', 'back-again', st=remote)
    case('a remote agent in one cell and a local one in the other: each takes its own typing; a down machine refuses typing in its cell only; both come back', remote_cells)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server')
    for st in (state, remote):
        subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
