#!/usr/bin/env python3
"""Disposable native tmux sidebar contract tests. Four inert fixtures, no model."""
import os,argparse,json,pathlib,subprocess,tempfile,time,uuid
os.environ['HERDR_ENV'] = '1'  # Only disposable test servers bypass the Herdr startup gate.
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);a=p.parse_args()
B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-side-',dir=os.path.realpath('/tmp')));state=root/'s';results=[];commands=[]
def cli(*args,ok=True):
 r=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,args)],capture_output=True,text=True,timeout=20)
 commands.append({'args':list(map(str,args)),'exit':r.returncode,'stdout':r.stdout,'stderr':r.stderr})
 assert (r.returncode==0)==ok,(args,r.returncode,r.stdout,r.stderr)
 return json.loads(r.stdout if ok else r.stderr)
def tm(*args,ok=True):
 r=subprocess.run([T,'-S',str(state/'tmux.sock'),'-f','/dev/null',*map(str,args)],capture_output=True,text=True,timeout=10)
 if ok:assert r.returncode==0,(args,r.stderr)
 return r.stdout.strip()
def wait(fn,why,seconds=5):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if fn():return
  time.sleep(.05)
 raise AssertionError(why)
def case(name,fn):
 start=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-start,4)});print('PASS',name,flush=True)
def all_views():return cli('agent','list')['agents']
def inspect(agent):return cli('agent','inspect','--agent',agent)
def signature():
 return {v['agent']['id']:{k:v['run'][k] for k in ['id','pane','pid','process_start','session','window']} for v in all_views()}
def open_side(index=0,width=28):
 agent,run=agents[index];return cli('sidebar','open','--agent',agent['id'],'--expected-run',run['id'],'--width',width)
def focus(index):
 agent,run=agents[index];return cli('agent','focus','--agent',agent['id'],'--expected-run',run['id'])
def sid():return cli('sidebar','inspect')['sidebar']
def side_window():return tm('display-message','-p','-t',sid()['pane'],'#{window_id}')
def new_agent(project,name):
 script=root/f'{name}.py';script.write_text('import os,time\nwhile True:\n data=os.read(0,1024)\n if not data:break\n')
 agent=cli('agent','add','--project',project['id'],'--name',name,'--cwd',root,'--','/usr/bin/python3',str(script));run=cli('agent','start','--agent',agent['id']);return agent,run
try:
 projects=[cli('project','add','--name',name,'--root',root) for name in ['Alpha','Beta']]
 agents=[new_agent(projects[i//2],f'agent-{i+1}') for i in range(4)]
 baseline=signature();initial_panes=len(tm('list-panes','-a','-F','#{pane_id}').splitlines())
 def optional():
  assert sid() is None;focus(0);assert signature()==baseline
 case('sidebar is opt-in and ordinary focus works without it',optional)
 current=open_side()
 def opened():
  assert current['width']==28;assert current['host_agent_id']==agents[0][0]['id'];assert len(tm('list-panes','-a','-F','#{pane_id}').splitlines())==initial_panes+1
  wait(lambda:'pctl' in tm('capture-pane','-p','-t',current['pane']),'sidebar did not render')
  assert signature()==baseline
 case('one native sidebar opens without changing any agent identity',opened)
 def repeated():
  again=open_side(0,30);assert again['id']==current['id'];assert again['pid']==current['pid'];assert again['pane']==current['pane'];assert len(tm('list-panes','-a','-F','#{pane_id}').splitlines())==initial_panes+1
  open_side(0,28)
 case('repeated open reuses sidebar generation and process',repeated)
 def cross_focus():
  for i in [1,2,3,0]:
   focus(i);assert side_window()==baseline[agents[i][0]['id']]['window'];assert signature()==baseline
   assert sid()['pane']==current['pane'];assert sid()['pid']==current['pid']
 case('same and cross-project focus moves only sidebar',cross_focus)
 def split_noop():
  agent,run=agents[0];cli('layout','split','--agent',agent['id'],'--expected-run',run['id']);assert signature()==baseline;assert side_window()==run['window']
 case('separate ignores sidebar when no second agent is paired',split_noop)
 def dangerous_keys():
  tm('send-keys','-t',current['pane'],'-l','xsruipbzSaNedy');time.sleep(.2);assert signature()==baseline;assert len(all_views())==4
 case('compact UI ignores lifecycle input and approval shortcuts',dangerous_keys)
 def keyboard_manager():
  focus(0);cli('sidebar','manager','--project',projects[0]['id']);assert tm('display-message','-p','-t',agents[0][1]['session'],'#{window_name}')=='manager';assert signature()==baseline
  focus(0)
 case('manager route verifies live executable and exact UI arguments',keyboard_manager)
 def mouse():
  # v0.5 header rows0-2; Alpha3, agent1=4, agent2=5, Beta6, agent3=7, agent4=8.
  focus(0);tm('send-keys','-t',current['pane'],'-l','\x1b[<0;4;9M')
  wait(lambda:side_window()==agents[3][1]['window'],'sidebar mouse focus did not move to exact fourth agent');assert signature()==baseline
 case('SGR left-click selects exact row across projects without Enter',mouse)
 def native_input():
  # Literal UI keys never route from the compact pane to an agent. Separately
  # selecting an agent remains native tmux behavior and is not emulated here.
  tm('send-keys','-t',current['pane'],'-l','\x1b[200~DROP_ALL\x1b[201~');time.sleep(.1)
  assert all('DROP_ALL' not in tm('capture-pane','-p','-t',r['pane']) for _,r in agents);assert signature()==baseline
 case('bracketed paste into sidebar is ignored and not forwarded',native_input)
 def zoom():
  agent,run=agents[3];cli('layout','zoom','--agent',agent['id'],'--expected-run',run['id']);assert tm('display-message','-p','-t',run['pane'],'#{window_zoomed_flag}')=='1';assert sid()['id']==current['id']
  cli('layout','zoom','--agent',agent['id'],'--expected-run',run['id']);assert signature()==baseline
 case('native zoom hides and restores sidebar without restart',zoom)
 def stale():
  err=cli('sidebar','open','--agent',agents[0][0]['id'],'--expected-run',str(uuid.uuid4()),ok=False);assert err['error_code']=='stale_run'
  err=cli('sidebar','close','--id',str(uuid.uuid4()),ok=False);assert err['error_code']=='stale_sidebar';assert sid()['id']==current['id'];assert signature()==baseline
 case('stale agent run and sidebar generation fail closed',stale)
 def sync():
  target=agents[1][1];tm('set-window-option','-t',target['window'],'synchronize-panes','on')
  try:assert 'synchronize-panes' in cli('agent','focus','--agent',agents[1][0]['id'],'--expected-run',target['id'],ok=False)['error']
  finally:tm('set-window-option','-t',target['window'],'synchronize-panes','off')
  assert signature()==baseline
 case('synchronized target refuses sidebar moves',sync)
 def manager_replaced():
  session=agents[0][1]['session'];original=tm('display-message','-p','-t',f'{session}:manager','#{window_id}')
  tm('rename-window','-t',original,'original-manager');fake=tm('new-window','-d','-t',session,'-n','manager','-P','-F','#{pane_id}','/bin/sleep 300')
  before=tm('display-message','-p','-t',fake,'#{pane_pid}')
  try:
   error=cli('sidebar','manager','--project',projects[0]['id'],ok=False);assert 'identity' in error['error'];assert tm('display-message','-p','-t',fake,'#{pane_pid}')==before
  finally:tm('kill-pane','-t',fake);tm('rename-window','-t',original,'manager')
  assert signature()==baseline
 case('renamed manager plus replacement shell is never adopted or respawned',manager_replaced)
 def close_reopen():
  cli('sidebar','close','--id',current['id']);assert sid() is None;assert signature()==baseline
  replacement=open_side(0);assert replacement['id']!=current['id'];assert replacement['pane']!=current['pane'];assert signature()==baseline
  current.clear();current.update(replacement)
 case('close removes only sidebar and reopen creates a fresh generation',close_reopen)
 def external_close():
  tm('kill-pane','-t',current['pane']);assert cli('sidebar','inspect')['guard_error'] is not None
  cli('sidebar','close','--id',current['id']);assert sid() is None;assert signature()==baseline
  replacement=open_side(0);current.clear();current.update(replacement)
 case('verified external sidebar deletion has explicit safe reconciliation',external_close)
 def q_close():
  wait(lambda:'pctl' in tm('capture-pane','-p','-t',current['pane']),'reopened sidebar did not render before q')
  tm('send-keys','-t',current['pane'],'-l','q');wait(lambda:sid() is None,'q did not close its own sidebar',8);assert signature()==baseline
  replacement=open_side(0);current.clear();current.update(replacement)
 case('q restores navigation terminal and closes only its own pane',q_close)
 def geometry():
  cli('sidebar','close','--id',current['id']);run=agents[0][1];tm('resize-window','-t',run['window'],'-x','60','-y','14')
  try:assert 'columns' in cli('sidebar','open','--agent',agents[0][0]['id'],'--expected-run',run['id'],ok=False)['error'];assert sid() is None
  finally:tm('resize-window','-t',run['window'],'-x','120','-y','36')
  for width in ['0','19','41']:assert 'width' in cli('sidebar','open','--agent',agents[0][0]['id'],'--expected-run',run['id'],'--width',width,ok=False)['error']
  replacement=open_side(0);current.clear();current.update(replacement)
 case('unsupported width and narrow geometry reject before creating a pane',geometry)
 def tamper():
  tm('respawn-pane','-k','-t',current['pane'],'/bin/sleep 300');newpid=tm('display-message','-p','-t',current['pane'],'#{pane_pid}');assert newpid!=current['pid']
  assert 'identity' in cli('sidebar','close','--id',current['id'],ok=False)['error'];assert tm('display-message','-p','-t',current['pane'],'#{pane_pid}')==newpid;assert signature()==baseline
 case('externally respawned sidebar is protected by process identity',tamper)
 report={'passed':True,'binary':B,'scope':'isolated native tmux, four inert agents, synthetic SGR mouse input; real GUI separately required','tests':results,'commands':commands,'root':str(root)}
 pathlib.Path(a.report).parent.mkdir(parents=True,exist_ok=True);pathlib.Path(a.report).write_text(json.dumps(report,indent=2)+'\n')
 print('ALL',len(results),'SIDEBAR TESTS PASSED',flush=True)
finally:
 diagnostics={'temporary_root':str(root),'completed_tests':results,'commands':commands}
 for name in ['sidebar-close.log','sidebar.json','pending-sidebar.json']:
  path=state/name
  if path.exists():diagnostics[name]=path.read_text(errors='replace')
 diagnostics['native_panes']=subprocess.run([T,'-S',str(state/'tmux.sock'),'list-panes','-a','-F','#{pane_id} #{pane_pid} #{pane_dead} #{pane_current_command}'],capture_output=True,text=True).stdout
 pathlib.Path(a.report+'.diagnostics.json').write_text(json.dumps(diagnostics,indent=2)+'\n')
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
