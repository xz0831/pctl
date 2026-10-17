#!/usr/bin/env python3
"""Image paste in the pctl app (key `image_paste`, default ctrl+v), seen and driven like a person: the
image on the clipboard of this computer reaches the clipboard of a saved machine, then ctrl+v reaches
its agent. The "other computer" is a second isolated state reached through a fake ssh (PCTL_SSH) that
runs the bridge locally. No real clipboard is touched: PCTL_CLIP_READ and PCTL_CLIP_WRITE (see
src/clipboard.rs) point at files. The agents are raw-terminal programs that print every byte they get."""
import argparse, base64, json, os, pathlib, shutil, stat, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-clip-', dir=os.path.realpath('/tmp')))
state, remote, cfg = root / 'local', root / 'remote', root / 'cfg'
V = root / 'v.sock'; bindir = root / 'bin'; bindir.mkdir(); results = []
local_clip, remote_clip, writes = root / 'clip-local.png', root / 'clip-remote.png', root / 'clip-writes'
hello = json.dumps({"ev": "hello", "protocol": 1, "host": "other-mac", "version": "9.9.9"})
ssh = bindir / 'ssh'
ssh.write_text(f'''#!/bin/sh
# Test ssh: ignore options and the target, run the last argument with the other state.
for a in "$@"; do line="$a"; done
PCTL_STATE={remote} exec /bin/sh -c "$line"
''')
ssh.chmod(0o755)
(bindir / 'clipread').write_text(f'#!/bin/sh\ncat {local_clip}\n')
(bindir / 'clipwrite').write_text(f'#!/bin/sh\ncat > {remote_clip} && echo write >> {writes}\n')
for tag in ('L', 'R'):  # a raw terminal that prints each byte it receives
    (bindir / f'raw{tag}').write_text(f'#!/bin/bash\necho "ready raw {tag}"\nstty -icanon -echo -isig -ixon -iexten min 1\nwhile true; do b=$(dd bs=1 count=1 2>/dev/null | od -An -tu1 | tr -d " "); echo "BYTE $b"; done\n')
for f in bindir.iterdir(): f.chmod(0o755)
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'WAYLAND_DISPLAY', 'DISPLAY'))}
ENV.update(XDG_CONFIG_HOME=str(cfg), TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ')
machines = cfg / 'pctl' / 'machines'; machines.parent.mkdir(parents=True)
machines.write_text('rm\tfake\tenabled\n')
config = cfg / 'pctl' / 'config.toml'
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
def app():
    vt('kill-server')
    envs = f'PCTL_SSH={ssh} PCTL_REMOTE_BIN={B} PCTL_TMUX={T} PCTL_CLIP_READ={bindir}/clipread PCTL_CLIP_WRITE={bindir}/clipwrite'
    cmd = f"env -u TMUX {envs} {B} --state {state} --tmux {T}; echo EXITED $?; sleep 600"
    subprocess.run([T, '-S', str(V), '-f', '/dev/null', 'new-session', '-d', '-x', '110', '-y', '26', '-s', 'view', cmd], env=ENV, check=True, timeout=10)
def click(x, y, button=0):
    vt('send-keys', '-t', 'view', '-l', f'\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m'); time.sleep(0.4)
def keys(*k): vt('send-keys', '-t', 'view', *k); time.sleep(0.4)
def side(name, under):
    ls = lines(); start = next(i for i, l in enumerate(ls) if under in l[:26] and ('▾' in l[:26] or '▸' in l[:26]))
    for i in range(start + 1, len(ls)):
        if name in ls[i][:26] and ls[i].startswith('    '): return ls[i].index(name) + 1, i + 1
        if ('▾' in ls[i][:26] or '▸' in ls[i][:26]): break
    raise AssertionError(f'{name} not under {under}:\n{screen()}')
def add(st, name, project, prog):
    d = root / 'proj' / project / name; d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, '--', bindir / prog, st=st)
def png(size, seed):
    """A PNG signature, then pseudo-random bytes: big images that differ from each other."""
    body = bytearray(); x = seed
    while len(body) < size - 8:
        x = (x * 1103515245 + 12345) & 0x7fffffff; body += x.to_bytes(4, 'big')
    return b'\x89PNG\r\n\x1a\n' + bytes(body[:size - 8])
def reads(agent, st): return (rcli if st is remote else cli)('session', 'read', agent, '--lines', '200')
def bytes22(agent, st=remote): return reads(agent, st).count('BYTE 22')
def clear_remote():
    for f in (remote_clip, writes): f.unlink(missing_ok=True)
def piece_files(): return list(remote.glob('clip-*/*'))
class Bridge:
    """`pctl bridge` of the remote state, driven directly with JSON lines."""
    def __init__(self):
        self.p = subprocess.Popen([B, '--state', str(remote), '--tmux', T, 'bridge'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=dict(ENV, PCTL_STANDALONE='1', PCTL_CLIP_WRITE=f'{bindir}/clipwrite'))
        assert json.loads(self.p.stdout.readline())['ev'] == 'hello'; self.n = 0
    def ask(self, **op):
        self.n += 1; self.p.stdin.write(json.dumps(dict(op, id=self.n)) + '\n'); self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())
    def close(self): self.p.stdin.close(); self.p.wait(10)
try:
    add(state, 'l-raw', 'alpha', 'rawL'); add(remote, 'r-raw', 'rproj', 'rawR')
    app()
    assert until(lambda: 'rproj' in screen() and 'r-raw' in screen()), screen()
    def open_agent(name, project, ready):
        click(*side(name, project)); assert until(lambda: ready in screen()), screen()
    def to_remote():
        open_agent('r-raw', 'rproj', 'ready raw R')
        img = png(100_000, 1); local_clip.write_bytes(img); clear_remote(); before = bytes22('r-raw')
        keys('C-v')
        assert until(lambda: remote_clip.exists() and remote_clip.read_bytes() == img), 'the image reached the other computer\'s clipboard, byte for byte'
        assert until(lambda: bytes22('r-raw') == before + 1), reads('r-raw', remote)
        assert until(lambda: 'Sent image (98 KB) to rm' in status()), status()
        assert writes.read_text() == 'write\n'
        assert until(lambda: not piece_files()), piece_files()
    case('an image on this clipboard reaches the other computer\'s clipboard, then ctrl+v reaches its agent', to_remote)
    def no_image():
        local_clip.unlink(); clear_remote(); before = bytes22('r-raw')
        keys('C-v')
        assert until(lambda: bytes22('r-raw') == before + 1), reads('r-raw', remote)
        time.sleep(0.5)
        assert not remote_clip.exists() and not writes.exists() and not piece_files() and bytes22('r-raw') == before + 1
    case('with no image, ctrl+v is an ordinary key and the other clipboard is untouched', no_image)
    def local_agent():
        open_agent('l-raw', 'alpha', 'ready raw L')
        local_clip.write_bytes(png(50_000, 2)); clear_remote(); before = bytes22('l-raw', state)
        keys('C-v')
        assert until(lambda: bytes22('l-raw', state) == before + 1), reads('l-raw', state)
        time.sleep(0.5)
        assert not remote_clip.exists() and not piece_files() and bytes22('l-raw', state) == before + 1
    case('an agent on this computer gets ctrl+v only; its clipboard is this one', local_agent)
    def big():
        open_agent('r-raw', 'rproj', 'ready raw R')
        img = png(3_000_000, 3); local_clip.write_bytes(img); clear_remote(); before = bytes22('r-raw')
        keys('C-v')
        assert until(lambda: remote_clip.exists() and remote_clip.read_bytes() == img, 30), 'a 3 MB image arrives whole'
        assert until(lambda: bytes22('r-raw') == before + 1, 15), reads('r-raw', remote)
        assert until(lambda: 'Sent image (2930 KB) to rm' in status()), status()
    case('a 3 MB image (several pieces) arrives whole', big)
    def too_big():
        local_clip.write_bytes(png(21 << 20, 4)); clear_remote(); before = bytes22('r-raw')
        keys('C-v')
        assert until(lambda: 'over 20 MB' in status() and 'Image not sent' in status(), 15), screen()
        time.sleep(0.5)
        assert not remote_clip.exists() and not piece_files() and bytes22('r-raw') == before
    case('an image over 20 MB is refused with a message and nothing is sent, not even the key', too_big)
    def changed():
        local_clip.write_bytes(png(10_000, 5)); clear_remote(); before = bytes22('r-raw')
        s = rcli('session', 'get', 'r-raw'); rt('set-option', '-p', '-t', s['pane'], '@pctl_run', 'someone-else')
        keys('C-v')
        assert until(lambda: 'r-raw changed (restarted or moved)' in status()), status()
        time.sleep(0.5)
        rt('set-option', '-p', '-t', s['pane'], '@pctl_run', s['run_id'])
        assert not remote_clip.exists() and not piece_files() and bytes22('r-raw') == before
    case('a run that changed is refused before the clipboard is touched', changed)
    def off():
        config.write_text('[keys]\nimage_paste = ""\n'); app()
        assert until(lambda: 'rproj' in screen() and 'r-raw' in screen() and 'Shortcut config' not in screen()), screen()
        open_agent('r-raw', 'rproj', 'ready raw R')
        local_clip.write_bytes(png(10_000, 6)); clear_remote(); before = bytes22('r-raw')
        keys('C-v')
        assert until(lambda: bytes22('r-raw') == before + 1), reads('r-raw', remote)
        time.sleep(0.5)
        assert not remote_clip.exists() and not piece_files() and 'Sent image' not in status()
    case('an empty image_paste turns it off: ctrl+v is an ordinary key even with an image', off)
    def protocol():
        run = rcli('session', 'get', 'r-raw')['run_id']; pane = rcli('session', 'get', 'r-raw')['pane']
        ok = dict(agent='r-raw', run=run); clear_remote(); br = Bridge()
        def piece(seq, raw): return br.ask(op='clip_put', seq=seq, data=base64.b64encode(raw).decode(), **ok)
        assert 'came after' in piece(1, b'x')['error'], 'a piece out of order'
        assert 'not base64' in br.ask(op='clip_put', seq=0, data='not base64!', **ok)['error']
        assert 'at most 512 KB' in piece(0, b'x' * ((512 << 10) + 1))['error']
        assert piece(0, b'\x89PNG\r\n\x1a\n' + b'a' * 10)['ok'] and piece(1, b'b' * 10)['ok']
        d = remote / f'clip-{br.p.pid}'; f = d / 'image.png'
        assert stat.S_IMODE(d.stat().st_mode) == 0o700 and stat.S_IMODE(f.stat().st_mode) == 0o600, 'private folder and file'
        assert 'arrived' in br.ask(op='clip_set', kind='png', bytes=99, **ok)['error'] and not f.exists() and not remote_clip.exists(), 'wrong length'
        assert piece(0, b'GIF89a not a png')['ok']
        assert 'not a png' in br.ask(op='clip_set', kind='png', bytes=16, **ok)['error'] and not f.exists() and not remote_clip.exists()
        assert 'only png' in br.ask(op='clip_set', kind='gif', bytes=1, **ok)['error']
        rt('set-option', '-p', '-t', pane, '@pctl_run', 'someone-else')
        assert br.ask(op='clip_put', seq=0, data='AAAA', **ok)['error'].startswith('changed:')
        assert br.ask(op='clip_set', kind='png', bytes=10, **ok)['error'].startswith('changed:') and not remote_clip.exists()
        rt('set-option', '-p', '-t', pane, '@pctl_run', run)
        assert piece(0, b'\x89PNG\r\n\x1a\nzz')['ok'] and br.ask(op='clip_set', kind='png', bytes=10, **ok)['ok'] and remote_clip.read_bytes() == b'\x89PNG\r\n\x1a\nzz' and not f.exists()
        br.close(); assert not d.exists(), 'the folder goes with the bridge'
    case('the bridge ops check order, size, signature and run; files are private and removed', protocol)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    vt('kill-server')
    for st in (state, remote):
        subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
