#!/usr/bin/env python3
"""`pctl tell`: agent-to-agent messages. Sessions on this computer (reached, queued, typed_unconfirmed,
blocked -> kept and typed in once idle (queued_blocked), a restarted run drops it with the reason, stopped -> exit 5, self-send), the sender line (job, ssh, a Codex
thread, a pctl session), `codex:<prefix>`, a session on another computer through a fake ssh with the
sender kept, and an A2A agent through a fake curl first in PATH (accepted after the first stream frame
only, --wait reply, anti-loop refusal and the retry key, missing token, connection failure, a context
key). The A2A token must be in no argument list, no log and no output. Fake harnesses only."""
import argparse, json, os, pathlib, re, shutil, stat, subprocess, tempfile, time
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-tell-', dir=os.path.realpath('/tmp')))
state, rstate, home, bindir, fake = root / 'local', root / 'remote', root / 'home', root / 'bin', root / 'fake'
for d in (home, bindir, fake): d.mkdir()
cfg, rcfg = root / 'cfg', root / 'rcfg'
SECRET = 'TOKEN-SECRET-7f3a91'
(home / 'api.env').write_text(f'OTHER=1\nFRONT_TOKEN=stale\nFRONT_TOKEN={SECRET}\n')
(cfg / 'pctl').mkdir(parents=True); (rcfg / 'pctl').mkdir(parents=True)
(cfg / 'pctl' / 'config.toml').write_text(f'''[tell]
host = "hosta"
door_services = "door.svc"
job_prefix = "jobs."

[tell.a2a.front]
url = "http://127.0.0.1:9/a2a"
token_file = "{home}/api.env"
token_key = "FRONT_TOKEN"

[tell.a2a.nokey]
url = "http://127.0.0.1:9/a2a"
token_file = "{home}/api.env"
token_key = "MISSING_KEY"
''')
(rcfg / 'pctl' / 'config.toml').write_text('[tell]\nhost = "hostb"\n')
(cfg / 'pctl' / 'machines').write_text('rm\tfake-target\tenabled\n')
results = []
# A fake agent. WORK shows a working footer for 3 s, BLOCK a dialog that stays until answered with 1; anything else is echoed.
(bindir / 'agent').write_text(r'''#!/bin/bash
echo "ready in $(basename "$PWD")"
echo "> "
while IFS= read -r line; do
  case "$line" in
    *WORK*) echo "✻ Thinking… (3s · esc to interrupt)"; sleep 3; printf '\033[2J\033[Hdone: %s\n> \n' "$line" ;;
    *BLOCK*) echo "Do you want to proceed?"; echo "❯ 1. Yes" ;;
    1) printf '\033[2J\033[Hanswered\n> \n' ;;
    *) echo "got: $line"; echo "> " ;;
  esac
done
''')
for n in ('claude', 'codex'): shutil.copy(bindir / 'agent', bindir / n)
for n in ('agent', 'claude', 'codex'): (bindir / n).chmod(0o755)
# A fake ssh: ignores its options and the target, runs the last argument as another computer would (its
# own state and config, the login variable sshd sets). A marker file makes it fail like an unreachable host.
(bindir / 'ssh').write_text(f'''#!/bin/sh
echo "$@" >> {fake}/ssh.log
[ -f {fake}/ssh-down ] && exit 255
for a in "$@"; do line="$a"; done
SSH_CONNECTION="10.0.0.1 50000 10.0.0.2 22" PCTL_STATE={rstate} XDG_CONFIG_HOME={rcfg} exec /bin/sh -c "$line"
''')
# A fake curl: logs its arguments, the headers it was given on stdin and the body; answers from a queue.
(bindir / 'curl').write_text(f'''#!/usr/bin/env python3
import json, os, sys, time
d = "{fake}"
args = sys.argv[1:]
open(d + "/argv.log", "a").write(json.dumps(args) + "\\n")
open(d + "/stdin.log", "a").write(json.dumps(sys.stdin.read()) + "\\n")
open(d + "/bodies.log", "a").write(args[args.index("--data-binary") + 1] + "\\n")
open(d + "/pid", "w").write(str(os.getpid()))
q = open(d + "/replies").read().splitlines() if os.path.exists(d + "/replies") else []
reply, rest = (q[0], q[1:]) if q else ("", [])
open(d + "/replies", "w").write("\\n".join(rest) + ("\\n" if rest else ""))
if reply == "EXIT7":
    sys.stderr.write("curl: (7) Failed to connect to 127.0.0.1 port 9\\n"); sys.exit(7)
if reply.startswith("STREAM "):
    print(": comment"); print("data: " + reply[7:], flush=True); time.sleep(30); sys.exit(0)
print(reply)
''')
for n in ('ssh', 'curl'): (bindir / n).chmod(0o755)
BASE = {k: v for k, v in os.environ.items() if not k.startswith(('PCTL_', 'TMUX', 'XDG_CONFIG', 'XPC_', 'CODEX_', 'SSH_'))}
BASE.update(HOME=str(home), XDG_CONFIG_HOME=str(cfg), PATH=f'{bindir}:{os.environ["PATH"]}', TERM='xterm-256color', SHELL='/bin/bash', PS1='$ ',
            PCTL_STANDALONE='1', PCTL_SSH=str(bindir / 'ssh'), PCTL_REMOTE_BIN=B, PCTL_TMUX=T, FAKE_DIR=str(fake))
def run(*args, st=None, env=None, cwd=None):
    """pctl with the given extra environment; (exit code, stdout, stderr)."""
    q = subprocess.run([B, '--state', str(st or state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=120, env=dict(BASE, **(env or {})), cwd=cwd)
    return q.returncode, q.stdout, q.stderr
def cli(*args, st=None):
    rc, out, err = run(*args, st=st); assert rc == 0, (args, rc, err, out)
    return json.loads(out) if out.strip().startswith(('{', '[')) else out
def tell(*args, env=None, st=None, cwd=None):
    rc, out, err = run('tell', *args, st=st, env=env, cwd=cwd)
    return rc, (json.loads(out) if out.strip().startswith('{') else out), err
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def add(name, prog='agent', st=None, project='p', *extra):
    d = root / 'proj' / name; d.mkdir(parents=True, exist_ok=True)
    return cli('session', 'add', '--name', name, '--cwd', d, '--project', project, *extra, '--', bindir / prog, st=st)
def screen(name, st=None): return run('session', 'read', name, '--lines', '30', st=st)[1]
def status(name, st=None): return cli('session', 'get', name, st=st)['status']
def log(name): return (fake / name).read_text() if (fake / name).exists() else ''
def bodies(): return [json.loads(l) for l in log('bodies.log').splitlines()]
def reset_fake():
    for n in ('argv.log', 'stdin.log', 'bodies.log', 'replies'): (fake / n).unlink(missing_ok=True)
def replies(*lines): (fake / 'replies').write_text('\n'.join(lines) + '\n')
DOOR = {'XPC_SERVICE_NAME': 'door.svc'}
SUB = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"task": {"id": "t", "contextId": "c", "status": {"state": "TASK_STATE_SUBMITTED"}}}})
REJ = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"statusUpdate": {"taskId": "t", "contextId": "c", "status": {"state": "TASK_STATE_REJECTED", "message": {"parts": [{"text": "Anti-loop protection: context c exceeded 5 turns."}]}}}}})
DONE = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"task": {"id": "t", "contextId": "c", "status": {"state": "TASK_STATE_COMPLETED", "message": {"parts": [{"text": "pong"}]}}}}})
DAY = time.strftime('%Y%m%d')
try:
    for n in ('idle1', 'busy1', 'unconf1', 'blocked1', 'blocked2', 'blocked3', 'blocked4', 'gone1', 'sender1'): add(n)
    add('cx', 'claude', None, 'p', '--conversation', 'convo-abcdef12-rest')
    add('rwork', st=rstate); add('rgone', st=rstate); add('rblock', st=rstate)
    for n in ('idle1', 'busy1', 'unconf1', 'blocked1', 'blocked2', 'blocked3', 'blocked4', 'gone1', 'sender1', 'cx'): assert until(lambda: 'ready in' in screen(n)), n
    assert until(lambda: all('ready in' in screen(n, rstate) for n in ('rwork', 'rgone', 'rblock')))

    def reached():
        rc, v, err = tell('idle1', 'WORK now', env=DOOR)
        assert rc == 0 and v == {'to': 'idle1', 'result': 'reached'}, (rc, v, err)
        assert until(lambda: 'done: [from hosta hosta ' in screen('idle1')), screen('idle1')
        rc, out, _ = run('tell', 'idle1', 'WORK again', '--plain', env=DOOR)
        assert rc == 0 and out.strip().startswith('reached: idle1'), out
        assert until(lambda: status('idle1') == 'idle', 8)
    case('a session that starts working reads as reached (JSON and --plain), exit 0', reached)
    def queued():
        cli('session', 'prompt', 'busy1', 'WORK first')
        assert until(lambda: status('busy1') == 'working', 4)
        rc, v, err = tell('busy1', 'hello later', env=DOOR)
        assert rc == 0 and v['result'] == 'queued', (rc, v, err)
        assert until(lambda: 'got: [from hosta hosta ' in screen('busy1') and 'hello later' in screen('busy1'), 12), screen('busy1')
    case('a session that was working gets the text queued: result queued, exit 0', queued)
    def unconfirmed():
        t0 = time.monotonic(); rc, v, err = tell('unconf1', 'plain hello', env=DOOR)
        assert rc == 0 and v['result'] == 'typed_unconfirmed' and time.monotonic() - t0 >= 7, (rc, v, err)
        assert 'got: [from hosta hosta ' in screen('unconf1') and 'plain hello' in screen('unconf1')
    case('typed in but never seen to start: typed_unconfirmed after 8 s, exit 0 (sending again would duplicate)', unconfirmed)
    def block(n, st=None):
        cli('session', 'prompt', n, 'BLOCK it', st=st); assert until(lambda: status(n, st) == 'blocked', 5), n
    def answer(n): cli('session', 'keys', n, '1', 'Enter')
    def blocked():
        block('blocked1'); before = screen('blocked1')
        rc, v, err = tell('blocked1', 'first kept', env=DOOR)
        assert rc == 0 and v['result'] == 'queued_blocked' and v['queue'] == 1 and 'human' in v['note'], (rc, v, err)
        rc, out, err = run('tell', 'blocked1', 'second kept', '--plain', env=DOOR)
        assert rc == 0 and out.startswith('queued blocked: blocked1') and '2 waiting' in out, (rc, out, err)
        time.sleep(1); assert screen('blocked1') == before and 'kept' not in screen('blocked1'), 'nothing typed into the dialog'
        assert cli('session', 'get', 'blocked1')['tell_queue'] == 2 and cli('session', 'list')[3]['tell_queue'] in (0, 2)
        q = state / 'tell' / 'queue' / 'blocked1'
        assert oct(q.stat().st_mode & 0o777) == '0o700' and all(oct(f.stat().st_mode & 0o777) == '0o600' for f in q.glob('*.json')), 'private files'
        answer('blocked1'); assert until(lambda: 'answered' in screen('blocked1'), 5)
        v = cli('session', 'get', 'blocked1')  # a status read of an idle session delivers
        assert until(lambda: 'second kept' in screen('blocked1'), 8), screen('blocked1')
        s = screen('blocked1'); assert s.index('first kept') < s.index('second kept') and s.count('got: [from') == 2, s
        assert 'got: [from hosta hosta ' in s and cli('session', 'get', 'blocked1')['tell_queue'] == 0
    case('a session waiting for a human decision keeps the message (queued_blocked, exit 0), types nothing into the dialog, and gets it once idle, in order, once each', blocked)
    def queue_order():
        block('blocked2')
        assert tell('blocked2', 'one', env=DOOR)[1]['result'] == 'queued_blocked' and tell('blocked2', 'two', env=DOOR)[1]['result'] == 'queued_blocked'
        answer('blocked2'); assert until(lambda: 'answered' in screen('blocked2'), 5)
        rc, v, err = tell('blocked2', 'three', env=DOOR)  # no status read in between: the queue goes first
        assert rc == 0 and v['result'] in ('reached', 'queued', 'typed_unconfirmed'), (rc, v, err)
        assert until(lambda: 'three' in screen('blocked2'), 12), screen('blocked2')
        s = screen('blocked2'); assert s.index('one') < s.index('two') < s.index('three'), s
    case('a later tell to a session that has kept messages sends them first, so the order holds', queue_order)
    def queue_listed():
        block('blocked3'); tell('blocked3', 'via list', env=DOOR)
        answer('blocked3'); assert until(lambda: 'answered' in screen('blocked3'), 5)
        assert until(lambda: (cli('session', 'list'), 'via list' in screen('blocked3'))[1], 8), screen('blocked3')  # what the app polls
    case('session list (what the app polls) delivers kept messages to a session that is idle', queue_listed)
    def queue_run_changed():
        block('blocked4'); tell('blocked4', 'for the old run', env=DOOR)
        answer('blocked4'); assert until(lambda: 'answered' in screen('blocked4'), 5)  # no status read: nothing is delivered yet
        cli('session', 'restart', 'blocked4', '--force'); assert until(lambda: 'ready in' in screen('blocked4'), 8)
        v = cli('session', 'get', 'blocked4')
        time.sleep(1)
        v = cli('session', 'get', 'blocked4')
        assert v['tell_queue'] == 0 and v['tell_dropped'] == 1 and 'old run' not in screen('blocked4'), (v, screen('blocked4'))
        why = [json.loads(f.read_text()) for f in (state / 'tell' / 'dropped' / 'blocked4').glob('*.json')]
        assert len(why) == 1 and 'another run' in why[0]['reason'] and why[0]['text'].endswith('for the old run'), why
    case('a message kept for a run that was restarted is not typed into the new one: it is moved aside with the reason and counted in session get', queue_run_changed)
    def gone():
        pane = cli('session', 'get', 'gone1')['pane']
        subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-pane', '-t', pane], check=True)
        assert until(lambda: status('gone1') in ('lost', 'exited', 'stopped'), 8), status('gone1')
        rc, v, err = tell('gone1', 'anyone?', env=DOOR); assert rc == 5 and v['result'] == 'not_sent' and 'is ' in v['error'], (rc, v)
        rc, v, err = tell('nobody-by-that-name', 'anyone?', env=DOOR); assert rc == 5 and v['result'] == 'not_sent', (rc, v, err)
    case('a stopped, lost or exited session, and an unknown name, exit 5', gone)
    def to_myself():
        for to in ('hosta', 'hosta:hosta'):
            rc, v, err = tell(to, 'note to self', env=DOOR); assert rc == 0 and v['result'] == 'self', (to, rc, v, err)
    case('sending to yourself prints and sends nothing, exit 0', to_myself)
    def sender_lines():
        proj = home / 'proj'; proj.mkdir()
        def line_for(env, text):
            # WORK makes the session start working at once, so each send returns without the 8 s wait.
            rc, v, err = tell('sender1', 'WORK ' + text, env=env, cwd=proj); assert rc == 0, (rc, v, err)
            assert until(lambda: text in screen('sender1').replace('\n', ' '), 15), screen('sender1')
            return re.findall(r'(\[from [^\]]*\]) WORK ' + text, screen('sender1').replace('\n', ' '))[-1]
        got = line_for(DOOR, 'door-text'); assert got == '[from hosta hosta ~/proj]', got
        assert line_for({'XPC_SERVICE_NAME': 'jobs.nightly'}, 'job-text') == '[from job:nightly hosta ~/proj]'
        assert line_for({'SSH_CONNECTION': '1 2 3 4'}, 'ssh-text') == '[from ssh hosta ~/proj]'
        assert line_for({}, 'none-text') == '[from ? hosta ~/proj]'
        assert line_for({'CODEX_THREAD_ID': 'zzzzzzzz-unknown'}, 'codex-text') == '[from codex:zzzzzzzz hosta ~/proj]'
        assert line_for({'CODEX_THREAD_ID': 'convo-abcdef12-rest'}, 'known-text') == '[from cx hosta ~/proj]'
        s = cli('session', 'get', 'idle1')
        assert line_for({'PCTL_AGENT_ID': s['agent_id'], 'PCTL_RUN_ID': s['run_id']}, 'pane-text') == '[from idle1 hosta ~/proj]'
    case('the sender line names a pctl session, a Codex thread, the front door, a job, ssh or ?, with a short path', sender_lines)
    def codex_prefix():
        rc, v, err = tell('codex:convo-ab', 'reply address', env=DOOR); assert rc == 0 and v['to'] == 'codex:convo-ab' and v['result'] != 'not_sent', (rc, v, err)
        assert until(lambda: 'reply address' in screen('cx'), 12), screen('cx')
        rc, v, err = tell('codex:nomatch1', 'x', env=DOOR); assert rc == 5 and v['result'] == 'not_sent', (rc, v)
        rc, v, err = tell('codex:con', 'x', env=DOOR); assert rc == 5, 'too short a prefix matches nothing'
    case('codex:<prefix> reaches the session whose conversation starts with it', codex_prefix)

    def remote_session():
        job = {'XPC_SERVICE_NAME': 'jobs.nightly'}
        rc, v, err = tell('rm:rwork', 'WORK remote', env=job, cwd=home)
        assert rc == 0 and v == {'to': 'rm:rwork', 'result': 'reached'}, (rc, v, err)
        assert until(lambda: 'done: [from job:nightly hosta ~] WORK remote' in screen('rwork', rstate).replace('\n', ' ')), screen('rwork', rstate)
        assert 'fake-target' in log('ssh.log') and '--from-line' in log('ssh.log') and 'WORK remote' in log('ssh.log')
        rc, v, err = tell('otherhost:rwork', 'by ssh target', env=job); assert rc == 0, (rc, v, err)
        assert 'otherhost' in log('ssh.log'), 'an unsaved name is used as the ssh target'
        assert until(lambda: 'by ssh target' in screen('rwork', rstate), 12)
        rc, out, err = run('tell', 'rm:rwork', 'plain remote', '--plain', env=job); assert rc == 0 and out.startswith('reached: rm:rwork') or out.startswith(('queued: rm:rwork', 'typed unconfirmed: rm:rwork')), (rc, out, err)
    case('a session on another computer by saved label or ssh target: the sender stays the caller, not ssh', remote_session)
    def remote_refusals():
        cli('session', 'prompt', 'rblock', 'BLOCK it', st=rstate); assert until(lambda: status('rblock', rstate) == 'blocked', 5)
        rc, v, err = tell('rm:rblock', 'nope', env=DOOR); assert rc == 0 and v['result'] == 'queued_blocked' and v['to'] == 'rm:rblock' and v['queue'] == 1, (rc, v, err)
        assert (rstate / 'tell' / 'queue' / 'rblock').is_dir() and 'nope' not in screen('rblock', rstate), 'kept on the other computer, not typed'
        pane = cli('session', 'get', 'rgone', st=rstate)['pane']
        subprocess.run([T, '-S', str(rstate / 'tmux.sock'), 'kill-pane', '-t', pane], check=True)
        assert until(lambda: status('rgone', rstate) in ('lost', 'exited', 'stopped'), 8)
        rc, v, err = tell('rm:rgone', 'nope', env=DOOR); assert rc == 5, (rc, v)
        (fake / 'ssh-down').write_text('')
        rc, v, err = tell('rm:rwork', 'unreachable', env=DOOR); assert rc == 4 and v['result'] == 'no_connection', (rc, v, err)
        (fake / 'ssh-down').unlink()
        rc, v, err = run('tell', 'idle1', 'x', '--from-line', '[from a b c]', env=DOOR); assert rc == 2 and 'only accepted over ssh' in err, (rc, err)
        rc, v, err = run('tell', 'idle1', 'x', '--from-line', '[from a b c]', env=dict(DOOR, SSH_CONNECTION='1 2 3 4')); assert rc == 0, 'with an ssh login it is accepted'
        rc, v, err = run('tell', 'idle1', 'x', '--from-line', 'not a line', env={'SSH_CONNECTION': '1 2 3 4'}); assert rc == 2, (rc, err)
    case('the other computer\'s refusals and failures keep their exit codes; --from-line only over ssh', remote_refusals)

    def accepted():
        reset_fake(); replies('STREAM ' + SUB)
        t0 = time.monotonic(); rc, v, err = tell('front', 'hello front', env=DOOR); dt = time.monotonic() - t0
        ctx = f'msg-hosta-hosta-{DAY}'
        assert rc == 0 and v == {'to': 'front', 'result': 'accepted', 'context': ctx}, (rc, v, err)
        assert dt < 10, f'the first frame is enough, the stream is dropped ({dt:.1f}s)'
        pid = int((fake / 'pid').read_text()); time.sleep(0.3)
        try: os.kill(pid, 0); raise AssertionError('curl was left running')
        except ProcessLookupError: pass
        b = bodies()[0]; m = b['params']['message']
        assert b['method'] == 'SendStreamingMessage' and m['contextId'] == ctx and m['role'] == 'ROLE_USER', b
        assert m['parts'][0]['text'] == '[from hosta hosta ~ · reply with tell] hello front' or m['parts'][0]['text'].startswith('[from hosta hosta '), m
        assert m['parts'][0]['text'].endswith(' · reply with tell] hello front'), m['parts'][0]['text']
        argv = json.loads(log('argv.log').splitlines()[0])
        assert 'A2A-Version: 1.0' in argv and '@-' in argv and 'Accept: text/event-stream' in argv and argv[-1] == 'http://127.0.0.1:9/a2a', argv
        assert json.loads(log('stdin.log').splitlines()[0]) == f'Authorization: Bearer {SECRET}\n', 'the token arrives on stdin'
    case('an A2A agent: first stream frame only (accepted), the hint in the sender line, the token on curl\'s stdin', accepted)
    def waited():
        reset_fake(); replies(DONE)
        rc, v, err = tell('front', 'ping', '--wait', env={'XPC_SERVICE_NAME': 'jobs.x'})
        assert rc == 0 and v['result'] == 'replied' and v['reply'] == 'pong' and v['context'] == f'msg-hosta-job-x-{DAY}', (rc, v, err)
        b = bodies()[0]; assert b['method'] == 'SendMessage' and 'reply with tell' not in b['params']['message']['parts'][0]['text'], b
        replies(DONE); rc, out, err = run('tell', 'front', 'ping', '--wait', '--plain', env=DOOR)
        assert rc == 0 and out.splitlines()[-1] == 'pong' and out.startswith('replied: front'), (rc, out, err)
    case('--wait sends SendMessage and prints the reply text', waited)
    def anti_loop():
        reset_fake(); replies('STREAM ' + REJ, 'STREAM ' + SUB)
        rc, v, err = tell('front', 'sixth', env=DOOR)
        base = f'msg-hosta-hosta-{DAY}'
        assert rc == 0 and v['result'] == 'accepted' and v['context'] == base + '-r2', (rc, v, err)
        assert [b['params']['message']['contextId'] for b in bodies()] == [base, base + '-r2']
        f = state / 'tell' / f'ctx-front-{base}'
        assert f.read_text().strip() == base + '-r2' and stat.S_IMODE(f.stat().st_mode) == 0o600, 'remembered in a private file'
        reset_fake(); replies('STREAM ' + SUB); rc, v, err = tell('front', 'next', env=DOOR)
        assert [b['params']['message']['contextId'] for b in bodies()] == [base + '-r2'], 'the next message uses the new key at once'
        reset_fake(); replies('STREAM ' + REJ, 'STREAM ' + REJ)
        rc, v, err = tell('front', 'again', env=DOOR)
        assert rc == 1 and v['result'] == 'refused' and 'refused twice' in v['error'], (rc, v)
        assert [b['params']['message']['contextId'] for b in bodies()] == [base + '-r2', base + '-r3'], 'one retry per send, with the next key'
        reset_fake(); replies('STREAM ' + SUB); tell('front', 'after both', env=DOOR)
        assert [b['params']['message']['contextId'] for b in bodies()] == [base + '-r3'], 'the newest key is the one remembered'
    case('an anti-loop refusal retries once with -r2, -r3 …; the key is remembered in a 0600 file', anti_loop)
    def context_key():
        reset_fake(); replies('STREAM ' + SUB)
        rc, v, err = tell('front', 'card talk', '--context', 'card 42/x', env=DOOR)
        assert rc == 0 and v['context'] == 'card-42-x' and bodies()[0]['params']['message']['contextId'] == 'card-42-x', (rc, v)
    case('--context names the conversation (sanitized)', context_key)
    def failures():
        reset_fake()
        rc, v, err = tell('nokey', 'x', env=DOOR); assert rc == 3 and v['result'] == 'no_token' and not log('argv.log'), (rc, v, 'curl must not run')
        assert SECRET not in json.dumps(v) + err
        replies('EXIT7'); rc, v, err = tell('front', 'x', env=DOOR); assert rc == 4 and v['result'] == 'no_connection' and 'Failed to connect' in v['error'], (rc, v, err)
        replies('STREAM {"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"no such agent"}}'); rc, v, err = tell('front', 'x', env=DOOR)
        assert rc == 1 and v['result'] == 'refused' and 'no such agent' in v['error'], (rc, v)
        replies(DONE.replace('COMPLETED', 'FAILED')); rc, v, err = tell('front', 'x', '--wait', env=DOOR); assert rc == 1, (rc, v)
    case('no token is exit 3 (curl never runs), a failed connection exit 4, a refusal exit 1 with the reason', failures)
    def secret_nowhere():
        mine = state / 'tell.log'; assert mine.exists() and stat.S_IMODE(mine.stat().st_mode) == 0o600
        text = mine.read_text()
        for needle in (SECRET, 'hello front', 'plain hello', 'note to self'): assert needle not in text, needle
        rows = [l.split('\t') for l in text.splitlines()]
        assert all(len(r) == 5 for r in rows), rows
        assert any(r[1:] == ['hosta', 'front', 'accepted', str(len('hello front'))] for r in rows), rows
        assert any(r[2] == 'blocked1' and r[3] == 'queued_blocked' for r in rows) and any(r[3] == 'self' for r in rows), rows
        assert SECRET not in log('argv.log'), 'the token was never an argument'
        assert SECRET in log('stdin.log'), 'it did reach curl, on stdin'
        for f in (state, rstate):
            for path in f.rglob('*'):
                if path.is_file() and path.name not in ('tmux.sock',) and path.stat().st_size < 5_000_000:
                    assert SECRET not in path.read_text(errors='ignore'), path
    case('the token is in no argument list, no log and no state file; the log has no text', secret_nowhere)
    json.dump(results, open(a.report, 'w'), indent=2)
finally:
    for st in (state, rstate): subprocess.run([T, '-S', str(st / 'tmux.sock'), 'kill-server'], capture_output=True)
    shutil.rmtree(root, ignore_errors=True)
