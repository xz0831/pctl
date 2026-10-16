#!/usr/bin/env python3
"""pctl web over real tmux with fake harnesses: refuses non-loopback/non-Tailscale listen addresses
and loose token files, serves the page without data, needs the bearer token for every API call,
lists sessions, reads a screen, types into an idle session, never into a blocked one, and logs no
token, names or text. A refused add (no server) is checked too: web never starts a server."""
import argparse, json, os, pathlib, secrets, subprocess, tempfile, time, urllib.error, urllib.request
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); p.add_argument('--tmux', required=True); p.add_argument('--report', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve()); T = str(pathlib.Path(a.tmux).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix='pctl-web-', dir=os.path.realpath('/tmp'))); state = root / 's'; results = []
bindir = root / 'bin'; bindir.mkdir()
FAKE = r'''#!/bin/bash
if [ "$(basename "$PWD")" = b-block ]; then echo "Do you want to proceed?"; echo "❯ 1. Yes"; fi
if [ "$(basename "$PWD")" = m-menu ]; then echo "Update available"; echo "› 1. Update now"; echo "  2. Skip"; fi
echo "> "
while IFS= read -r line; do echo "got: $line"; echo "> "; done
'''
(bindir / 'plain').write_text(FAKE); (bindir / 'plain').chmod(0o755)
ENV = dict(os.environ, PCTL_STANDALONE='1')
OUTSIDE = {k: v for k, v in os.environ.items() if not k.startswith('PCTL_STANDALONE')}
OUTSIDE['XDG_CONFIG_HOME'] = '/nonexistent-pctl-config'  # no [server] start_env either
def cli(*args, ok=True):
    q = subprocess.run([B, '--state', str(state), '--tmux', T, *map(str, args)], text=True, capture_output=True, timeout=30, env=ENV)
    assert (q.returncode == 0) == ok, (args, q.returncode, q.stderr, q.stdout)
    return (json.loads(q.stdout) if q.stdout.strip().startswith(('{', '[')) else q.stdout) if ok else q.stderr
def until(fn, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn(): return True
        time.sleep(0.1)
    return fn()
def case(name, fn):
    t = time.monotonic(); fn(); results.append({'test': name, 'passed': True, 'seconds': round(time.monotonic() - t, 3)}); print('PASS', name, flush=True)
def cwd(n):
    d = root / 'proj' / n; d.mkdir(parents=True, exist_ok=True); return d
TOKEN = secrets.token_hex(24)
tok = root / 'token'; tok.write_text(TOKEN + '\n'); tok.chmod(0o600)
def web(st, listen, token_file):
    return subprocess.Popen([B, '--state', str(st), '--tmux', T, 'web', '--listen', listen, '--token-file', str(token_file)],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=OUTSIDE)
def call(base, path, body=None, token=TOKEN, method='POST', raw=None):
    data = raw if raw is not None else (None if method == 'GET' else json.dumps(body or {}).encode())
    req = urllib.request.Request(base + path, data=data, method=method)
    if token: req.add_header('Authorization', 'Bearer ' + token)
    try:
        with urllib.request.urlopen(req, timeout=15) as r: return r.status, dict(r.headers), r.read().decode()
    except urllib.error.HTTPError as e: return e.code, dict(e.headers), e.read().decode()
servers = []
try:
    def refusals():
        for listen in ('0.0.0.0:0', '[::]:0', '192.168.1.5:0', '8.8.8.8:0'):
            q = web(state, listen, tok); out, err = q.communicate(timeout=15)
            assert q.returncode != 0 and 'refused' in err and out == '', (listen, err)
        loose = root / 'loose'; loose.write_text(TOKEN); loose.chmod(0o644)
        q = web(state, '127.0.0.1:0', loose); _, err = q.communicate(timeout=15)
        assert q.returncode != 0 and '0600' in err and TOKEN not in err, err
        short = root / 'short'; short.write_text('abc'); short.chmod(0o600)
        q = web(state, '127.0.0.1:0', short); _, err = q.communicate(timeout=15)
        assert q.returncode != 0 and '32' in err, err
    case('refuses 0.0.0.0, ::, LAN and public addresses, loose and short token files', refusals)
    def sessions():
        cli('session', 'add', '--name', 'p-shell', '--cwd', cwd('p-shell'), '--project', 'demo', '--', bindir / 'plain')
        cli('session', 'add', '--name', 'b-block', '--cwd', cwd('b-block'), '--project', 'demo', '--', bindir / 'plain')
        cli('session', 'add', '--name', 'm-menu', '--cwd', cwd('m-menu'), '--project', 'demo', '--', bindir / 'plain')
        assert until(lambda: cli('session', 'get', 'b-block')['status'] == 'blocked')
        assert until(lambda: cli('session', 'get', 'm-menu')['status'] == 'blocked'), 'a numbered menu waits for a person'
        assert until(lambda: cli('session', 'get', 'p-shell')['status'] == 'idle')
    case('fake sessions: one idle, one blocked by a question, one by a numbered menu', sessions)
    w = web(state, '127.0.0.1:0', tok); servers.append(w)
    base = 'http://' + json.loads(w.stdout.readline())['listening']
    def page():
        code, h, body = call(base, '/', method='GET', token=None)
        assert code == 200 and '<title>pctl sessions</title>' in body, (code, body[:80])
        assert "default-src 'none'" in h['Content-Security-Policy'] and h['Cache-Control'] == 'no-store', h
        assert call(base, '/app.js', method='GET', token=None)[0] == 200 and call(base, '/app.css', method='GET', token=None)[0] == 200
        assert call(base, '/nope', method='GET', token=None)[0] == 404
        assert call(base, '/api/sessions', method='GET')[0] == 404, 'the API is POST only'
    case('page, script and style are served without data; unknown paths are 404', page)
    def auth():
        t = time.monotonic()
        assert call(base, '/api/sessions', token=None)[0] == 401
        assert call(base, '/api/sessions', token=TOKEN[:-1] + 'x')[0] == 401
        assert time.monotonic() - t >= 2, 'each refusal waits a second'
    case('API calls without the right token are refused, slowly', auth)
    def listing():
        code, _, body = call(base, '/api/sessions')
        v = json.loads(body); assert code == 200, body
        assert {s['name']: s['status'] for s in v} == {'p-shell': 'idle', 'b-block': 'blocked', 'm-menu': 'blocked'}, v
        assert all(set(s) == {'name', 'project', 'harness', 'status', 'branch'} for s in v), v
    case('lists sessions with name, project, harness, status and branch only', listing)
    def read_and_type():
        code, _, body = call(base, '/api/screen', {'name': 'p-shell', 'lines': 5})
        assert code == 200 and json.loads(body)['status'] == 'idle' and '> ' in json.loads(body)['text'], body
        code, _, body = call(base, '/api/prompt', {'name': 'p-shell', 'text': 'hello from web 한글'})
        assert code == 200 and json.loads(body)['status_before'] == 'idle', body
        assert until(lambda: 'got: hello from web 한글' in json.loads(call(base, '/api/screen', {'name': 'p-shell'})[2])['text'])
    case('reads a screen and types a line into an idle session', read_and_type)
    def blocked():
        code, _, body = call(base, '/api/prompt', {'name': 'b-block', 'text': 'yes please'})
        assert code == 409 and json.loads(body)['error'].startswith('blocked'), (code, body)
        time.sleep(0.5)
        assert 'yes please' not in cli('session', 'read', 'b-block', '--lines', '20')
        assert call(base, '/api/prompt', {'name': 'm-menu', 'text': 'hi'})[0] == 409
        assert 'got: hi' not in cli('session', 'read', 'm-menu', '--lines', '20'), 'Enter never reached the menu'
        code, _, body = call(base, '/api/screen', {'name': 'b-block'})
        assert code == 200 and 'Do you want to proceed?' in json.loads(body)['text'], 'a blocked screen can still be read'
    case('blocked sessions (question or menu) refuse input (409) and are not typed into', blocked)
    def bad_input():
        assert call(base, '/api/prompt', {'name': 'nobody', 'text': 'x'})[0] == 400
        assert call(base, '/api/prompt', {'name': 'p-shell', 'text': '   '})[0] == 400
        assert call(base, '/api/prompt', raw=b'x' * 100_000)[0] == 400, 'oversized body'
        assert call(base, '/api/sessions')[0] == 200, 'still serving'
    case('unknown names, empty text and oversized bodies are refused; the server keeps serving', bad_input)
    def no_server():
        other = root / 's2'
        w2 = web(other, '127.0.0.1:0', tok); servers.append(w2)
        b2 = 'http://' + json.loads(w2.stdout.readline())['listening']
        code, _, body = call(b2, '/api/sessions')
        assert code == 200 and json.loads(body) == [], body
        assert subprocess.run([T, '-S', str(other / 'tmux.sock'), 'ls'], capture_output=True).returncode != 0, 'no server created'
    case('with no PCTL server it lists nothing and starts none', no_server)
    def log():
        w.terminate(); err = w.stderr.read()
        lines = err.strip().splitlines()
        assert lines and all(l.split()[-1].isdigit() for l in lines), err
        for secret in (TOKEN, 'p-shell', 'b-block', 'm-menu', 'hello from web', 'yes please'):
            assert secret not in err, secret
    case('the log has method, path and status only: no token, names or text', log)
    pathlib.Path(a.report).write_text(json.dumps({'results': results, 'passed': len(results)}, indent=1))
finally:
    for s in servers: s.kill()
    subprocess.run([T, '-S', str(state / 'tmux.sock'), 'kill-server'], capture_output=True)
