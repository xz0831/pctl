#!/usr/bin/env python3
"""Disposable sidebar intent fault tests; direct state mutation is test setup only."""
import os,argparse,json,pathlib,shlex,subprocess,tempfile,time,uuid
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);a=p.parse_args()
B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-side-fault-',dir=os.path.realpath('/tmp')));state=root/'s';results=[]
def cli(*args,ok=True,tmux=T):
 r=subprocess.run([B,'--state',str(state),'--tmux',tmux,*map(str,args)],capture_output=True,text=True,timeout=20)
 assert (r.returncode==0)==ok,(args,r.returncode,r.stdout,r.stderr)
 return json.loads(r.stdout if ok else r.stderr)
def tm(*args):
 r=subprocess.run([T,'-S',str(state/'tmux.sock'),'-f','/dev/null',*map(str,args)],capture_output=True,text=True,timeout=10);assert r.returncode==0,(args,r.stderr);return r.stdout.strip()
def case(name,fn):
 start=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-start,4)});print('PASS',name,flush=True)
def signature():
 v=cli('agent','inspect','--agent',agent['id']);return {k:v['run'][k] for k in ['id','pane','pid','process_start','session','window']}
def cleanup(record):
 tm('kill-pane','-t',record['pane'])
 for name in ['sidebar.json','pending-sidebar.json',f"sidebar-{record['id']}.ready"]:(state/name).unlink(missing_ok=True)
def pending(with_registry=False,partial=False):
 pane,pid=tm('split-window','-d','-h','-t',run['pane'],'-P','-F','#{pane_id}\t#{pane_pid}','exec /bin/sleep 300').split('\t')
 start=subprocess.run(['ps','-o','lstart=','-p',str(pid)],text=True,capture_output=True,env={'LC_ALL':'C'}).stdout.strip()
 rec={'schema_version':1,'id':str(uuid.uuid4()),'server_instance':json.loads((state/'registry.json').read_text())['server_instance'],'pane':pane,'pid':pid,'process_start':start,'width':28,'host_project_id':project['id'],'host_agent_id':agent['id'],'host_run_id':run['id'],'started_at':int(time.time()*1000)}
 intent={'sidebar_id':rec['id'],'phase':'gated_pane_created','sidebar':rec,'window':run['window']}
 (state/'pending-sidebar.json').write_text(json.dumps(intent))
 if with_registry:(state/'sidebar.json').write_text(json.dumps(rec))
 if partial:tm('set-option','-p','-t',pane,'@pctl_sidebar',rec['id'])
 return rec,intent
try:
 project=cli('project','add','--name','fault-fixtures','--root',root)
 agent=cli('agent','add','--project',project['id'],'--name','untouched','--cwd',root,'--','/bin/sleep','300');run=cli('agent','start','--agent',agent['id']);baseline=signature()
 def missing_registry():
  rec,_=pending();assert cli('sidebar','inspect')['pending']['sidebar']['id']==rec['id'];cli('sidebar','close','--id',rec['id']);assert not (state/'pending-sidebar.json').exists();assert signature()==baseline
 case('nested exact pending identity recovers without sidebar registry',missing_registry)
 def partial_tags():
  rec,_=pending(with_registry=True,partial=True);cli('sidebar','close','--id',rec['id']);assert not (state/'sidebar.json').exists();assert not (state/'pending-sidebar.json').exists();assert signature()==baseline
 case('known gated launcher with partial tags closes only its pane',partial_tags)
 def mismatch():
  for field in ['window','sidebar_id','server_instance']:
   rec,intent=pending(with_registry=True)
   if field=='server_instance':
    changed=dict(rec,server_instance=str(uuid.uuid4()));(state/'sidebar.json').write_text(json.dumps(changed))
   else:intent[field]='wrong';(state/'pending-sidebar.json').write_text(json.dumps(intent))
   cli('sidebar','close','--id',rec['id'],ok=False);assert tm('display-message','-p','-t',rec['pane'],'#{pane_pid}')==rec['pid'];assert signature()==baseline;cleanup(rec)
 case('wrong pending window generation or server never removes a pane',mismatch)
 def released_gate():
  rec,_=pending(with_registry=True,partial=True);(state/f"sidebar-{rec['id']}.ready").write_text('ready\n');cli('sidebar','close','--id',rec['id'],ok=False);assert tm('display-message','-p','-t',rec['pane'],'#{pane_pid}')==rec['pid'];cleanup(rec);assert signature()==baseline
 case('partial metadata after gate release is not treated as safe gated recovery',released_gate)
 def unknown():
  sid=str(uuid.uuid4());(state/'pending-sidebar.json').write_text(json.dumps({'sidebar_id':sid}));before=tm('list-panes','-a','-F','#{pane_id}')
  cli('sidebar','open','--agent',agent['id'],'--expected-run',run['id'],ok=False);cli('sidebar','close','--id',sid,ok=False);assert tm('list-panes','-a','-F','#{pane_id}')==before;assert (state/'pending-sidebar.json').exists();(state/'pending-sidebar.json').unlink()
 case('unknown launch identity refuses both duplicate creation and guessed cleanup',unknown)
 def gate_timeout():
  wrapper=root/'fail-sidebar-tag';wrapper.write_text('#!/usr/bin/python3\nimport os,sys\n'+f'T={T!r}\n'+
   'args=sys.argv[1:];i=0\nwhile i<len(args) and args[i] in ["-u","-S","-f"]:\n i+=1 if args[i]=="-u" else 2\n'+
   'if i<len(args) and args[i]=="set-option" and "@pctl_sidebar" in args[i+1:]:raise SystemExit(66)\nos.execv(T,[T,*args])\n');wrapper.chmod(0o700)
  cli('sidebar','open','--agent',agent['id'],'--expected-run',run['id'],ok=False,tmux=str(wrapper));rec=cli('sidebar','inspect')['sidebar'];assert rec is not None;assert not (state/f"sidebar-{rec['id']}.ready").exists()
  end=time.monotonic()+18
  while time.monotonic()<end:
   if tm('display-message','-p','-t',rec['pane'],'#{pane_dead}')=='1':break
   time.sleep(.1)
  assert tm('display-message','-p','-t',rec['pane'],'#{pane_dead}')=='1','failed launch gate exceeded bounded wait';assert (state/'pending-sidebar.json').exists();cli('sidebar','close','--id',rec['id']);assert signature()==baseline
 case('real metadata failure leaves inspectable intent and gate waiter exits within bound',gate_timeout)
 print('ALL',len(results),'SIDEBAR RECOVERY TESTS PASSED',flush=True)
finally:
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
 pathlib.Path(a.report).parent.mkdir(parents=True,exist_ok=True);pathlib.Path(a.report).write_text(json.dumps({'binary':B,'tests':results,'passed':len(results),'scope':'isolated native fault injection; direct registry writes used only to construct failure states','temporary_root':str(root)},indent=2)+'\n')
