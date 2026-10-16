#!/usr/bin/env python3
"""Saved machines, the bridge and its client, with no real ssh.

The fake ssh (`PCTL_SSH`, and first on PATH) ignores ssh's options and the target, and runs the
remaining command locally under `sh -c` with a second isolated state in `PCTL_STATE`. Every pctl
here uses its own state and its own tmux server.
"""
import argparse, json, os, pathlib, pty, select, shutil, subprocess, tempfile, time
os.environ['PCTL_STANDALONE'] = '1'  # Only disposable test servers start themselves.
parser = argparse.ArgumentParser(); parser.add_argument('--bin', required=True); parser.add_argument('--tmux', required=True); parser.add_argument('--report', default='test-results/machine_integration.json'); args = parser.parse_args()
B = str(pathlib.Path(args.bin).resolve()); T = str(pathlib.Path(args.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-machine-', dir=os.path.realpath('/tmp')))
config, state, state2, state3 = root/'config', root/'local', root/'remote', root/'empty'
bindir = root/'bin'; marker = root/'ssh-mode'; pids = root/'pids'; arglog = root/'ssh-args'
bindir.mkdir(parents=True)
machines = pathlib.Path(str(config))/'pctl'/'machines'
ssh = bindir/'ssh'
hello = json.dumps({"ev": "hello", "protocol": 1, "host": "other-mac", "version": "9.9.9"})
ssh.write_text(f'''#!/bin/sh
# Test ssh: ignore options and the target, run the last argument locally.
echo $$ >> {pids}
printf '%s|' "$@" >> {arglog}; printf '\\n' >> {arglog}
case "$(cat {marker} 2>/dev/null || true)" in
  silent) exit 0 ;;
  garbage) printf 'not a hello\\n'; exit 0 ;;
  slow) printf '%s\\n' '{hello}'; exec sleep 60 ;;
esac
for a in "$@"; do line="$a"; done
PCTL_STATE={state2} exec /bin/sh -c "$line"
''')
ssh.chmod(0o700)
env = dict(os.environ, PATH=f'{bindir}:{os.environ["PATH"]}', PCTL_SSH=str(ssh), PCTL_REMOTE_BIN=B, PCTL_TMUX=T, XDG_CONFIG_HOME=str(config), PCTL_STATE=str(state))
for k in ('PCTL_AGENT_ID', 'PCTL_RUN_ID', 'TMUX'):  # ssh does not carry them; this suite may run in an agent's pane
    env.pop(k, None)
results = []

def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic()-t, 4)}); print('PASS', name, flush=True)

def cli(*a, ok=True, e=None):
    p = subprocess.run([B, '--state', str(state), '--tmux', T, *map(str, a)], text=True, capture_output=True, timeout=60, env=e or env)
    if ok:
        assert p.returncode == 0, (a, p.stderr, p.stdout)
        return json.loads(p.stdout)
    assert p.returncode, (a, p.stdout)
    return json.loads(p.stderr)

def saved():
    return machines.read_text() if machines.exists() else ''

def status(only=None, e=None):
    rows = cli('machine', 'status', e=e)['machines']
    return next((r for r in rows if r['label'] == only), rows[0]) if only else rows

def tm(*a):
    return subprocess.run([T, '-S', str(state2/'tmux.sock'), '-f', '/dev/null', *map(str, a)], text=True, capture_output=True, timeout=10).stdout

class Bridge:
    """`pctl bridge` on the remote state, one JSON object per line each way."""
    def __init__(self, st=state2):
        self.p = subprocess.Popen([B, '--state', str(st), '--tmux', T, 'bridge'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, env=env)
        self.id = 0
        assert self.line(15)['ev'] == 'hello'
    def line(self, timeout=15):
        assert select.select([self.p.stdout], [], [], timeout)[0], 'no answer from the bridge'
        return json.loads(self.p.stdout.readline())
    def send(self, obj):
        self.p.stdin.write((obj if isinstance(obj, str) else json.dumps(obj)) + '\n'); self.p.stdin.flush()
    def req(self, **kw):
        self.id += 1; kw['id'] = self.id; self.send(kw)
        return self.line()
    def raw(self, text):
        self.send(text); return self.line()
    def close(self):
        self.p.stdin.close(); assert self.p.wait(timeout=10) == 0, 'the bridge must exit when stdin closes'

try:
    # Four synthetic workers on the "remote" computer's own state and tmux server.
    subprocess.run([B, '--state', str(state2), '--tmux', T, 'demo'], text=True, capture_output=True, timeout=30, env=env, check=True)

    def silent_bridge_is_refused():
        marker.write_text('silent')
        e = cli('machine', 'add', 'lab', 'lab-host', ok=False)
        assert 'lab-host' in e['error'] and machines.exists() is False, e
        marker.unlink()
    case('machine add refuses a computer whose bridge never says hello', silent_bridge_is_refused)

    def add_and_list():
        out = cli('machine', 'add', 'lab', 'lab-host')
        assert out['version'] == '0.7.0' and out['host'], out
        assert machines.read_text() == 'lab\tlab-host\tenabled\n', saved()
        assert cli('machine', 'list')['machines'] == [{'label': 'lab', 'target': 'lab-host', 'enabled': True}]
        assert 'already exists' in cli('machine', 'add', 'lab', 'other', ok=False)['error']
        assert 'letters, digits' in cli('machine', 'add', 'lab 2', 'other', ok=False)['error']
        assert saved() == 'lab\tlab-host\tenabled\n', 'a refused add must not touch the file'
    case('machine add checks the hello first, then saves; duplicates and bad labels are refused', add_and_list)

    def status_reports():
        cli('machine', 'add', 'lab2', 'other-host')
        rows = status()
        assert [r['label'] for r in rows] == ['lab', 'lab2'], rows
        r = rows[0]
        assert (r['reachable'], r['state'], r['server'], r['agents']) == (True, 'up', True, 4), r
        assert r['host'] and r['version'] == '0.7.0', r
        cli('machine', 'remove', 'lab2')
    case('machine status reports host, version, server and agent count over the bridge', status_reports)

    def status_without_ssh():
        r = status('lab', e=dict(env, PCTL_SSH='%s/nonexistent-ssh' % root))
        assert r['reachable'] is False and r['state'] == 'down' and 'cannot run' in r['error'], r
    case('machine status reports a machine it cannot reach', status_without_ssh)

    def enable_disable():
        cli('machine', 'disable', 'lab')
        assert saved() == 'lab\tlab-host\tdisabled\n', saved()
        assert status() == [], 'a disabled machine is not asked'
        e = cli('--machine', 'lab', 'machine', 'list', ok=False)
        assert 'disabled' in e['error'], e
        cli('machine', 'enable', 'lab')
        assert saved() == 'lab\tlab-host\tenabled\n', saved()
    case('disable keeps status away and --machine out; enable brings them back', enable_disable)

    def rename_remove():
        cli('machine', 'rename', 'lab', 'lab2')
        assert saved() == 'lab2\tlab-host\tenabled\n', saved()
        assert 'already exists' in cli('machine', 'rename', 'nope', 'lab2', ok=False)['error']
        assert 'not found' in cli('machine', 'remove', 'nope', ok=False)['error']
        assert cli('machine', 'remove', 'lab2') == {'removed': 'lab2'}
        assert cli('machine', 'list')['machines'] == [] and saved() == ''
    case('machine rename, enable/disable and remove keep the file in step', rename_remove)

    def machine_option_round_trip():
        cli('machine', 'add', 'lab', 'lab-host')
        arglog.write_text('')
        out = subprocess.run([B, '--machine', 'lab', 'session', 'list'], text=True, capture_output=True, timeout=30, env=env)
        assert out.returncode == 0, out.stderr
        assert len(json.loads(out.stdout)) == 4, out.stdout
        argv = arglog.read_text().split('|')
        assert '-t' not in argv and '-o' in argv and 'BatchMode=yes' in argv, argv
        e = subprocess.run([B, '--machine', 'nosuch', 'session', 'list'], text=True, capture_output=True, timeout=30, env=env)
        assert e.returncode == 2 and 'not found' in e.stderr, e.stderr
    case('--machine runs a command on the saved computer through ssh, without a tty', machine_option_round_trip)

    def machine_option_with_a_tty():
        arglog.write_text('')
        master, slave = pty.openpty()
        p = subprocess.Popen([B, '--machine', 'lab', 'session', 'list'], stdin=slave, stdout=slave, stderr=slave, env=env)
        os.close(slave); text = b''
        while select.select([master], [], [], 30)[0]:
            try:
                chunk = os.read(master, 4096)
            except OSError:
                break
            if not chunk:
                break
            text += chunk
        os.close(master)
        assert p.wait(timeout=30) == 0
        assert len(json.loads(text.decode().replace('\r\n', '\n'))) == 4, text
        assert arglog.read_text().split('|')[0] == '-t', arglog.read_text()
    case('--machine asks ssh for a terminal when both ends are one', machine_option_with_a_tty)

    def status_leaves_nothing_behind():
        pids.write_text('')
        status()
        stale = [p for p in pids.read_text().split() if subprocess.run(['/bin/ps', '-p', p], capture_output=True).returncode == 0]
        assert stale == [], f'ssh children left running: {stale}'
        assert len(pids.read_text().split()) >= 1, 'the fake ssh should have run'
    case('machine status kills the ssh child it no longer needs', status_leaves_nothing_behind)

    def a_bridge_that_does_not_answer():
        marker.write_text('slow')
        r = status('lab')
        assert r['reachable'] and r['error'].startswith('list:'), r
        marker.unlink()
    case('a request without an answer fails within its deadline', a_bridge_that_does_not_answer)

    def bridge_lists_the_remote():
        b = Bridge()
        answer = b.req(op='list')
        assert answer['ok'] and answer['connected'] is True, answer
        assert len(answer['sessions']) == 4 and len(answer['projects']) == 2, answer
        b.close()
    case('bridge list answers with the remote sessions, projects and connected', bridge_lists_the_remote)

    def bridge_without_a_server():
        b = Bridge(state3)
        answer = b.req(op='list')
        assert answer['ok'] and answer['connected'] is False and answer['sessions'] == [], answer
        b.close()
        assert not (state3/'tmux.sock').exists(), 'the bridge must not start a server'
    case('bridge list with no server says connected: false and starts nothing', bridge_without_a_server)

    def bridge_refuses_an_agent():
        p = subprocess.run([B, '--state', str(state2), '--tmux', T, 'bridge'], input='', text=True, capture_output=True, timeout=30, env=dict(env, PCTL_AGENT_ID='some-agent'))
        assert p.returncode == 2 and 'agent_caller' in p.stderr and p.stdout == '', (p.returncode, p.stdout, p.stderr)
    case('an agent of this computer is refused before the hello', bridge_refuses_an_agent)

    def bridge_screen_keys_paste():
        b = Bridge()
        s = b.req(op='list')['sessions'][0]
        head = b.req(op='screen', agent=s['agent_id'], run=s['run_id'], width=100, height=30, scroll=0)
        assert head['ok'] and 'PCTL SYNTHETIC' in head['rows'], head
        assert head['size'] == [100, 30] and head['history'] >= 0, head
        assert head['cursor'] is None or len(head['cursor']) == 2, head
        small = b.req(op='screen', agent=s['agent_id'], run=s['run_id'], width=60, height=20, scroll=0)
        assert small['size'] == [60, 20], small
        assert b.req(op='keys', agent=s['agent_id'], run=s['run_id'], keys=[{'literal': True, 'key': 'echo from-the-bridge'}, {'literal': False, 'key': 'Enter'}])['ok']
        assert b.req(op='paste', agent=s['agent_id'], run=s['run_id'], text='pasted-한글')['ok']
        assert b.req(op='keys', agent=s['agent_id'], run=s['run_id'], keys=[{'literal': False, 'key': 'Enter'}])['ok']
        screen = ''
        for _ in range(60):
            screen = tm('capture-pane', '-p', '-t', s['pane'])
            if 'pasted-한글' in screen and 'from-the-bridge' in screen:
                break
            time.sleep(0.05)
        assert 'from-the-bridge' in screen, screen
        assert 'pasted-한글' in screen, screen
        b.close()
    case('bridge screen resizes and returns text; keys and paste arrive', bridge_screen_keys_paste)

    def bridge_refuses_a_changed_run():
        b = Bridge()
        s = b.req(op='list')['sessions'][0]
        a = b.req(op='screen', agent=s['agent_id'], run='bogus-run', width=80, height=24, scroll=0)
        assert a['ok'] is False and a['error'].startswith('changed:'), a
        a = b.req(op='keys', agent=s['agent_id'], run='bogus-run', keys=[{'literal': True, 'key': 'x'}])
        assert a['ok'] is False and a['error'].startswith('changed:'), a
        assert b.req(op='list')['ok'], 'the bridge keeps running'
        b.close()
    case('a pane whose run changed is refused with changed: and nothing is typed', bridge_refuses_a_changed_run)

    def bridge_survives_bad_input():
        b = Bridge()
        a = b.req(op='nope')
        assert a['ok'] is False and 'unknown op nope' in a['error'], a
        a = b.req(op='screen', agent='x')
        assert a['ok'] is False and 'missing' in a['error'], a
        a = b.raw('this is not json')
        assert a['ok'] is False and a['id'] is None, a
        a = b.raw('{"op":"list","id":"not a number"}')
        assert a['ok'] is False and a['id'] is None, a
        a = b.raw('x' * (1024 * 1024 + 32))
        assert a['ok'] is False and '1 MiB' in a['error'], a
        assert b.req(op='list')['ok'], 'the bridge keeps running after bad input'
        b.close()
    case('unknown ops, missing fields, bad json and over-long lines are refused, and the bridge lives on', bridge_survives_bad_input)

    print(f'ALL {len(results)} MACHINE TESTS PASSED', flush=True)
finally:
    subprocess.run([T, '-S', str(state2/'tmux.sock'), '-f', '/dev/null', 'kill-server'], capture_output=True)
    report = pathlib.Path(args.report); report.parent.mkdir(parents=True, exist_ok=True)
    tmux_version = subprocess.run([T, '-V'], text=True, capture_output=True).stdout.strip()
    report.write_text(json.dumps({'suite': 'saved machines, bridge and client (fake ssh)', 'tmux': tmux_version, 'binary': B, 'tests': results, 'passed': len(results), 'note': 'No real ssh and no real computer: the fake ssh runs the remote command on this machine with a second state. The installed pctl on other computers has no bridge yet, so a real bridge round trip waits for panel-main to install this build.'}, indent=2) + '\n')
    shutil.rmtree(root)
