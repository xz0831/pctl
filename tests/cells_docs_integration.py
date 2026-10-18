#!/usr/bin/env python3
"""Cells you can close, a layout per project, and agents that open documents, driven like a person in the
app (as doc_app_integration.py does). Closing: the right-click menu of a cell's title row and its x close a cell
and stop no agent; a document cell's close ends its reader and keeps the document. Layouts: a split in one
project is gone when another project is picked and back when you return; "Show in this cell" / "Show beside"
in an agent's menu keep the cells on screen. `pctl show --open`: nothing opens without an app; beside opens
a cell right of the agent's own without moving the keyboard, a second one takes that cell; an agent not on
screen, `tab` and a full screen make a tab; a picture is not opened; the same for an agent of another
computer through a fake ssh. Fake agents only."""
import argparse, json, os, pathlib, re, shutil, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-docapp-', dir=os.path.realpath('/tmp')))
state, remote, cfg, home = root / 'local', root / 'remote', root / 'cfg', root / 'home'
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); results = []
for d in (home / 'Projects' / 'alpha', home / 'Projects' / 'beta'): d.mkdir(parents=True)
(bindir / 'plain').write_text('#!/bin/bash\necho "ready in $(basename "$PWD")"\necho "> "\nwhile IFS= read -r line; do echo "got: $line"; echo "> "; done\n')
(bindir / 'plain').chmod(0o755)
(bindir / 'notify').write_text('#!/bin/sh\nprintf "%s|" "$@" >> "$0.out"; echo >> "$0.out"\n'); (bindir / 'notify').chmod(0o755)  # PCTL_NOTIFY_SEND
# A fake ssh to "another computer": a second state, the variable sshd sets, and the same files.
(bindir / 'ssh').write_text(f'#!/bin/sh\nfor a in "$@"; do line="$a"; done\nSSH_CONNECTION="10.0.0.1 50000 10.0.0.2 22" PCTL_STATE={remote} exec /bin/sh -c "$line"\n')
(bindir / 'ssh').chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'VTE_VERSION', 'SSH_CONNECTION', 'TERM_PROGRAM', 'WAYLAND_DISPLAY', 'DISPLAY', 'NO_COLOR', 'MDV_'))}
ENV.update(XDG_CONFIG_HOME=str(cfg), HOME=str(home), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ',
           MDV_THREADS_FILE=str(root / 'threads.jsonl'), PCTL_CLIP_WRITE_TEXT=f'cat > {root}/clip.out', PCTL_NOTIFY_SEND=str(bindir / 'notify'))
machines = cfg / 'pctl' / 'machines'; machines.parent.mkdir(parents=True)
COLS, ROWS = 110, 30
def cli(*args, st=None, env=None):
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=60, env=dict(ENV, PCTL_STANDALONE='1', **(env or {})))
    assert q.returncode == 0, (args, q.returncode, q.stderr, q.stdout)
    return json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout
def vt(*args): return subprocess.run([T, '-S', str(V), *map(str, args)], text=True, capture_output=True, timeout=10).stdout
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
def status(): return lines()[ROWS - 1]
def app(machines_on=False):
    vt('kill-server')
    if machines_on: machines.write_text('rm\tfake\tenabled\n')
    elif machines.exists(): machines.unlink()
    envs = ' '.join(f'{k}={v}' for k, v in {'PCTL_SSH': bindir / 'ssh', 'PCTL_REMOTE_BIN': B, 'PCTL_TMUX': T, 'HOME': home, 'PCTL_NOTIFY_SEND': bindir / 'notify'}.items())
    cmd = f"env -u TMUX {envs} {B} --state {state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', str(COLS), '-y', str(ROWS), '-s', 'view', cmd], env=ENV, check=True, timeout=10)
    assert until(lambda: '+ agent' in screen()), screen()
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m'); time.sleep(0.4)
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def text(t): vt('send-keys', '-t', 'view', '-l', t); time.sleep(0.3)
def find(t, start=0):
    for i, line in enumerate(lines()):
        if t in line[start:]: return line.index(t, start) + 1 + len(t) // 2, i + 1
    raise AssertionError(f'{t!r} not on screen:\n{screen()}')
def dump(tag):
    if os.environ.get('DOCAPP_DEBUG'): print(f'--- {tag}\n{screen()}', flush=True)
def sessions(st=None): return cli('session', 'list', st=st)
def viewers(st=None): return [s['name'] for s in sessions(st) if s['viewer'] and s['process'] == 'running']
def as_agent(name, st=None):
    s = cli('session', 'get', name, st=st); return {'PCTL_AGENT_ID': s['agent_id'], 'PCTL_RUN_ID': s['run_id']}
def side_cols(): return 27  # the sidebar and its edge; cells start at column 28
def doc_rows():
    """The rows of the rightmost cell. Wide letters make columns differ from characters, so rows are split at the dividers."""
    return [l.split('│')[-1] for l in lines()[2:ROWS - 1] if l.count('│') >= 2]
def dcell():
    """The text of the rightmost cell with no line breaks, so a phrase the cell wrapped still matches."""
    return ''.join(r.strip() for r in doc_rows())
def cell_text(): return '\n'.join(l[side_cols():] for l in lines()[2:ROWS - 1])
def notices(): return (bindir / 'notify.out').read_text() if (bindir / 'notify.out').exists() else ''
def add(st, name, project, prog='plain'):
    d = home / 'Projects' / project; return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / prog, st=st)

def side(t):
    for i, line in enumerate(lines()):
        if t in line[:27]: return line.index(t) + 1 + len(t) // 2, i + 1
    raise AssertionError(f'{t!r} not in the sidebar:\n{screen()}')
def in_cells(t):
    for i, line in enumerate(lines()[1:ROWS - 1]):
        if t in line[27:]: return line.index(t, 27) + 1 + len(t) // 2, i + 2
    raise AssertionError(f'{t!r} not in the cells:\n{screen()}')
def cells(): return sum(l[27:].count('×') for l in lines()[1:ROWS - 1])
def titles(): return [m for l in lines()[1:ROWS - 1] for m in re.findall(r'(alpha-main|alpha-two|beta-main|doc-[0-9a-f]{8}|📄 [A-Za-z0-9 ]+?) ', l[27:]) if '×' in l[27:]]
def running(n, st=None): return cli('session', 'get', n, st=st)['process'] == 'running'
def tabbar(): return lines()[0]
def end_readers():
    for n in viewers():  # an app that was killed (not left) leaves its readers; end them like a reboot would
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-pane', '-t', cli('session', 'get', n)['pane']], capture_output=True)
        until(lambda: not running(n), 5)
def type_into(t): text(t); keys('Enter')
def got(n, t, st=None): return f'got: {t}' in cli('session', 'read', n, '--lines', '30', st=st)
def pick(item):
    click(*find(item)); 

doc1, doc2, png = (home / 'Projects' / 'alpha' / n for n in ('one.md', 'two.md', 'pic.png'))
doc1.write_text('# First note\n\nThe first beside document.\n'); doc2.write_text('# Second note\n\nThe second beside document.\n'); png.write_bytes(b'\x89PNG\r\n\x1a\n')
try:
    add(state, 'alpha-main', 'alpha'); add(state, 'alpha-two', 'alpha'); add(state, 'beta-main', 'beta')
    assert until(lambda: all('ready in' in cli('session', 'read', n, '--lines', '5') for n in ('alpha-main', 'alpha-two', 'beta-main')))

    def nothing_without_an_app():
        d = cli('show', doc1, '--title', 'Plain', '--open', 'beside', env=as_agent('alpha-main'))
        assert d['open'] == 'beside' and d['open_seq'] == 1 and d['new'], d
        time.sleep(1); assert viewers() == [], 'no app runs, so nothing opened'
        bad = subprocess.run([B, '--state', str(state), '--tmux', T, 'show', str(doc1), '--open', 'left'], capture_output=True, text=True, env=dict(ENV, PCTL_STANDALONE='1'), timeout=20)
        assert bad.returncode != 0, 'only beside and tab'
    case('without an app `pctl show --open` only records; only beside and tab are asked for', nothing_without_an_app)

    def close_menu_and_x():
        app(); assert until(lambda: 'ready in alpha' in screen()), screen()
        keys('C-b', '%'); assert until(lambda: cells() == 2, 5), screen()
        click(*side('alpha-two'))  # into the new, focused cell
        assert until(lambda: sorted(titles()) == ['alpha-main', 'alpha-two'], 6), (titles(), screen())
        dump('two cells')
        assert 'ready in alpha' in screen()
        # Right-click on the title row of the left cell: Close cell, Split right, Split down.
        x, y = in_cells('alpha-main'); click(x, y, 2)
        assert until(lambda: 'Close cell' in screen(), 5), screen()
        assert 'Split right' in screen() and 'Split down' in screen(), screen()
        assert re.search(r'Close cell +(prefix|ctrl\+b)[ +]?&', screen()), screen()
        click(*find('Close cell'))
        assert until(lambda: cells() == 0 and 'alpha-two' in screen(), 5), screen()
        assert running('alpha-main') and running('alpha-two'), 'the agents keep running'
        # The x of a title row closes that cell.
        keys('C-b', '%'); assert until(lambda: cells() == 2, 5), screen()
        click(*side('alpha-main'))
        assert until(lambda: sorted(titles()) == ['alpha-main', 'alpha-two'], 6), (titles(), screen())
        row = next(l for l in lines()[1:] if 'alpha-two' in l[27:] and '×' in l[27:])
        xs = [i for i in range(27, len(row)) if row[i] == '×']
        y = lines().index(row) + 1
        click(xs[-1] + 1, y)
        assert until(lambda: cells() == 0, 5), screen()
        assert running('alpha-main') and running('alpha-two')
    case('the right-click menu of a cell title and its x close the cell; the agent keeps running', close_menu_and_x)

    def document_cell_close():
        d = cli('show', doc1, '--title', 'First', env=as_agent('alpha-main'))
        assert until(lambda: '📄 First' in screen()), screen()
        click(*side('First'))
        assert until(lambda: viewers() != [] and 'First note' in screen(), 10), screen()
        x, y = in_cells('📄 First'); click(x, y, 2)
        assert until(lambda: 'Close (the document stays)' in screen(), 5), screen()
        assert 'Split right' not in screen(), 'a reader has no splits to offer'
        click(*find('Close (the document stays)'))
        assert until(lambda: viewers() == [], 10), (viewers(), screen())
        assert len(cli('show', '--list')) == 1 and doc1.exists(), 'the document stays'
        assert '📄 First' in screen()
    case('a document cell closes from its title menu: the reader ends, the document stays in the sidebar', document_cell_close)

    def layout_per_project():
        app(); assert until(lambda: 'ready in alpha' in screen()), screen()
        keys('C-b', '%'); assert until(lambda: cells() == 2, 5), screen()
        click(*side('alpha-two')); assert until(lambda: sorted(titles()) == ['alpha-main', 'alpha-two'], 6), screen()
        click(*side('beta-main'))
        assert until(lambda: cells() == 0 and 'ready in beta' in screen(), 6), screen()
        click(*side('alpha-main'))
        assert until(lambda: cells() == 2 and sorted(titles()) == ['alpha-main', 'alpha-two'], 6), screen()
        # Another project's agent into the cells on screen, by the menu; the project stays.
        click(*side('beta-main'), 2) if False else None
        x, y = side('beta-main'); click(x, y, 2)
        assert until(lambda: 'Show in this cell' in screen(), 5), screen()
        click(*find('Show in this cell'))
        assert until(lambda: sorted(titles()) == ['alpha-main', 'beta-main'] or sorted(titles()) == ['alpha-two', 'beta-main'], 6), (titles(), screen())
        assert cells() == 2, 'still two cells'
        shown = sorted(titles())
        left = next(n for n in ('alpha-main', 'alpha-two') if n not in shown)
        x, y = side(left); click(x, y, 2)
        assert until(lambda: 'Show beside' in screen(), 5), screen()
        click(*find('Show beside'))
        assert until(lambda: cells() == 3, 6), screen()
        assert left in titles() and 'beta-main' in titles(), titles()
    case('each project keeps its layout; a plain click follows the project, the agent menu can show it in these cells', layout_per_project)

    def beside_keeps_focus():
        end_readers(); app(); assert until(lambda: 'ready in alpha' in screen()), screen()
        keys('C-b', '%'); assert until(lambda: cells() == 2, 5), screen()
        click(*side('alpha-two')); assert until(lambda: sorted(titles()) == ['alpha-main', 'alpha-two'], 6), screen()
        # The focus is in alpha-two (the right cell). While the document opens, keys keep going there.
        time.sleep(1.2)  # the app has listed the documents once
        d = cli('show', doc2, '--title', 'Second', '--open', 'beside', env=as_agent('alpha-main'))
        for i in range(8):
            text(f'k{i}'); time.sleep(0.15)
        keys('Enter')
        assert until(lambda: cells() == 3 and 'Second note' in screen(), 10), screen()
        for i in range(4):
            text(f'm{i}'); time.sleep(0.1)
        keys('Enter')
        assert until(lambda: got('alpha-two', 'k0k1k2k3k4k5k6k7') and got('alpha-two', 'm0m1m2m3'), 8), cli('session', 'read', 'alpha-two', '--lines', '20')
        assert not got('alpha-main', 'k0k1k2k3k4k5k6k7'), 'nothing reached the agent beside which the reader opened'
        # Order: a's cell, the reader right of it, then the other cell.
        row = next(l[27:] for l in lines()[1:] if 'alpha-main' in l[27:] and 'alpha-two' in l[27:])
        assert row.index('alpha-main') < row.index('Second') < row.index('alpha-two'), row
        assert cli('show', '--list')[0]['new'] is True, 'still unread'
        # A second document from the same agent takes that cell.
        cli('show', doc1, '--title', 'First', '--open', 'beside', env=as_agent('alpha-main'))
        assert until(lambda: 'First note' in screen() and 'Second note' not in screen(), 10), screen()
        assert cells() == 3 and len(viewers()) == 1, (cells(), viewers())
        # Looking at the cell reads it.
        click(*in_cells('First note'))
        assert until(lambda: next(x for x in cli('show', '--list') if x['title'] == 'First')['new'] is False, 6), cli('show', '--list')
    case('--open beside opens right of the agent without moving the keyboard; the next one takes that cell; focusing it reads it', beside_keeps_focus)

    def tab_and_fallbacks():
        end_readers(); app(); assert until(lambda: 'ready in alpha' in screen()), screen()
        time.sleep(1.2)
        # --open tab: a tab of the project, no split, shown when picked.
        cli('show', doc2, '--title', 'Tabbed', '--open', 'tab', env=as_agent('alpha-main'))
        assert until(lambda: '📄 Tabbed' in tabbar(), 8), screen()
        assert cells() == 0 and viewers() == [], 'nothing opened'
        # An agent that is not on screen: beside becomes a tab.
        cli('show', doc1, '--title', 'Elsewhere', '--open', 'beside', '--project', 'alpha', env=as_agent('beta-main'))
        assert until(lambda: '📄 Elsewhere' in tabbar(), 8), screen()
        assert cells() == 0 and viewers() == []
        # A picture is recorded and announced, not opened.
        d = cli('show', png, '--title', 'Pic', '--open', 'beside', env=as_agent('alpha-main'))
        assert d['open_refused'].startswith('pictures open in the page') and 'open' not in d, d
        time.sleep(1.5); assert '📄 Pic' not in tabbar() and viewers() == [] and cells() == 0, screen()
        click(*find('📄 Tabbed', 0)); 
        assert until(lambda: viewers() != [] and 'Second note' in screen(), 10), screen()
        assert cells() == 0, 'picking the tab takes the cell, no split'
        assert next(x for x in cli('show', '--list') if x['title'] == 'Tabbed')['new'] is False
        # A right-click on a tab closes it.
        x, y = find('📄 Elsewhere'); click(x, y, 2)
        assert until(lambda: 'Close tab' in screen(), 5), screen()
        click(*find('Close tab')); assert until(lambda: '📄 Elsewhere' not in tabbar(), 5), screen()
    case('--open tab and an agent off screen make tabs; a picture is not opened; a tab opens in the focused cell', tab_and_fallbacks)

    # ---- an agent of another computer ----
    add(remote, 'rbeta-main', 'beta')
    assert until(lambda: 'ready in' in cli('session', 'read', 'rbeta-main', '--lines', '5', st=remote))
    rdoc = home / 'Projects' / 'beta' / 'remote.md'; rdoc.write_text('# Far note\n\nA sentence from another computer.\n')
    def remote_beside():
        end_readers(); app(machines_on=True)
        assert until(lambda: 'rbeta-main' in screen(), 15), screen()
        click(*side('rbeta-main')); assert until(lambda: 'ready in beta' in screen(), 10), screen()
        time.sleep(1.5)
        cli('show', rdoc, '--title', 'Far', '--open', 'beside', env=as_agent('rbeta-main', remote), st=remote)
        assert until(lambda: 'Far note' in screen() and viewers(remote) != [], 20), screen()
        assert cells() == 2, 'a cell right of the remote agent'
        assert cli('show', '--list', st=remote)[0]['new'] is True
    case('an agent of another computer opens a document beside itself through the document list', remote_beside)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server')
    for st in (state, remote): subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
