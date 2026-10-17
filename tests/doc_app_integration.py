#!/usr/bin/env python3
"""Documents in the pctl app, seen and driven like a person (as app_integration.py does). An agent runs
`pctl show`: its row appears in the sidebar as `new` with one notice, and nothing opens or moves. A click
opens a document cell right of the agent that showed it, running `pctl view` as a reader session; opening
marks it read; closing the cell ends the reader, not the document. In the cell: read and copy long Hangul,
code and a wide table, ask about a sentence (it reaches the project main through tell), an answer arriving
while reading moves nothing, the source sentence changing keeps the original at question time, reopening.
Then the same for a document on another computer through a fake ssh. Fake agents only."""
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
def viewers(st=None): return [s['name'] for s in sessions(st) if s['viewer']]
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

PLAN = '''# 계획 문서

이 구현은 메모리 사용량이 줄어들기 때문에 유리하다. 긴 한글 문장이 이어집니다. 가나다라마바사아자차카타파하 가나다라마바사아자차카타파하 가나다라마바사아자차카타파하.

다음 문단은 별도 주장입니다.

```
let answer = compute(42);
```

| 이름 | 설명 | 값 | 비고 비고 비고 비고 | 더 긴 열 더 긴 열 더 긴 열 | 마지막 |
| --- | --- | --- | --- | --- | --- |
| 하나 | 첫 번째 줄 설명 | 1 | alpha beta gamma delta | epsilon zeta eta theta iota kappa | end |
'''
doc = home / 'Projects' / 'alpha' / 'plan.md'
try:
    add(state, 'alpha-main', 'alpha'); add(state, 'alpha-two', 'alpha')
    assert until(lambda: all('ready in' in cli('session', 'read', n, '--lines', '5') for n in ('alpha-main', 'alpha-two')))
    doc.write_text(PLAN)
    def shown_not_taken():
        app()
        assert until(lambda: 'ready in alpha' in screen()), screen()
        before = (screen().split('\n')[2:ROWS - 1], status())
        d = cli('show', doc, '--title', 'Plan', '--note', 'read the table', env=as_agent('alpha-main'))
        assert d['by'] == 'alpha-main' and d['project'] == 'alpha' and d['new'], d
        assert until(lambda: '📄 Plan' in screen() and ' new' in screen()), screen()
        row = next(l for l in lines() if '📄 Plan' in l)
        assert row.startswith('   ') and 'new' in row[:27], row
        assert until(lambda: 'alpha-main: wants you to read Plan' in notices()), notices()
        assert viewers() == [], 'nothing was started'
        assert len([l for l in lines() if '│' in l[27:30]]) == 0, 'no new cell'
        assert 'ready in alpha' in screen(), 'the agent cell is still there'
        keys('Enter'); text('typed after'); keys('Enter')
        assert until(lambda: 'got: typed after' in cli('session', 'read', 'alpha-main', '--lines', '10')), 'keys still go to the agent'
        assert cli('show', '--list')[0]['new'] is True, 'still unread'
    case('a shown document is a `new` row and one notice; nothing opens, focus stays on the agent', shown_not_taken)

    def click_opens_cell():
        click(*find('Plan'))
        assert until(lambda: viewers() != [] and '계획 문서' in screen()), screen()
        name = viewers()[0]
        s = cli('session', 'get', name)
        assert s['project'] == 'alpha' and s['viewer'] is True, s
        assert until(lambda: 'new' not in next(l for l in lines() if '📄 Plan' in l)[:27]), screen()
        assert cli('show', '--list')[0]['new'] is False
        title = next(l for l in lines() if '📄 Plan' in l[27:])
        assert 'alpha-main' in lines()[1] or 'alpha-main' in '\n'.join(lines()[:3]), screen()
        left = [l[27:68] for l in lines()[2:ROWS - 1]]
        assert any('ready in alpha' in l or 'got:' in l for l in left), 'the agent cell stays on the left\n' + screen()
        assert any('계획 문서' in l[68:] for l in lines()), 'the document is in a cell on its right\n' + screen()
        assert name not in '\n'.join(l[:27] for l in lines()), 'the reader is not a row of agents'
    case('a click opens the document right of the agent that showed it, as a `pctl view` reader; it is read', click_opens_cell)

    def read_and_copy():
        s = cell_text()
        for t in ('let answer = compute(42);', '가나다라마바사', '이름'):
            assert t in s, (t, s)
        (root / 'clip.out').unlink(missing_ok=True)
        keys('Tab', 'Tab'); keys('y')
        assert until(lambda: (root / 'clip.out').exists() and (root / 'clip.out').read_text().strip()), screen()
        got = (root / 'clip.out').read_text()
        assert '긴 한글 문장' in got or '메모리' in got, got
        assert until(lambda: 'Copied' in status() or 'Copied' in screen()), screen()
    case('scene 1: long Hangul, code and a wide table read in the cell; copy goes through the clipboard command', read_and_copy)

    req = {}
    def ask_main():
        keys('Escape'); keys('Enter')  # the focus is still on the sentence block from the copy
        assert until(lambda: 'Ask the agent' in screen()), screen()
        keys('Down', 'Down', 'Down', 'Down', 'Enter')
        assert until(lambda: 'Enter send' in screen()), screen()
        text('이 판단의 근거가 무엇인가요?'); keys('Enter')
        assert until(lambda: 'mdv 질문' in cli('session', 'read', 'alpha-main', '--lines', '40'), 15), cli('session', 'read', 'alpha-main', '--lines', '40')
        read = cli('session', 'read', 'alpha-main', '--lines', '40')
        assert 'show --answer' in read and '질문 당시 원문' in read, read
        ev = [json.loads(l) for l in (root / 'threads.jsonl').read_text().splitlines()]
        ask = next(e for e in ev if e['kind'] == 'ask')
        req['id'] = ask['req_id']; req['quote'] = ask['quote']
        assert '메모리 사용량이' in ask['quote'] and ask['source'] == 'terminal', ask
    case('scene 2: a question about a sentence reaches the project main through tell', ask_main)

    def answer_moves_nothing():
        keys('Escape'); time.sleep(0.5)
        flat = lambda: dcell().replace(' ', '')
        top = lambda: doc_rows()[:3]  # the first rows the reader shows
        before = (top(), len(viewers()), lines()[0], [l[:27] for l in lines()])
        cli('show', '--answer', req['id'], '근거는 이렇습니다', env=as_agent('alpha-main'))
        assert until(lambda: 'Answerready:근거는' in flat(), 8), screen()
        assert (top(), len(viewers()), lines()[0]) == before[:3], 'the reader stayed where it was; no cell opened or closed'
        assert [l[:27] for l in lines()] == before[3], 'the list only got refreshed'
        assert 'got:' in screen(), 'the agent cell is still there'
    case('scene 3: an answer arriving while reading moves nothing', answer_moves_nothing)

    def source_changes():
        doc.write_text(PLAN.replace('메모리 사용량이', '처리 지연이'))
        assert until(lambda: '처리지연이' in dcell().replace(' ', ''), 10), screen()
        keys('G'); time.sleep(0.5)
        flat = dcell().replace(' ', '')
        assert '질문당시원문' in flat and '메모리사용량이' in flat and '연결끊김' in flat, screen()
    case('scene 4: the source sentence changes and the original at question time is kept', source_changes)

    def close_and_reopen():
        name = viewers()[0]
        keys('C-b', '&')
        assert until(lambda: viewers() == [], 10), (viewers(), screen())
        assert 'got:' in screen() or 'ready in alpha' in screen(), 'the agent cell is still shown'
        assert len(cli('show', '--list')) == 1 and doc.exists(), 'the document stays'
        assert '📄 Plan' in screen()
        click(*find('Plan'))
        assert until(lambda: viewers() != [] and '계획 문서' in screen()), screen()
        keys('G'); time.sleep(0.5)
        flat = dcell().replace(' ', '')
        assert '근거는' in flat and '질문당시원문' in flat, screen()
    case('scene 5: closing the cell ends the reader, not the document; reopening shows the same discussion', close_and_reopen)

    def leave_ends_the_reader():
        assert viewers() != []
        click(*find('⏏ Leave'))
        assert until(lambda: 'EXITED 0' in screen()), screen()
        assert until(lambda: viewers() == [], 10), viewers()
        assert len(cli('show', '--list')) == 1 and doc.exists(), 'the document stays'
        assert 'ready in alpha' in cli('session', 'read', 'alpha-main', '--lines', '40') or 'got' in cli('session', 'read', 'alpha-main', '--lines', '40'), 'agents keep running'
    case('Leave ends the readers of this computer and stops no agent', leave_ends_the_reader)

    # ---- the same for a document of another computer ----
    add(remote, 'beta-main', 'beta')
    assert until(lambda: 'ready in' in cli('session', 'read', 'beta-main', '--lines', '5', st=remote))
    rdoc = home / 'Projects' / 'beta' / 'remote.md'; rdoc.write_text('# 원격 문서\n\n다른 컴퓨터의 문장입니다.\n')
    def remote_document():
        app(machines_on=True)
        assert until(lambda: 'beta-main' in screen()), screen()
        (bindir / 'notify.out').unlink(missing_ok=True)
        d = cli('show', rdoc, '--title', 'Remote note', env=as_agent('beta-main', remote), st=remote)
        assert until(lambda: '📄 Remote note' in screen() and 'new' in next(l for l in lines() if 'Remote note' in l)[:27], 15), screen()
        assert until(lambda: 'beta-main: wants you to read Remote note' in notices(), 10) or 'wants you to read Remote note' in notices(), notices()
        assert viewers(remote) == [], 'nothing started over there'
        click(*find('Remote note'))
        assert until(lambda: viewers(remote) != [] and '원격 문서' in screen(), 20), screen()
        s = cli('session', 'get', viewers(remote)[0], st=remote)
        argv = next(x['argv'] for x in json.loads((remote / 'registry.json').read_text())['agents'] if x['name'] == s['name'])
        assert s['viewer'] is True and argv[1] == 'view' and argv[-1] == '--remote', argv
        assert cli('show', '--list', st=remote)[0]['new'] is False
        (root / 'clip.out').unlink(missing_ok=True)
        # The reader's status line is `<host> | <project> | <pct> | <message>`; in a narrow cell a long host name cuts the message.
        vt('resize-window', '-t', 'view', '-x', '150'); time.sleep(1)
        keys('Tab', 'Tab'); keys('y')
        assert until(lambda: 'Copy failed: drag' in screen(), 8), screen()
        assert not (root / 'clip.out').exists(), 'no clipboard command ran for a document that is over ssh'
        keys('C-b', '&')
        assert until(lambda: viewers(remote) == [], 15), viewers(remote)
    case('a document of another computer: row, notice, reader over there, honest copy, close', remote_document)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server')
    for st in (state, remote): subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
