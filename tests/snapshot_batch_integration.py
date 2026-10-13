#!/usr/bin/env python3
"""Isolated native snapshot batching, partial-output and fresh-control-guard regression."""
import argparse, hashlib, json, os, pathlib, subprocess, tempfile
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);a=p.parse_args()
B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-batch-',dir='/tmp'));state=root/'s';calls=root/'calls.jsonl';mode=root/'mode';shim=root/'tmux-shim';results=[]
env={k:v for k,v in os.environ.items() if not k.startswith('PCTL_')}
def cli(*args,ok=True,wrapped=False):
 q=subprocess.run([B,'--state',str(state),'--tmux',str(shim) if wrapped else T,*map(str,args)],capture_output=True,text=True,env=env,timeout=20)
 assert (q.returncode==0)==ok,(args,q.returncode,q.stdout,q.stderr)
 return json.loads(q.stdout if ok else q.stderr)
def tm(*args):return subprocess.run([T,'-S',str(state/'tmux.sock'),*args],capture_output=True,text=True,timeout=10)
def check(name,fn):fn();results.append({'test':name,'passed':True});print('PASS',name,flush=True)
try:
 pr=cli('project','add','--name','batch-guard','--root',root)
 ag=cli('agent','add','--project',pr['id'],'--name','sleeper','--cwd',root,'--','/bin/sleep','300')
 run=cli('agent','start','--agent',ag['id']);registry=json.loads((state/'registry.json').read_text());marker=registry['server_instance']
 shim.write_text('#!/usr/bin/env python3\n'+f'''import json,os,pathlib,subprocess,sys
with open({str(calls)!r},'a') as f:f.write(json.dumps(sys.argv[1:])+'\\n')
mode=pathlib.Path({str(mode)!r}).read_text().strip() if pathlib.Path({str(mode)!r}).exists() else 'normal'
if ';' in sys.argv:
 if mode=='first-fails':
  print('simulated marker subcommand failure',file=sys.stderr);sys.exit(1)
 if mode=='second-fails':
  q=subprocess.run([{T!r},*sys.argv[1:]],capture_output=True);sys.stdout.buffer.write(q.stdout);print('simulated pane subcommand failure',file=sys.stderr);sys.exit(1)
 if mode=='missing-marker':
  print('\\n$1\\t@1\\t%1\\t123\\t0\\t\\tr\\ta\\tp\\t0\\t0');sys.exit(0)
 if mode=='malformed-panes':
  print('@pctl_instance '+{marker!r}+'\\nshort\\trow');sys.exit(0)
os.execv({T!r},[{T!r},*sys.argv[1:]])
''');shim.chmod(0o700)
 def single_call():
  calls.write_text('');v=cli('agent','list',wrapped=True);assert v['connected'] and v['agents'][0]['process']=='running'
  trace=[json.loads(line) for line in calls.read_text().splitlines()];assert len(trace)==1 and ';' in trace[0],trace
 check('snapshot uses exactly one tmux client for marker plus pane reads',single_call)
 for failure in ['first-fails','second-fails','missing-marker','malformed-panes']:
  def failed_snapshot(failure=failure):
   mode.write_text(failure);v=cli('agent','list',wrapped=True);assert not v['connected'] and v['error'];assert v['agents'][0]['process']=='unknown' and v['agents'][0]['task']=='unknown',v
  check(f'{failure} never exposes partial live state',failed_snapshot)
 mode.write_text('normal')
 def fresh_guard():
  assert cli('agent','list',wrapped=True)['connected']
  assert tm('set-option','-g','@pctl_instance','foreign-instance').returncode==0
  assert not cli('agent','list',wrapped=True)['connected']
  calls.write_text('');err=cli('agent','terminate','--agent',ag['id'],'--expected-run',run['id'],ok=False,wrapped=True)
  assert err['error_code']=='target_unverified',err
  trace=[json.loads(line) for line in calls.read_text().splitlines()]
  assert trace and all('kill-pane' not in row for row in trace) and all(';' not in row for row in trace),trace
  assert tm('display-message','-p','-t',run['pane'],'#{pane_pid}').stdout.strip()==run['pid']
 check('valid prior snapshot cannot authorize mutation after native server identity changes',fresh_guard)
 def multiline_marker():
  fake_row='$999\t@999\t%999\t123\t0\t\tforged\tforged\tforged\t0\t0'
  assert tm('set-option','-g','@pctl_instance',marker+'\n'+fake_row).returncode==0
  v=cli('agent','list',wrapped=True);assert not v['connected'] and v['agents'][0]['process']=='unknown'
  err=cli('agent','terminate','--agent',ag['id'],'--expected-run',run['id'],ok=False,wrapped=True)
  assert err['error_code']=='target_unverified' and tm('display-message','-p','-t',run['pane'],'#{pane_pid}').stdout.strip()==run['pid']
 check('multiline global marker containing a valid fake pane row stays disconnected and cannot authorize controls',multiline_marker)
 def restored():
  assert tm('set-option','-g','@pctl_instance',marker).returncode==0
  assert cli('agent','list',wrapped=True)['connected']
 check('isolated fixture restoration is freshly observed without cached state',restored)
finally:
 tm('kill-server')
 report=pathlib.Path(a.report);report.parent.mkdir(parents=True,exist_ok=True);report.write_text(json.dumps({'passed':len(results)==8,'binary_sha256':hashlib.sha256(pathlib.Path(B).read_bytes()).hexdigest(),'tests':results,'scope':'isolated native tmux; shim failure injection is explicit test data'},indent=2)+'\n')
