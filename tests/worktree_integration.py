#!/usr/bin/env python3
"""Worktree workers over real tmux and git with fake harnesses: `session add --worktree` makes a
branch and a worktree beside the repo and runs the session there; every refusal leaves no session,
branch or worktree behind; `--model` gives an OpenCode session a private server and its model
without writing a file; `worktree list` reports dirty, ahead and merged; `worktree remove` refuses
a running, dirty, unmerged, locked, moved, missing or symlinked worktree and otherwise moves the folder and its
own git record to the trash (no repo-wide prune: another worktree that is away keeps its record),
deletes the merged branch and unregisters the session."""
import argparse, json, os, pathlib, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-wt-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
home = root / 'home'; home.mkdir(); bindir = root / 'bin'; bindir.mkdir()
FAKE = '#!/bin/bash\necho "pwd: $PWD"; echo "args: $*"; echo "cfg: $OPENCODE_CONFIG_CONTENT"; echo "> "\nwhile IFS= read -r line; do echo "got: $line"; echo "> "; done\n'
for n in ('plain', 'opencode'):
    (bindir / n).write_text(FAKE); (bindir / n).chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('GIT_', 'PCTL_'))}
ENV.update(PCTL_STANDALONE='1', HOME=str(home), GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
           GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.invalid', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.invalid')
def run(*args, cwd=None, ok=True):
    q = subprocess.run(list(map(str, args)), cwd=cwd, text=True, capture_output=True, timeout=60, env=ENV)
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    return q
def cli(*args, ok=True):
    q = run(B, '--state', state, '--tmux', T, *args, ok=ok)
    return (json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout) if ok else q.stderr
def git(*args, ok=True): return run('git', '-C', *args, ok=ok).stdout.strip()
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
repo = root / 'repo'; repo.mkdir()
git(repo, 'init', '-q', '-b', 'main'); (repo / 'a.txt').write_text('a\n'); git(repo, 'add', '.'); git(repo, 'commit', '-q', '-m', 'first')
wt = lambda n: root / f'repo-wt-{n}'
def names(): return {s['name'] for s in cli('session', 'list')}
def snapshot(): return (names(), git(repo, 'worktree', 'list', '--porcelain'), git(repo, 'branch', '--list'))
def screen(n): return cli('session', 'read', n, '--lines', '20')
try:
    def create():
        v = cli('session', 'add', '--name', 'w-one', '--cwd', repo, '--worktree', 't/one', '--', bindir / 'plain')
        assert v['cwd'] == str(wt('w-one')) and v['project'] == 'repo' and v['branch'] == 't/one', v
        assert str(wt('w-one')) in git(repo, 'worktree', 'list') and git(wt('w-one'), 'branch', '--show-current') == 't/one'
        assert git(repo, 'rev-parse', 't/one') == git(repo, 'rev-parse', 'main'), 'branch starts at the base'
        assert git(repo, 'config', '--get', 'branch.t/one.remote', ok=False) == '', 'no upstream'
        assert until(lambda: f'pwd: {wt("w-one")}' in screen('w-one')), screen('w-one')
    case('add --worktree creates branch and worktree beside the repo; the session runs there', create)
    def refusals():
        before = snapshot()
        def refused(*args, expect):
            pre = snapshot(); err = cli('session', 'add', *args, ok=False)
            assert expect in err, (args, err)
            assert snapshot() == pre, (args, 'left something behind')
        refused('--name', 'w-dup', '--cwd', repo, '--worktree', 't/one', '--', bindir / 'plain', expect='branch t/one already exists')
        wt('w-folder').mkdir(); refused('--name', 'w-folder', '--cwd', repo, '--worktree', 't/folder', '--', bindir / 'plain', expect='already exists'); wt('w-folder').rmdir()
        (repo / 'new.txt').write_text('x')
        refused('--name', 'w-dirty', '--cwd', repo, '--worktree', 't/dirty', '--', bindir / 'plain', expect='dirty base')
        refused('--name', 'w-dirty', '--cwd', repo, '--worktree', 't/dirty', '--base', 'main', '--', bindir / 'plain', expect='dirty base')
        (repo / 'new.txt').unlink()
        git(repo, 'checkout', '-q', '--detach')
        refused('--name', 'w-det', '--cwd', repo, '--worktree', 't/det', '--', bindir / 'plain', expect='detached HEAD')
        git(repo, 'checkout', '-q', 'main')
        refused('--name', 'w-bad', '--cwd', repo, '--worktree', 'bad..name', '--', bindir / 'plain', expect='invalid branch name')
        refused('--name', 'w-bad', '--cwd', repo, '--worktree=-x', '--', bindir / 'plain', expect='invalid branch name')
        refused('--name', 'w-bad', '--cwd', repo, '--worktree', 't/bad', '--base', 'nope', '--', bindir / 'plain', expect='not a commit')
        refused('--name', 'w/slash', '--cwd', repo, '--worktree', 't/slash', '--', bindir / 'plain', expect='session name of letters')
        refused('--name', 'w-oc', '--cwd', repo, '--worktree', 't/oc', '--model', 'opencode/x', '--', bindir / 'plain', expect='--model is for opencode')
        refused('--name', 'w-oc', '--cwd', repo, '--worktree', 't/oc', '--model', 'no-slash', '--', bindir / 'opencode', expect='invalid model')
        plain_dir = root / 'notgit'; plain_dir.mkdir()
        refused('--name', 'w-ng', '--cwd', plain_dir, '--worktree', 't/ng', '--', bindir / 'plain', expect='needs a git checkout')
        assert '--worktree' in cli('session', 'add', '--name', 'w-b', '--cwd', repo, '--base', 'main', '--', bindir / 'plain', ok=False), '--base needs --worktree'
        # A path git still lists (locked, folder gone) is refused until unlocked and pruned.
        git(repo, 'worktree', 'add', '-q', '--lock', '-b', 't/locked', wt('w-locked')); git(repo, 'branch', '-q', '-m', 't/locked', 't/locked2')
        os.rename(wt('w-locked'), root / 'moved-away')
        refused('--name', 'w-locked', '--cwd', repo, '--worktree', 't/locked', '--', bindir / 'plain', expect='(locked)')
        git(repo, 'worktree', 'unlock', wt('w-locked')); git(repo, 'worktree', 'prune'); git(repo, 'branch', '-q', '-D', 't/locked2')
        assert snapshot() == before
    case('refusals (branch, folder, dirty base, detached, names, model, not git, listed path) leave nothing behind', refusals)
    def model():
        v = cli('session', 'add', '--name', 'w-oc', '--cwd', repo, '--worktree', 't/oc', '--model', 'opencode/glm-5.3-flash-free', '--', bindir / 'opencode')
        argv = next(x['argv'] for x in json.loads((state / 'registry.json').read_text())['agents'] if x['name'] == 'w-oc')
        assert argv == ['env', 'OPENCODE_CONFIG_CONTENT={"model":"opencode/glm-5.3-flash-free"}', str(bindir / 'opencode'), '--standalone'], argv
        assert until(lambda: 'cfg: {"model":"opencode/glm-5.3-flash-free"}' in screen('w-oc') and 'args: --standalone' in screen('w-oc')), screen('w-oc')
        assert git(v['cwd'], 'status', '--porcelain') == '' and not (pathlib.Path(v['cwd']) / 'opencode.json').exists(), 'nothing written'
    case('--model starts OpenCode on a private server with the model in OPENCODE_CONFIG_CONTENT; no file written', model)
    def listing():
        w = wt('w-one'); (w / 'b.txt').write_text('b\n'); git(w, 'add', 'b.txt'); git(w, 'commit', '-q', '-m', 'b'); (w / 'scratch.txt').write_text('s')
        v = {x['session']: x for x in cli('worktree', 'list')}
        assert set(v) == {'w-one', 'w-oc'}, v
        one = v['w-one']
        assert (one['branch'], one['base'], one['path'], one['checked_out']) == ('t/one', 'main', str(w), 't/one'), one
        assert (one['dirty'], one['ahead'], one['merged'], one['locked'], one['exists']) == (1, 1, False, False, True), one
        assert one['status'] in ('idle', 'unknown', 'working'), one
        assert v['w-oc']['ahead'] == 0 and v['w-oc']['merged'] is True
    case('worktree list: branch, base, dirty files, commits ahead, merged, locked', listing)
    def remove_gates():
        w = wt('w-one')
        def refused(expect):
            err = cli('worktree', 'remove', 'w-one', ok=False)
            assert expect in err, err
            assert w.is_dir() and 'w-one' in names() and 't/one' in git(repo, 'branch', '--list'), 'nothing removed'
        refused('has_run')
        s = cli('session', 'get', 'w-one'); cli('agent', 'terminate', '--agent', s['agent_id'], '--expected-run', s['run_id'])
        refused('dirty'); (w / 'scratch.txt').unlink()
        refused('unmerged')
        git(repo, 'merge', '-q', '--no-ff', '-m', 'merge one', 't/one')
        git(repo, 'worktree', 'lock', w); refused('locked'); git(repo, 'worktree', 'unlock', w)
        git(w, 'switch', '-q', '-c', 't/other'); refused('moved'); git(w, 'switch', '-q', 't/one'); git(repo, 'branch', '-q', '-D', 't/other')
        w.rename(root / 'away'); assert 'missing' in cli('worktree', 'remove', 'w-one', ok=False); (root / 'away').rename(w)
        assert 'w-one' in names() and str(w) in git(repo, 'worktree', 'list') and git(w, 'status', '--porcelain') == '', 'a missing folder removes nothing'
        # The folder was moved and a link left in its place: removing would trash the link and the
        # moved folder's git record, breaking the real folder.
        w.rename(root / 'moved-one'); w.symlink_to(root / 'moved-one')
        assert 'symlink' in cli('worktree', 'remove', 'w-one', ok=False)
        w.unlink(); (root / 'moved-one').rename(w)
        assert git(w, 'branch', '--show-current') == 't/one' and 'w-one' in names(), 'a symlinked folder removes nothing'
        plain = cli('session', 'add', '--name', 'p-plain', '--cwd', repo, '--', bindir / 'plain')
        cli('agent', 'terminate', '--agent', plain['agent_id'], '--expected-run', plain['run_id'])
        assert 'has no worktree' in cli('worktree', 'remove', 'p-plain', ok=False)
        cli('session', 'remove', 'p-plain')
    case('remove refuses a running, dirty, unmerged, locked, moved, missing or symlinked worktree and a plain session', remove_gates)
    def remove():
        admin = pathlib.Path(git(wt('w-one'), 'rev-parse', '--absolute-git-dir'))
        (admin / 'modules' / 'sub').mkdir(parents=True); (admin / 'modules' / 'sub' / 'HEAD').write_text('unpushed\n')  # stands in for submodule data
        wt('w-oc').rename(root / 'oc-away')  # another worktree's folder is away (unmounted, moved) right now
        v = cli('worktree', 'remove', 'w-one')
        (root / 'oc-away').rename(wt('w-oc'))
        assert git(wt('w-oc'), 'branch', '--show-current') == 't/oc' and str(wt('w-oc')) in git(repo, 'worktree', 'list'), 'the away worktree kept its record'
        assert v['branch_deleted'] is True and v['trashed'], v
        moved = pathlib.Path(v['trashed']); gitdir = pathlib.Path(v['trashed_git_dir'])
        assert moved.parent == home / '.Trash' and moved.name.startswith('repo-wt-w-one-') and (moved / 'b.txt').read_text() == 'b\n', v
        assert gitdir == moved.with_name(moved.name + '.gitdir') and (gitdir / 'modules' / 'sub' / 'HEAD').read_text() == 'unpushed\n' and not admin.exists(), v
        assert not wt('w-one').exists() and str(wt('w-one')) not in git(repo, 'worktree', 'list')
        assert 't/one' not in git(repo, 'branch', '--list') and 'w-one' not in names()
        assert (repo / 'b.txt').read_text() == 'b\n', 'the merged work stays on main'
    case('remove moves a stopped, clean, merged worktree and its own git record to the trash, deletes the branch, unregisters', remove)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
