#!/usr/bin/env python3
"""Real-tmux local contract checks. Synthetic sleep processes; no model cooperation claim."""
import argparse, concurrent.futures, hashlib, json, os, pathlib, subprocess, tempfile, time
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',required=True);args=p.parse_args()
B=str(pathlib.Path(args.bin).resolve());T=str(pathlib.Path(args.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-wf-',dir=os.path.realpath('/tmp')));state=root/'s';results=[]
base_env={k:v for k,v in os.environ.items() if not k.startswith('PCTL_')}
def cli(*a,who=None,ok=True,env_extra=None):
 env=base_env.copy()
 if who:
  agent,run=who;env.update(PCTL_BIN=B,PCTL_STATE=str(state),PCTL_TMUX=T,PCTL_PROJECT_ID=agent['project_id'],PCTL_AGENT_ID=agent['id'],PCTL_RUN_ID=run['id'])
 if env_extra: env.update(env_extra)
 q=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,a)],text=True,capture_output=True,timeout=20,env=env)
 assert (q.returncode==0)==ok,(a,q.returncode,q.stdout,q.stderr)
 return json.loads(q.stdout if ok else q.stderr)
def tm(*a):return subprocess.run([T,'-S',str(state/'tmux.sock'),*map(str,a)],text=True,capture_output=True,timeout=10)
def case(name,fn):
 started=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-started,4)});print('PASS',name,flush=True)
def create(project,name,mailbox=True,start=True):
 a=cli('agent','add','--project',project['id'],'--name',name,'--cwd',root,'--','/bin/sleep','300')
 if mailbox: cli('agent','configure-collaboration','--agent',a['id'],'--mode','mailbox')
 r=cli('agent','start','--agent',a['id']) if start else None
 return a,r
def msg(who,*args,**kw):return cli('msg',*args,who=who,**kw)
def status(who,h):return msg(who,'status',h)['message']
def stored():return json.loads((state/f"mailbox-{pr['id']}.json").read_text())
try:
 pr=cli('project','add','--name','workflow','--root',root);other=cli('project','add','--name','other','--root',root)
 a=create(pr,'impl');b=create(pr,'reviewer');outsider=create(other,'reviewer');off=create(pr,'disabled',False)
 case('context missing rejects sender invention',lambda: (lambda v: v['error_code']=='context_missing' or (_ for _ in ()).throw(AssertionError(v)))(cli('msg','peers',ok=False)))
 def context():
  for env,code in [({'PCTL_RUN_ID':'stale'},'stale_run'),({'PCTL_PROJECT_ID':other['id']},'target_unverified'),({'PCTL_STATE':str(root)},'context_mismatch'),({'PCTL_TMUX':'/other/tmux'},'context_mismatch')]:
   assert msg(a,'peers',ok=False,env_extra=env)['error_code']==code
 case('sender run/project/state/tmux context bound',context)
 def preflight_no_writes():
  wrong=root/'must-not-create';env=base_env.copy();env.update(PCTL_BIN=B,PCTL_STATE=str(state),PCTL_TMUX=T,PCTL_PROJECT_ID=pr['id'],PCTL_AGENT_ID=a[0]['id'],PCTL_RUN_ID=a[1]['id'])
  for extra in [[],['--theme','mono']]:
   q=subprocess.run([B,'--state',str(wrong),'--tmux',T,*extra,'msg','status','m1'],env=env,text=True,capture_output=True)
   assert q.returncode!=0 and not wrong.exists(),(q.stdout,q.stderr)
  assert not (state/'appearance.json').exists()
  q=subprocess.run([B,'--state',str(wrong),'--tmux',T,'msg','peers'],env=base_env,text=True,capture_output=True)
  assert q.returncode!=0 and not wrong.exists()
 case('wrong or missing context and global theme reject before state writes',preflight_no_writes)
 def roles():
  v=msg(a,'peers');assert {x['role'] for x in v['peers']}=={'reviewer','disabled'}
  for role in ['Reviewer','reviewer ','review','absent']:
   assert msg(a,'send',role,'--key','bad-role','--text','x',ok=False)['error_code']=='not_found'
  assert msg(a,'send','disabled','--key','disabled','--text','x',ok=False)['error_code']=='unsupported'
  assert not (state/f"mailbox-{pr['id']}.json").exists()
 case('exact roles exclude other project and disabled destination',roles)
 sent=msg(a,'send','reviewer','--key','review-1','--text','timeout_seconds=37');h=sent['message']['handle']
 def no_uuid():
  assert h=='m1';assert 'binding' not in sent['message'];assert not sent['deduplicated'];assert sent['message']['retrieval'] is None
  assert 'timeout_seconds=37' not in tm('capture-pane','-p','-t',b[1]['pane']).stdout
  q=msg(a,'send','reviewer','--key','review-1','--text','timeout_seconds=37');assert q['deduplicated'];assert q['message']['handle']==h;assert len(stored()['messages'])==1
  q=msg(a,'send','reviewer','--key','review-1','--text','timeout_seconds=38',ok=False);assert q['error_code']=='idempotency_conflict'
 case('role send stored only, short handle, stable key dedup',no_uuid)
 def read_only():
  path=state/f"mailbox-{pr['id']}.json";before=path.read_bytes()
  v=msg(b,'inbox');assert v['messages'][0]['handle']==h;assert v['messages'][0]['text']=='timeout_seconds=37'
  status(a,h);status(b,h);assert before==path.read_bytes()
  assert msg(outsider,'status',h,ok=False)['error_code']=='not_found'
  assert msg(a,'consume',h,ok=False)['ok'] is False
 case('inbox/status read-only and handles exact project/caller-run scoped',read_only)
 def receipt():
  v=msg(b,'consume',h)['message'];stamp=v['retrieval']['recorded_at'];assert v['acknowledged_at'] is None and v['completion'] is None
  assert 'understanding unverified' in v['retrieval']['source'];assert msg(b,'consume',h)['message']['retrieval']['recorded_at']==stamp
  ack=msg(b,'ack',h)['message'];assert ack['acknowledged_at'];assert ack['completion'] is None and ack['verified_result'] is None
  assert msg(b,'ack',h)['message']['acknowledged_at']==ack['acknowledged_at'];assert not msg(b,'inbox','--unacked')['messages']
 case('consume and acknowledgment are distinct idempotent evidence',receipt)
 reply=msg(b,'reply',h,'--key','answer-1','--text','APPROVED timeout_seconds=37');rh=reply['message']['handle']
 def reply_checks():
  assert rh=='m2' and reply['message']['in_reply_to']==h
  assert msg(b,'reply',h,'--key','answer-1','--text','APPROVED timeout_seconds=37')['deduplicated']
  assert msg(a,'reply',h,'--key','wrong','--text','x',ok=False)['ok'] is False
  assert msg(b,'send','impl','--key','answer-1','--text','APPROVED timeout_seconds=37',ok=False)['error_code']=='idempotency_conflict'
  msg(a,'ack',rh);v=status(a,rh);assert v['retrieval'] is None and v['acknowledged_at'] is not None
 case('reply reverses exact run pair, keys span send/reply, ack need not consume',reply_checks)
 def completion():
  v=msg(b,'complete',h,'--summary','Reviewed positive timeout','--artifact','results/review.json')['message'];assert v['completion']['summary']=='Reviewed positive timeout';assert v['verified_result'] is None
  assert v['completion']['artifact_path']=='results/review.json';assert not (root/'results').exists()
  assert msg(b,'complete',h,'--summary','Reviewed positive timeout','--artifact','results/review.json')['message']['completion']==v['completion']
  assert msg(b,'complete',h,'--summary','different',ok=False)['error_code']=='idempotency_conflict'
  assert msg(a,'complete',rh,'--summary','result is not handoff',ok=False)['ok'] is False
  assert msg(a,'complete',h,'--summary','sender cannot complete',ok=False)['ok'] is False
  observed=cli('agent','inspect','--agent',b[0]['id']);assert observed['task']=='unknown' and observed['report'] is None
 case('handoff completion is immutable exact-recipient self-report, inert artifact, observed task unchanged',completion)
 def payload_limits():
  for key in ['','../bad','two words','x'*65]:assert not msg(a,'send','reviewer','--key',key,'--text','x',ok=False)['ok']
  for text in ['', 'x'*8193, '\x1b]52;bad\x07']:
   assert not msg(a,'send','reviewer','--key','bad-text','--text',text,ok=False)['ok']
  for ref in ['m01','m0','m-1','latest','m1;sh']:
   assert not msg(b,'consume',ref,ok=False)['ok']
  for path in ['/tmp/file','../file','./file','a/../file']:
   assert not msg(b,'complete',h,'--summary','x','--artifact',path,ok=False)['ok']
 case('bounded payload/key/handle and artifact path validation',payload_limits)
 def duplicated_role():
  dup=create(pr,'reviewer',False,False)
  v=msg(a,'peers');assert len([p for p in v['peers'] if p['role']=='reviewer'])==2;assert all(not p['unique'] for p in v['peers'] if p['role']=='reviewer')
  assert msg(a,'send','reviewer','--key','new-ambiguous','--text','x',ok=False)['error_code']=='ambiguous_role'
  assert msg(a,'send','reviewer','--key','review-1','--text','timeout_seconds=37')['deduplicated']
 case('disabled duplicate causes ambiguity on new send; repeat retains original binding',duplicated_role)
 def concurrency():
  # impl is unique; simultaneous identical keys persist only one message.
  with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
   replies=list(pool.map(lambda _:msg(b,'reply',h,'--key','concurrent','--text','one result'),range(4)))
  assert len({x['message']['handle'] for x in replies})==1;assert sum(not x['deduplicated'] for x in replies)==1
 case('concurrent identical key requests atomically deduplicate',concurrency)
 def generation():
  old=b[1]['id'];fresh=cli('agent','restart','--agent',b[0]['id'],'--expected-run',old)
  assert msg(a,'send','reviewer','--key','review-1','--text','timeout_seconds=37',ok=False)['error_code']=='stale_run'
  assert msg(b,'ack',h,ok=False)['error_code']=='stale_run'
  new=(b[0],fresh);assert not msg(new,'inbox')['messages'];assert msg(new,'status',h,ok=False)['error_code']=='not_found'
  assert status(a,h)['binding']['to_run_id']==old
  # Changing role text under the same key cannot rebind the request.
  assert msg(a,'send','impl','--key','review-1','--text','timeout_seconds=37',ok=False)['error_code']=='idempotency_conflict'
 case('restart fails closed without mailbox transfer or silent rebind',generation)
 print('ALL PASS',len(results),flush=True)
finally:
 tm('kill-server')
 out={'binary_sha256':hashlib.sha256(pathlib.Path(B).read_bytes()).hexdigest(),'fixture':'sleep processes; not actual model cooperation','state':str(state),'checks':results,'passed':len(results)==13}
 report=pathlib.Path(args.report);report.parent.mkdir(parents=True,exist_ok=True);report.write_text(json.dumps(out,indent=2)+'\n')
