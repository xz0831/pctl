#!/usr/bin/env python3
"""Two-project synthetic team acceptance against an isolated, private tmux server.
No real models, account access, arbitrary message execution, or preexisting servers.
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, subprocess, tempfile, time
os.environ['HERDR_ENV'] = '1'  # Only disposable test servers bypass the Herdr startup gate.
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',default='test-results/v06-orchestration.json');args=p.parse_args()
B=str(pathlib.Path(args.bin).resolve());T=str(pathlib.Path(args.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-team-',dir=os.path.realpath('/tmp')));state=root/'s';results=[]
baseenv={k:v for k,v in os.environ.items() if not k.startswith('PCTL_')}
def pid_cwd(pid):
 out=subprocess.run(['lsof','-a','-d','cwd','-Fn','-p',str(pid)],text=True,capture_output=True).stdout
 return pathlib.Path(next((l[1:] for l in out.splitlines() if l.startswith('n')),'/nonexistent')).resolve()
def cli(*a,env=None,ok=True):
 q=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,a)],env=env or baseenv,text=True,capture_output=True,timeout=25)
 if ok: assert q.returncode==0,(a,q.stderr,q.stdout)
 else: assert q.returncode!=0,(a,q.stdout)
 try:return json.loads(q.stdout if ok else q.stderr)
 except json.JSONDecodeError:return {'error':q.stderr,'returncode':q.returncode}
def tm(*a):
 q=subprocess.run([T,'-S',str(state/'tmux.sock'),*map(str,a)],text=True,capture_output=True,timeout=10);assert q.returncode==0,(a,q.stderr);return q.stdout.strip()
def case(name,fn):
 start=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-start,4)});print('PASS',name,flush=True)
def context(a,r):
 return {**baseenv,'PCTL_BIN':B,'PCTL_STATE':str(state),'PCTL_TMUX':T,'PCTL_PROJECT_ID':a['project_id'],'PCTL_AGENT_ID':a['id'],'PCTL_RUN_ID':r['id']}
def reg(project,name,worker=True,cwd=None):
 cwd=cwd or pathlib.Path(project['root'])/name;pathlib.Path(cwd).mkdir(exist_ok=True)
 command=[B,'worker','--label',name] if worker else ['/bin/sleep','600']
 a=cli('agent','add','--project',project['id'],'--name',name,'--cwd',cwd,'--adapter','synthetic' if worker else 'generic','--',*command)
 cli('agent','configure-collaboration','--agent',a['id'],'--mode','mailbox');return a
def grant(a,r,targets,allow=None,live=2,budget=8,ttl=3600,replace=None):
 flags=[]
 for target in targets: flags+=['--target',target['id']]
 for cap in allow or ['launch','focus','pair','unpair','zoom','stage','interrupt','stop','restart']:flags+=['--allow',cap]
 if replace: flags+=['--replace-grant',replace]
 return cli('team','grant','--controller',a['id'],'--controller-run',r['id'],'--max-live',live,'--max-launches',budget,'--ttl-seconds',ttl,*flags)['grant']
def team(env,*a,ok=True):return cli('team',*a,env=env,ok=ok)
def launch(env,profile,key):return team(env,'launch',profile,'--key',key)['result']
def livewin(ch):return tm('display-message','-p','-t',ch['binding']['pane'],'#{window_id}')
def inspect(a):return cli('agent','inspect','--agent',a['id'])
try:
 projects=[]
 for name in ['alpha','beta']:
  d=root/name;d.mkdir();projects.append(cli('project','add','--name',name,'--root',d))
 pa,pb=projects
 aa=reg(pa,'main',False);ab=reg(pb,'main',False);ra=cli('agent','start','--agent',aa['id']);rb=cli('agent','start','--agent',ab['id']);ea=context(aa,ra);eb=context(ab,rb)
 wa=reg(pa,'worker');qa=reg(pa,'reviewer');sa=reg(pa,'spare');wb=reg(pb,'worker')
 def default_denied():
  assert 'no operator grant' in team(ea,'launch','worker','--key','no-grant',ok=False)['error']
  assert not inspect(wa)['run'];assert 'operator_only' in team(ea,'policy',ok=False)['error']
 case('disabled by default and launcher context cannot configure grants',default_denied)
 def invalid_grants():
  common=['team','grant','--controller',aa['id'],'--controller-run',ra['id'],'--allow','launch']
  assert 'cross-project' in cli(*common,'--target',wb['id'],ok=False)['error']
  assert 'itself' in cli(*common,'--target',aa['id'],ok=False)['error']
  outside=reg(pa,'outside',False,cwd=root/'outside')
  assert 'outside registered project' in cli(*common,'--target',outside['id'],ok=False)['error']
 case('grant rejects cross-project, self-child and outside-root workdir',invalid_grants)
 ga=grant(aa,ra,[wa,qa,sa]);gb=grant(ab,rb,[wb],live=1,budget=2)
 def profiles():
  v=team(ea,'profiles');assert {a['profile'] for a in v['profiles']}=={'worker','reviewer','spare'};assert v['limits']['attempts']==0
  assert all(str(pathlib.Path(a['cwd'])).startswith(pa['root']+'/') for a in v['profiles'])
 case('profile discovery returns only granted exact same-project workdirs',profiles)
 ca=launch(ea,'worker','worker-1');cb=launch(eb,'worker','worker-1')
 def two_projects():
  assert ca['binding']['project_id']==pa['id'];assert cb['binding']['project_id']==pb['id'];assert ca['binding']['id']!=cb['binding']['id']
  for c,a in [(ca,wa),(cb,wb)]:
   for _ in range(100):
    if pid_cwd(c['binding']['pid'])==pathlib.Path(a['cwd']):break
    time.sleep(.02)
   assert pid_cwd(c['binding']['pid'])==pathlib.Path(a['cwd'])
   assert c['task']=='unknown'
 case('two projects launch distinct registered native processes in role workdirs',two_projects)
 def dedup():
  with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
   replies=list(pool.map(lambda _:team(ea,'launch','worker','--key','worker-1'),range(3)))
  assert all(v['deduplicated'] and v['result']['binding']['id']==ca['binding']['id'] for v in replies)
  assert team(ea,'profiles')['limits']['attempts']==1
  assert 'idempotency_conflict' in team(ea,'launch','reviewer','--key','worker-1',ok=False)['error']
 case('concurrent launch retries preserve one child and one budget debit',dedup)
 cq=launch(ea,'reviewer','reviewer-1')
 def live_limit():
  assert 'launch_limit' in team(ea,'launch','spare','--key','spare-1',ok=False)['error'];assert not inspect(sa)['run']
 case('bounded live children limit rejects a third process without starting it',live_limit)
 def isolation():
  assert 'ownership_denied' in team(ea,'stop',cb['child'],ok=False)['error']
  assert 'ownership_denied' in team(eb,'focus',ca['child'],ok=False)['error']
  assert 'ownership_denied' in team(ea,'pair',ca['child'],'--with',cb['child'],ok=False)['error']
  assert inspect(wb)['run']['id']==cb['binding']['id']
 case('cross-project and foreign owned handles cannot be focused, paired or stopped',isolation)
 def layout():
  team(ea,'pair',ca['child'],'--with','self');assert livewin(ca)==tm('display-message','-p','-t',ra['pane'],'#{window_id}')
  team(ea,'unpair',ca['child']);assert livewin(ca)!=tm('display-message','-p','-t',ra['pane'],'#{window_id}')
  team(ea,'pair',ca['child'],'--with',cq['child']);assert livewin(ca)==livewin(cq)
  team(ea,'focus',ca['child']);assert tm('display-message','-p','-t',ca['binding']['pane'],'#{pane_active}')=='1'
  team(ea,'zoom',ca['child']);assert tm('display-message','-p','-t',ca['binding']['pane'],'#{window_zoomed_flag}')=='1';team(ea,'zoom',ca['child'])
  team(ea,'unpair',ca['child']);assert livewin(ca)!=livewin(cq)
  assert inspect(wa)['run']['id']==ca['binding']['id'];assert inspect(qa)['run']['pid']==cq['binding']['pid']
 case('focus, controller split view, peer pair, unpair and zoom preserve exact processes',layout)
 def staging_mail():
  token='NO_EXECUTE_OR_APPROVE_7491'
  out=team(ea,'stage',ca['child'],'--text',token);assert 'nothing transmitted' in out['result']
  draft=cli('input','read-draft','--agent',wa['id'],'--expected-run',ca['binding']['id']);assert draft['text']==token
  mail=cli('msg','send','worker','--key','dispatch-1','--text','Synthetic mailbox task '+token,env=ea)
  assert token not in tm('capture-pane','-p','-t',ca['binding']['pane'])
  # Durable message dispatch is independent of native terminal staging.
  echild=context(wa,ca['binding']);inbox=cli('msg','inbox',env=echild);assert token in json.dumps(inbox)
  assert team(ea,'submit',ca['child'],ok=False)['returncode']!=0
 case('stage stays local and msg dispatch stays mailbox data without terminal execution',staging_mail)
 def restart():
  global ca
  old=ca;team(ea,'interrupt',old['child']);time.sleep(.15)
  new=team(ea,'restart',old['child'],'--key','restart-1');ca=new['result'];assert ca['child']!=old['child'];assert ca['binding']['id']!=old['binding']['id']
  again=team(ea,'restart',old['child'],'--key','restart-1');assert again['deduplicated'];assert again['result']['binding']['id']==ca['binding']['id']
  assert 'stale_run' in team(ea,'focus',old['child'],ok=False)['error']
  assert cli('input','read-draft','--agent',wa['id'],'--expected-run',ca['binding']['id'])['text'] is None
 case('interrupt is explicit; restart returns new exact handle and replay cannot restart twice',restart)
 def stopped_history():
  team(ea,'stop',cq['child']);assert inspect(qa)['run'] is None
  replay=team(ea,'launch','reviewer','--key','reviewer-1');assert replay['deduplicated'];assert not replay['result']['current'];assert not inspect(qa)['run']
  assert any(c['child']==cq['child'] and not c['current'] for c in team(ea,'children')['children'])
 case('stopped child handle and launch key remain historical and cannot silently relaunch',stopped_history)
 def frozen_profile():
  cli('agent','edit','--agent',qa['id'],'--name','reviewer','--cwd',qa['cwd'],'--adapter','generic','--','/bin/sleep','600')
  assert 'profile changed' in team(ea,'launch','reviewer','--key','changed-profile',ok=False)['error'];assert not inspect(qa)['run']
 case('editing a stopped target invalidates its frozen grant before another start',frozen_profile)
 def capability_revoke():
  cli('team','revoke','--grant',ga['id'])
  assert 'no operator grant' in team(ea,'stage',ca['child'],'--text','not-staged',ok=False)['error']
  assert 'no operator grant' in team(ea,'stop',ca['child'],ok=False)['error']
  assert team(ea,'children')['children'];assert inspect(wa)['run']['id']==ca['binding']['id']
 case('revocation blocks all mutations while children remain visible and running',capability_revoke)
 ga2=grant(aa,ra,[wa,qa,sa],allow=['launch','stop'],replace=ga['id'],budget=3)
 def capabilities_budget():
  assert 'not granted' in team(ea,'focus',ca['child'],ok=False)['error']
  assert team(ea,'profiles')['limits']['attempts']==3
  assert 'launch_limit' in team(ea,'launch','spare','--key','after-regrant',ok=False)['error']
 case('specific capabilities and per-run launch budget persist across grant replacement',capabilities_budget)
 def no_adoption():
  direct=cli('agent','start','--agent',sa['id'])
  assert 'no implicit adoption' in team(ea,'launch','spare','--key','operator-existing',ok=False)['error']
  assert inspect(sa)['run']['id']==direct['id']
 case('operator-started existing peer is never implicitly adopted by controller',no_adoption)
 def expired():
  grant(ab,rb,[wb],allow=['stop'],live=1,budget=2,ttl=1,replace=gb['id']);time.sleep(1.1)
  assert 'expired' in team(eb,'stop',cb['child'],ok=False)['error'];assert team(eb,'children')['children']
 case('grant TTL expires mutation rights without hiding or stopping existing children',expired)
 def actor_restart():
  new=cli('agent','restart','--agent',aa['id'],'--expected-run',ra['id']);enew=context(aa,new)
  assert 'stale_run' in team(ea,'stop',ca['child'],ok=False)['error']
  assert 'no operator grant' in team(enew,'stop',ca['child'],ok=False)['error']
  assert team(enew,'children')['children']==[];old=team(ea,'children');assert not old['controller_current'];assert old['children']
  assert inspect(wa)['run']['id']==ca['binding']['id'];assert cli('team','policy')['policy']['attempts']
 case('controller restart loses grants and ownership; orphaned child remains operator-visible',actor_restart)
 report={'passed':True,'tests':results,'binary':B,'sha256':hashlib.sha256(pathlib.Path(B).read_bytes()).hexdigest(),'scope':'isolated two-project real tmux, registered synthetic workers and generic sleep controllers; no actual model claim','temporary_root':str(root)}
 pathlib.Path(args.report).parent.mkdir(parents=True,exist_ok=True);pathlib.Path(args.report).write_text(json.dumps(report,indent=2)+'\n')
finally:
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
