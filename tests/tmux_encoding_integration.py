#!/usr/bin/env python3
"""Native no-locale/C-locale tmux transport regression. Inert local profiles only."""
import argparse,hashlib,json,os,pathlib,subprocess,tempfile,time
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);a=p.parse_args();B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve());R=pathlib.Path(tempfile.mkdtemp(prefix='pctl-encoding-',dir='/tmp'));S=R/'s';checks=[]
operator={k:v for k,v in os.environ.items() if not k.startswith('PCTL_')};minimal={'PATH':'/usr/bin:/bin','TERM':'xterm-256color','HOME':str(R),'PWD':str(R)}
def cli(*args,env=None,ok=True):
 q=subprocess.run([B,'--state',str(S),'--tmux',T,*map(str,args)],capture_output=True,text=True,env=env or operator,timeout=20);assert (q.returncode==0)==ok,(args,q.returncode,q.stdout,q.stderr);return json.loads(q.stdout if ok else q.stderr)
def check(name,fn):fn();checks.append({'test':name,'passed':True});print('PASS',name,flush=True)
def actor(ag,run,locale):return {**minimal,**locale,'PCTL_BIN':B,'PCTL_STATE':str(S),'PCTL_TMUX':T,'PCTL_PROJECT_ID':ag['project_id'],'PCTL_AGENT_ID':ag['id'],'PCTL_RUN_ID':run['id']}
try:
 project=cli('project','add','--name','encoding','--root',R)
 agents=[]
 for name in ['controller','helper']:
  ag=cli('agent','add','--project',project['id'],'--name',name,'--cwd',R,'--','/bin/sleep','300');cli('agent','configure-collaboration','--agent',ag['id'],'--mode','mailbox');agents.append(ag)
 controller,helper=agents;run=cli('agent','start','--agent',controller['id']);hrun=cli('agent','start','--agent',helper['id'])
 for label,locale in [('unset',{}),('C',{'LANG':'C','LC_ALL':'C'}),('UTF8',{'LANG':'C.UTF-8'})]:
  env=actor(controller,run,locale)
  def observations(env=env):
   snapshot=cli('agent','list',env=env);assert snapshot['connected'] and all(x['process']=='running' for x in snapshot['agents'])
   peers=cli('msg','peers',env=env);assert peers['peers'][0]['role']=='helper' and peers['peers'][0]['send_candidate']
  check(label+' locale: batched snapshot and role peers preserve tab-framed identity',observations)
  def messages(env=env,label=label):
   sent=cli('msg','send','helper','--key',label,'--text','timeout_seconds=37',env=env);h=sent['message']['handle'];received=cli('msg','inbox',env=actor(helper,hrun,locale));assert any(m['handle']==h for m in received['messages'])
  check(label+' locale: fresh guarded send and inbox succeed',messages)
 cli('agent','terminate','--agent',helper['id'],'--expected-run',hrun['id'])
 cli('team','grant','--controller',controller['id'],'--controller-run',run['id'],'--target',helper['id'],'--allow','launch','--allow','stop','--max-live','1','--max-launches','1','--ttl-seconds','300')
 env=actor(controller,run,{})
 def team():
  assert cli('team','profiles',env=env)['profiles'][0]['profile']=='helper'
  launched=cli('team','launch','helper','--key','one',env=env);child=launched['result']['child'];assert cli('team','stop',child,env=env)['ok']
 check('unset locale: typed-adapter discovery equivalent and exact launch/stop work',team)
 def changed_marker():
  reg=json.loads((S/'registry.json').read_text());q=subprocess.run([T,'-u','-S',str(S/'tmux.sock'),'set-option','-g','@pctl_instance','foreign'],capture_output=True);assert q.returncode==0
  assert not cli('agent','list',env=env)['connected'];assert cli('agent','terminate','--agent',controller['id'],'--expected-run',run['id'],env=env,ok=False)['error_code']=='target_unverified'
  subprocess.run([T,'-u','-S',str(S/'tmux.sock'),'set-option','-g','@pctl_instance',reg['server_instance']],check=True,capture_output=True)
 check('unset locale: changed marker remains disconnected and fresh mutation is denied',changed_marker)
finally:
 subprocess.run([T,'-S',str(S/'tmux.sock'),'kill-server'],capture_output=True)
 pathlib.Path(a.report).write_text(json.dumps({'passed':len(checks)==8,'binary_sha256':hashlib.sha256(pathlib.Path(B).read_bytes()).hexdigest(),'tests':checks,'scope':'isolated synthetic profiles; no actual model'},indent=2)+'\n')
