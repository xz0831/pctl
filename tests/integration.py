#!/usr/bin/env python3
"""Real tmux integration suite. Every server is isolated and removed by this script."""
import argparse, concurrent.futures, json, os, pathlib, shutil, subprocess, tempfile, time, shlex
os.environ['PCTL_STANDALONE'] = '1'  # Only disposable test servers start themselves.
parser=argparse.ArgumentParser(); parser.add_argument('--bin',required=True); parser.add_argument('--tmux',required=True);parser.add_argument('--report',default='test-results/integration.json');args=parser.parse_args()
B=str(pathlib.Path(args.bin).resolve()); T=str(pathlib.Path(args.tmux).resolve()); root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-it-',dir=os.path.realpath('/tmp'))); state=root/'state'; results=[]
def cli(*a,ok=True):
 p=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,a)],text=True,capture_output=True,timeout=15)
 if ok and p.returncode: raise AssertionError(f'{a}: {p.stderr} {p.stdout}')
 if not ok:
  assert p.returncode, (a,p.stdout)
  return json.loads(p.stderr)
 return json.loads(p.stdout)
def tm(*a,ok=True):
 p=subprocess.run([T,'-S',str(state/'tmux.sock'),'-f','/dev/null',*map(str,a)],text=True,capture_output=True,timeout=10)
 if ok: assert p.returncode==0,(a,p.stderr)
 return p.stdout.strip()
def case(name,fn):
 t=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-t,4)});print('PASS',name,flush=True)
def listv():return cli('agent','list')['agents']
def inspect(a):return cli('agent','inspect','--agent',a)
def send(a,r,text,ok=True):return cli('input','send-fixture','--agent',a,'--expected-run',r,'--text',text,ok=ok)
def wait_capture(pane,text):
 for _ in range(50):
  if text in tm('capture-pane','-p','-t',pane):return
  time.sleep(.05)
 raise AssertionError(f'missing {text} in {pane}')
def start_agent(project,name='generic',argv=None,adapter='generic',cwd=None):
 a=cli('agent','add','--project',project,'--name',name,'--cwd',cwd or root,'--adapter',adapter,'--',*(argv or ['/bin/sleep','60']))
 r=cli('agent','start','--agent',a['id']);return a,r
try:
 demo=cli('demo'); ps=demo['projects']; rs=demo['runs']; ag=[r['agent_id'] for r in rs]
 def four():
  s=cli('agent','list');assert s['connected'];assert len(s['agents'])==4;assert all(a['process']=='running' for a in s['agents'])
  for r in rs:
   send(r['agent_id'],r['id'],'echo isolation-'+r['id']);wait_capture(r['pane'],'RECEIVED run='+r['id']+' echo isolation-')
   for other in rs:
    if other['id']!=r['id']:assert 'isolation-'+r['id'] not in tm('capture-pane','-p','-t',other['pane'])
 case('2x2 four native workers and exact input recipients',four)
 def server_options():
  assert tm('show-options','-gv','history-limit').strip()=='20000', tm('show-options','-gv','history-limit')
  assert tm('show-options','-gv','set-clipboard').strip()=='external', tm('show-options','-gv','set-clipboard')
 case('the server keeps 20000 lines of scrollback and leaves the clipboard to the terminal',server_options)
 def draft():
  f=root/'draft.txt';f.write_text('draft only\nexit 17\n한글;');r=rs[0];cli('input','draft','--agent',r['agent_id'],'--expected-run',r['id'],'--from-file',f)
  assert 'draft only' not in tm('capture-pane','-p','-t',r['pane']);v=json.loads((state/'drafts'/f"{r['id']}.json").read_text());assert v['sent'] is False
 case('multiline drafts never reach the agent PTY',draft)
 def same_names():
  p1=cli('project','add','--name','duplicate','--root',root);p2=cli('project','add','--name','duplicate','--root',root);assert p1['id']!=p2['id'];cli('project','rename','--project',p1['id'],'--name','새 이름');assert next(p for p in cli('project','list') if p['id']==p1['id'])['name']=='새 이름'
 case('duplicate and renamed project labels preserve UUID identity',same_names)
 def pair():
  a,b=rs[:2];cli('layout','pair','--agent',a['agent_id'],'--expected-run',a['id'],'--with-agent',b['agent_id'],'--with-run',b['id']);va,vb=inspect(a['agent_id']),inspect(b['agent_id']);assert va['run']['window']==vb['run']['window'];assert vb['run']['pane']==b['pane'];assert vb['run']['id']==b['id']
  tm('rename-window','-t',va['run']['window'],'renamed');tm('move-window','-s',va['run']['window'],'-t',a['session']+':9');cli('agent','focus','--agent',b['agent_id'],'--expected-run',b['id']);send(b['agent_id'],b['id'],'echo after-reindex');wait_capture(b['pane'],'after-reindex')
 case('pair, rename, reindex and focus preserve native pane identity',pair)
 def cross():
  a,b=rs[0],rs[2];e=cli('layout','pair','--agent',a['agent_id'],'--expected-run',a['id'],'--with-agent',b['agent_id'],'--with-run',b['id'],ok=False);assert 'cross-project' in e['error']
 case('cross-project pairing rejected',cross)
 def sync():
  r=rs[0];w=inspect(r['agent_id'])['run']['window'];tm('set-option','-w','-t',w,'synchronize-panes','on');e=send(r['agent_id'],r['id'],'echo must-not-send',ok=False);assert 'synchronize-panes' in e['error'];tm('set-option','-w','-t',w,'synchronize-panes','off');assert 'must-not-send' not in tm('capture-pane','-p','-t',r['pane'])
 case('synchronize-panes input is refused before delivery',sync)
 def copy():
  r=rs[0];tm('copy-mode','-t',r['pane']);e=send(r['agent_id'],r['id'],'echo must-not-copy',ok=False);assert 'mode' in e['error'];tm('send-keys','-X','-t',r['pane'],'cancel')
 case('copy-mode input is refused',copy)
 def split_zoom():
  r=rs[1];cli('layout','split','--agent',r['agent_id'],'--expected-run',r['id']);assert inspect(r['agent_id'])['run']['window']!=inspect(rs[0]['agent_id'])['run']['window'];cli('layout','zoom','--agent',r['agent_id'],'--expected-run',r['id'])
 case('separate and zoom do not restart workers',split_zoom)
 def restart():
  r=rs[0];new=cli('agent','restart','--agent',r['agent_id'],'--expected-run',r['id']);assert new['id']!=r['id'];e=send(r['agent_id'],r['id'],'echo stale',ok=False);assert 'stale_run' in e['error'];assert not (state/'drafts'/f"{new['id']}.json").exists();rs[0]=new
 case('restart creates a fresh generation and rejects old run/draft bindings',restart)
 def terminate_joined():
  a,b=rs[:2];cli('layout','pair','--agent',a['agent_id'],'--expected-run',a['id'],'--with-agent',b['agent_id'],'--with-run',b['id']);cli('agent','terminate','--agent',a['agent_id'],'--expected-run',a['id']);assert inspect(b['agent_id'])['process']=='running';send(b['agent_id'],b['id'],'echo sibling-survived');wait_capture(b['pane'],'sibling-survived')
 case('terminate one joined pane leaves sibling running',terminate_joined)
 def generic():
  a,r=start_agent(ps[0]['id']);v=inspect(a['id']);assert v['process']=='running';assert v['task']=='unknown';e=send(a['id'],r['id'],'hello',ok=False);assert 'unsupported' in e['error']
  a2,r2=start_agent(ps[0]['id'],'exit-zero',['/bin/sh','-c','exit 0'])
  for _ in range(40):
   v=inspect(a2['id'])
   if v['process']!='starting' and v['process']!='running':break
   time.sleep(.05)
  assert v["process"]=="exited(0)",v;assert v['task']=='unknown';e=cli('agent','start','--agent',a2['id'],ok=False);assert 'existing run' in e['error']
 case('silent generic running and exit-zero never imply task completion',generic)
 def concurrent_writes():
  with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
   ids=list(pool.map(lambda i:cli('project','add','--name',f'parallel-{i}','--root',root)['id'],range(12)))
  assert len(set(ids))==12;reg=json.loads((state/'registry.json').read_text());assert all(i in [p['id'] for p in reg['projects']] for i in ids)
 case('parallel CLI mutations serialize without lost registry writes',concurrent_writes)
 def argv():
  cwd=root/"quote'; #\n한글;";cwd.mkdir();payload="literal '; touch NEVER; #\n#{pane_id} \\\"";code='import sys, time; print(repr(sys.argv[1]),flush=True); time.sleep(60)';a,r=start_agent(ps[1]['id'],'argv-test',['/usr/bin/python3','-c',code,payload],cwd=cwd);wait_capture(r['pane'],'literal');assert not (cwd/'NEVER').exists();assert inspect(a['id'])['process']=='running'
 case('quotes, semicolons, newline, format syntax and Unicode launch literally',argv)
 def external_respawn():
  r=rs[2];tm('respawn-pane','-k','-t',r['pane'],'exec /bin/sleep 60');v=inspect(r['agent_id']);assert v['process']=='unknown';assert 'identity changed' in v['guard_error'];assert 'identity changed' in send(r['agent_id'],r['id'],'echo stale',ok=False)['error']
 case('external same-pane respawn is locked by process identity',external_respawn)
 def external_move():
  r=rs[3];tm('join-pane','-d','-s',r['pane'],'-t',rs[1]['pane']);v=inspect(r['agent_id']);assert 'outside registered project' in v['guard_error'];assert 'outside registered project' in send(r['agent_id'],r['id'],'echo wrong-project',ok=False)['error']
 case('external cross-project move locks controls',external_move)
 def failed_launch():
  a=cli('agent','add','--project',ps[0]['id'],'--name','gated-fault','--cwd',root,'--adapter','generic','--','/bin/sh','-c',f'touch {root}/MUST_NOT_RUN; sleep 60')
  wrapper=root/'fault-tmux';created=root/'created-pane.txt'
  wrapper.write_text('#!/usr/bin/python3\nimport os,subprocess,sys\n'+f'T={T!r}\ncreated={str(created)!r}\n'+
   'args=sys.argv[1:];i=0\nwhile i<len(args) and args[i] in ["-u","-S","-f"]:\n i+=1 if args[i]=="-u" else 2\n'+
   'if i<len(args) and args[i]=="new-window":\n with open(created,"w") as output:subprocess.run([T,*args],stdout=output)\n raise SystemExit(66)\nos.execv(T,[T,*args])\n');wrapper.chmod(0o700)
  p=subprocess.run([B,'--state',str(state),'--tmux',str(wrapper),'agent','start','--agent',a['id']],text=True,capture_output=True,timeout=10);assert p.returncode!=0
  assert (state/f"pending-{a['id']}.json").exists();assert not (root/'MUST_NOT_RUN').exists()
  e=cli('agent','start','--agent',a['id'],ok=False);assert 'unresolved launch intent' in e['error']
  assert not (root/'MUST_NOT_RUN').exists()
 case('partial tmux launch failure remains gated and refuses duplicate retry',failed_launch)
 def replace_server():
  tm('kill-server');time.sleep(.1);s=cli('agent','list');assert not s['connected'];assert all(a['process']=='unknown' for a in s['agents'] if a['run']);tm('new-session','-d','-s','foreign','exec /bin/sleep 60');s=cli('agent','list');assert not s['connected'];r=rs[1];assert 'server identity changed' in send(r['agent_id'],r['id'],'echo wrong-server',ok=False)['error'];assert 'wrong-server' not in tm('capture-pane','-p','-t','foreign')
 case('disconnection differs from missing and server replacement is locked',replace_server)
 print(f'ALL {len(results)} INTEGRATION TESTS PASSED',flush=True)
finally:
 tm('kill-server',ok=False)
 report=pathlib.Path(args.report);report.parent.mkdir(parents=True,exist_ok=True);report.write_text(json.dumps({'suite':'real tmux synthetic integration','tmux':subprocess.check_output([T,'-V'],text=True).strip(),'binary':B,'tests':results,'passed':len(results),'note':'Actual-model quality, target desktop, clipboard, external descendant cleanup, malicious same-user races and performance budgets are not certified.'},indent=2)+'\n')
 shutil.rmtree(root)
