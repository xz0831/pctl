#!/usr/bin/env python3
"""--host forwards argv through ssh unchanged: a fake ssh runs the remote line locally."""
import argparse, json, os, pathlib, subprocess, tempfile
p = argparse.ArgumentParser(); p.add_argument('--bin', required=True); a = p.parse_args()
B = str(pathlib.Path(a.bin).resolve())
d = pathlib.Path(tempfile.mkdtemp(prefix='pctl-remote-', dir=os.path.realpath('/tmp')))
(d / 'ssh').write_text('#!/bin/sh\n# ssh -o BatchMode=yes -- HOST LINE\n[ "$4" = fakehost ] || exit 99\nexec /bin/sh -c "$5"\n'); (d / 'ssh').chmod(0o700)
(d / 'echo-argv').write_text('#!/usr/bin/python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n'); (d / 'echo-argv').chmod(0o700)
env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", PCTL_REMOTE_BIN=str(d / 'echo-argv'))
tricky = ['--state', "/tmp/a b/'q'", 'agent', 'add', '--name', '한글 $HOME `x` "y"', '--', 'sh', '-c', 'echo; exit 3']
out = subprocess.run([B, '--host', 'fakehost', *tricky], env=env, text=True, capture_output=True, timeout=10)
assert out.returncode == 0, out
assert json.loads(out.stdout) == tricky, out.stdout
fail = subprocess.run([B, '--host', 'fakehost', 'x'], env=dict(env, PCTL_REMOTE_BIN='/usr/bin/false'), capture_output=True, timeout=10)
assert fail.returncode == 1, fail
print('PASS --host forwards argv exactly and passes the remote exit code')
