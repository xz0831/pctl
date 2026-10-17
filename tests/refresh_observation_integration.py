#!/usr/bin/env python3
"""PR-A observes fake harnesses only; no model, real HOME, settings or server access."""
import argparse, json, os, pathlib, shlex, subprocess, tempfile, time
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);a=p.parse_args()
B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve())
root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-observe-',dir=os.path.realpath('/tmp')));state=root/'s';home=root/'home';home.mkdir();cfg=home/'.config'/'pctl';cfg.mkdir(parents=True)
env={k:v for k,v in os.environ.items() if not k.startswith(('PCTL_','CODEX_','XDG_','TMUX'))}
env.update(HOME=str(home),CODEX_HOME=str(home/'.codex'),XDG_CONFIG_HOME=str(home/'.config'),PCTL_STANDALONE='1',TERM='xterm-256color',PATH='/usr/bin:/bin')
bindir=root/'bin';bindir.mkdir();fake=bindir/'claude';fake.write_text('''#!/bin/bash
echo '> '
while IFS= read -r line; do
 case "$line" in
 *block*) printf '\\033[2J\\033[HDo you want to proceed?\\n❯ 1. Yes\\n' ;;
 *clear*) printf '\\033[2J\\033[H> \\n' ;;
 *) echo "got: $line"; echo '> ' ;;
 esac
done
''');fake.chmod(0o755)
results=[];sessions={};outer=root/'outer.sock'
def cli(*args,ok=True,extra=None,stdin=None):
 q=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,args)],env=dict(env,**(extra or {})),input=stdin,text=True,capture_output=True,timeout=30)
 assert (q.returncode==0)==ok,(args,q.returncode,q.stdout,q.stderr)
 if not ok:return q.stderr
 return json.loads(q.stdout) if q.stdout.strip() else None
def tm(*args):return subprocess.run([T,'-S',str(state/'tmux.sock'),*map(str,args)],text=True,capture_output=True,check=True).stdout
def case(name,fn):
 t=time.monotonic();fn();results.append(dict(test=name,passed=True,seconds=round(time.monotonic()-t,3)));print('PASS',name,flush=True)
def add(name,project):
 d=root/name;d.mkdir();sessions[name]=cli('session','add','--name',name,'--project',project,'--cwd',d,'--',fake)
def hook(name,event,**fields):
 s=sessions[name];body=dict(session_id=s['conversation'],hook_event_name=event,**fields)
 return cli('session','hook','--refresh-observe',extra={'PCTL_AGENT_ID':s['agent_id'],'PCTL_RUN_ID':s['run_id']},stdin=json.dumps(body))
def ready(name):
 hook(name,'SessionStart');hook(name,'UserPromptSubmit');hook(name,'Stop',background_tasks=[],session_crons=[],stop_hook_active=False)
def dry():return cli('session','refresh','--idle','--dry-run')
def row(report,name):return next(s for p in report['projects'] for s in p['sessions'] if s['name']==name)
def policy(seconds): (cfg/'config.toml').write_text(f'[refresh]\nidle_for_secs = {seconds}\n')
def bridge(op):
 q=subprocess.run([B,'--state',str(state),'--tmux',T,'bridge'],env=env,input=json.dumps(dict(id=1,**op))+'\n',capture_output=True,text=True,timeout=15)
 assert q.returncode==0,(q.stdout,q.stderr)
 v=json.loads(q.stdout.splitlines()[-1]);assert v['ok'],v
 return v
try:
 def snippet():
  doc=pathlib.Path('docs/2026-10-17_harness_refresh_observation_v1.md').read_text()
  hooks=json.loads(doc.split('```json\n',1)[1].split('```',1)[0])['hooks']
  assert set(hooks)==set(['SessionStart','UserPromptSubmit','PreToolUse','PostToolUse','PostToolUseFailure','SubagentStart','SubagentStop','PermissionRequest','Notification','Stop','StopFailure'])
  for groups in hooks.values():
   for group in groups:
    command=group['hooks'][0]['command'];assert '--refresh-observe' in command
    subprocess.run(['/bin/sh','-n','-c',command],check=True,capture_output=True)
  assert hooks['Notification'][0]['matcher']=='permission_prompt'
 case('documented hook JSON and shell commands parse without installing settings',snippet)
 def setup():
  add('a-main','alpha');add('a-peer','alpha');add('b-main','beta')
  # Replacement makes all three stale without touching installed harnesses.
  with fake.open('a') as f:f.write('# replacement\n')
  v=dry();assert v['policy']['idle_for_secs']==1800 and v['policy']['enabled'] is False and not v['would_swap']
  assert all('readiness_unknown' in row(v,n)['reasons'] for n in sessions)
  assert cli('session','refresh','--idle',ok=False)
  assert cli('session','refresh','--dry-run',ok=False)
 case('defaults are disabled/1800 seconds; uninstrumented runs and execution syntax refuse',setup)
 def candidates():
  for n in sessions:ready(n)
  v=dry();assert not v['would_swap'] and all('idle_window_incomplete' in p['reasons'] for p in v['projects'])
  policy(0);v=dry();assert {s['name'] for s in v['would_swap']}==set(sessions),v
  assert all(s['reservation']['active'] is False and s['reservation']['project_epoch'] and 'cohort' not in s['reservation'] for s in v['would_swap'])
  assert v['execution_supported'] is False
 case('all eligible projects considered; readiness and quiet window gate inactive proposals',candidates)
 def no_mutations():
  before=(state/'registry.json').read_bytes();screens={n:tm('capture-pane','-p','-t',s['pane']) for n,s in sessions.items()}
  for _ in range(2):dry()
  assert (state/'registry.json').read_bytes()==before
  assert screens=={n:tm('capture-pane','-p','-t',s['pane']) for n,s in sessions.items()}
 case('dry-run does not change registry, runs, process screens or send input',no_mutations)
 def hooks_fail_closed():
  hook('a-main','Stop',background_tasks=[{'description':'DO_NOT_PERSIST'}],session_crons=[],stop_hook_active=False)
  v=dry();assert not row(v,'a-main')['readiness']['ready']
  assert row(v,'a-main')['readiness']['background_tasks']==1
  assert 'DO_NOT_PERSIST' not in (state/'refresh-observe.json').read_text()
  hook('a-main','Stop',session_crons=[],stop_hook_active=False)
  assert not row(dry(),'a-main')['readiness']['ready']
  ready('a-main')
  s=sessions['a-main']
  identity={'PCTL_AGENT_ID':s['agent_id'],'PCTL_RUN_ID':s['run_id']}
  cli('session','hook',extra=identity,stdin=json.dumps({'session_id':'other-conversation'}))
  before_child=(state/'refresh-observe.json').read_bytes()
  cli('session','hook','--refresh-observe',extra=identity,stdin=json.dumps({'session_id':'other-conversation','hook_event_name':'SessionStart'}))
  assert (state/'refresh-observe.json').read_bytes()==before_child
  changed=row(dry(),'a-main')
  assert changed['conversation']=='other-conversation'
  assert changed['readiness']['conversation']==s['conversation']
  assert 'readiness_unknown' in row(dry(),'a-main')['reasons']
  cli('session','hook',extra=identity,stdin=json.dumps({'session_id':s['conversation']}))
  ready('a-main')
  s=sessions['a-main'];before=(state/'refresh-observe.json').read_bytes()
  cli('session','hook','--refresh-observe',extra={'PCTL_AGENT_ID':s['agent_id'],'PCTL_RUN_ID':'old-run'},stdin=json.dumps({'session_id':s['conversation'],'hook_event_name':'SessionStart'}))
  assert (state/'refresh-observe.json').read_bytes()==before
  cli('session','hook','--refresh-observe',extra={'PCTL_AGENT_ID':s['agent_id'],'PCTL_RUN_ID':s['run_id']},stdin=json.dumps({'session_id':'wrong','hook_event_name':'Stop'}))
  assert (state/'refresh-observe.json').read_bytes()==before
 case('missing/background/stale/mismatched hooks cannot mark ready; payload contents are not stored',hooks_fail_closed)
 def inputs():
  cli('session','keys','a-main','Space')
  hook('a-main','Stop',background_tasks=[],session_crons=[],stop_hook_active=False)
  assert not row(dry(),'a-main')['readiness']['ready']
  ready('a-main');s=sessions['a-main']
  bridge({'op':'keys','agent':'a-main','run':s['run_id'],'keys':[{'literal':True,'key':'draft'}]})
  assert not row(dry(),'a-main')['readiness']['ready'];ready('a-main')
  import base64
  bridge({'op':'clip_put','agent':'a-main','run':s['run_id'],'seq':0,'data':base64.b64encode(b'\x89PNG\r\n\x1a\n').decode()})
  assert not row(dry(),'a-main')['readiness']['ready'];ready('a-main')
 case('CLI keys and bridge draft input invalidate the old turn readiness',inputs)
 def protection():
  cli('session','protect','a-peer');v=dry();names={s['name'] for s in v['would_swap']}
  assert 'a-main' in names and 'a-peer' not in names
  assert 'protected' in row(v,'a-peer')['reasons'];cli('session','protect','a-peer','--off')
 case('idle protected sibling is excluded without hiding the other idle target',protection)
 def queue():
  cli('session','prompt','b-main','block')
  out=cli('tell','b-main','queued-fixture',extra={'PCTL_AGENT_ID':sessions['a-main']['agent_id'],'PCTL_RUN_ID':sessions['a-main']['run_id']})
  assert out['result']=='queued_blocked',out
  tm('send-keys','-t',sessions['b-main']['pane'],'clear','Enter');time.sleep(.1)
  files={p:p.read_bytes() for p in (state/'tell'/'queue').rglob('*.json')}
  assert files
  ready('b-main');v=dry()
  assert 'tell_pending_or_unresolved' in row(v,'b-main')['reasons']
  assert files=={p:p.read_bytes() for p in (state/'tell'/'queue').rglob('*.json')}
  assert 'queued-fixture' not in tm('capture-pane','-p','-t',sessions['b-main']['pane'])
 case('queued tell blocks observation and is never drained by the dry-run',queue)
 def visibility_epochs():
  before=row(dry(),'a-main')['readiness']['revision']
  bridge({'op':'view','runs':[sessions['a-main']['run_id']]})
  after=row(dry(),'a-main')['readiness']['revision'];assert after>before
 case('even a transient viewer changes the cohort epoch',visibility_epochs)
 def raw_client():
  registry=json.loads((state/'registry.json').read_text());session=next(r['session'] for r in registry['runs'] if r['id']==sessions['a-main']['run_id'])
  command='exec '+shlex.join([T,'-S',str(state/'tmux.sock'),'attach-session','-t',session])
  subprocess.run([T,'-S',str(outer),'new-session','-d','-s','watch',command],env=env,check=True,capture_output=True)
  try:
   end=time.monotonic()+5
   while time.monotonic()<end and not tm('list-clients'):time.sleep(.1)
   v=dry();assert 'raw_client_attached' in row(v,'a-main')['reasons']
  finally:subprocess.run([T,'-S',str(outer),'kill-server'],capture_output=True)
 case('raw attached terminal holds the entire project',raw_client)
 def policy_error():
  (cfg/'config.toml').write_text('[refresh]\npoll_interval_secs = 0\n')
  assert 'must be positive' in cli('session','refresh','--idle','--dry-run',ok=False)
  policy(0)
 case('invalid policy fails visibly',policy_error)
 pathlib.Path(a.report).write_text(json.dumps(dict(root=str(root),passed=len(results),results=results),indent=2))
finally:
 subprocess.run([T,'-S',str(outer),'kill-server'],capture_output=True)
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
