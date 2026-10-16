#!/usr/bin/env python3
"""Isolated real-tmux mailbox regression; never sends text to agent terminals."""
import argparse, concurrent.futures, json, os, pathlib, subprocess, tempfile, time, uuid
os.environ['HERDR_ENV'] = '1'  # Only disposable test servers bypass the Herdr startup gate.
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',default='test-results/v03-collaboration-integration.json');args=p.parse_args()
B=str(pathlib.Path(args.bin).resolve());T=str(pathlib.Path(args.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='pctl-msg-',dir=os.path.realpath('/tmp')));state=root/'s';results=[]
def cli(*a,ok=True):
 q=subprocess.run([B,'--state',str(state),'--tmux',T,*map(str,a)],text=True,capture_output=True,timeout=20)
 if ok: assert q.returncode==0,(a,q.stderr,q.stdout)
 else: assert q.returncode!=0,(a,q.stdout)
 return json.loads(q.stdout if ok else q.stderr)
def tm(*a):
 return subprocess.run([T,'-S',str(state/'tmux.sock'),*map(str,a)],text=True,capture_output=True,timeout=10).stdout
def case(name,fn):
 started=time.monotonic();fn();results.append({'test':name,'passed':True,'seconds':round(time.monotonic()-started,4)});print('PASS',name,flush=True)
def create(project,name,mode='mailbox'):
 envfile=root/f'{name}-env.json';script=f'python3 -c \'import os,json;json.dump({{k:v for k,v in os.environ.items() if k.startswith("PCTL_")}},open("{envfile}","w"))\'; exec /bin/sleep 300'
 a=cli('agent','add','--project',project['id'],'--name',name,'--cwd',root,'--','/bin/sh','-c',script)
 if mode=='mailbox':cli('agent','configure-collaboration','--agent',a['id'],'--mode','mailbox')
 r=cli('agent','start','--agent',a['id']);return a,r,envfile
def inbox(pair,**kw):
 a,r,*_=pair;flags=[]
 for k,v in kw.items():flags+=['--'+k.replace('_','-')]+([] if v is True else [str(v)])
 return cli('message','inbox','--agent',a['id'],'--expected-run',r['id'],*flags)
def send(src,dst,body='Synthetic test only.',request=None,reply=None,kind='handoff',ok=True):
 a,ra,*_=src;b,rb,*_=dst;rid=request or str(uuid.uuid4());f=root/f'{rid}.txt';f.write_text(body);flags=['--in-reply-to',reply] if reply else []
 return cli('message','send','--from-agent',a['id'],'--expected-run',ra['id'],'--to-agent',b['id'],'--to-run',rb['id'],'--kind',kind,'--subject','Synthetic mailbox test','--body-file',f,'--request-id',rid,*flags,ok=ok)
try:
 pr=cli('project','add','--name','same-project','--root',root);other=cli('project','add','--name','other-project','--root',root)
 a=create(pr,'sender');b=create(pr,'receiver');c=create(other,'outsider');d=create(pr,'disabled','disabled')
 def launch_context():
  for who in [a,b]:
   for _ in range(100):
    if who[2].exists():break
    time.sleep(.01)
   env=json.loads(who[2].read_text());assert env['PCTL_AGENT_ID']==who[0]['id'];assert env['PCTL_RUN_ID']==who[1]['id'];assert env['PCTL_PROJECT_ID']==pr['id'];assert env['PCTL_STATE']==str(state);assert env['PCTL_BIN']==B;assert env['PCTL_TMUX']==T
 case('launcher exports exact local agent/project/run and CLI context',launch_context)
 def peers():
  v=cli('peer','list','--agent',a[0]['id'],'--expected-run',a[1]['id']);assert {x['agent']['id'] for x in v['peers']}=={b[0]['id'],d[0]['id']};assert v['identity_trust'].startswith('caller_claimed')
 case('peer discovery excludes other projects and caller',peers)
 sent=send(a,b);mid=sent['message']['id']
 def no_terminal():
  v=inbox(b);assert len(v['messages'])==1;assert v['messages'][0]['acked_at'] is None;assert inbox(b)['messages'][0]['acked_at'] is None
  assert 'Synthetic test only.' not in tm('capture-pane','-p','-t',b[1]['pane'])
  assert inbox(a)['messages']==[]
 case('durable inbox reads are side-effect-free and never inject PTY input',no_terminal)
 def durable():
  stored=json.loads((state/f"mailbox-{pr['id']}.json").read_text());assert stored['messages'][0]['id']==mid;assert inbox(b)['messages'][0]['id']==mid
 case('separate CLI process rereads atomically persisted message',durable)
 def idempotent():
  duplicate=send(a,b,request=mid);assert duplicate['deduplicated'];assert len(inbox(b)['messages'])==1
  err=send(a,b,body='different',request=mid,ok=False);assert err['error_code']=='idempotency_conflict';assert len(inbox(b)['messages'])==1
 case('identical requests deduplicate and conflicting reuse is rejected',idempotent)
 def acknowledge():
  bad=cli('message','ack','--agent',a[0]['id'],'--expected-run',a[1]['id'],'--message',mid,ok=False);assert 'recipient' in bad['error']
  ack=cli('message','ack','--agent',b[0]['id'],'--expected-run',b[1]['id'],'--message',mid);again=cli('message','ack','--agent',b[0]['id'],'--expected-run',b[1]['id'],'--message',mid)
  assert ack['message']['acked_at']==again['message']['acked_at'];assert not inbox(b,unacked=True)['messages'];assert len(inbox(b)['messages'])==1
 case('ack is recipient-bound, idempotent receipt with retained history',acknowledge)
 def reply():
  answer=send(b,a,reply=mid,kind='result');assert answer['message']['correlation_id']==mid;assert answer['message']['in_reply_to']==mid
  wrong=send(a,b,reply=mid,ok=False);assert 'reverse' in wrong['error']
 case('reply correlates and requires exact reverse run pair',reply)
 def ack_reply():
  first=cli('message','ack-reply','--agent',a[0]['id'],'--expected-run',a[1]['id'],'--reply-to',mid)
  second=cli('message','ack-reply','--agent',a[0]['id'],'--expected-run',a[1]['id'],'--reply-to',mid)
  assert first['message']['id']!=mid;assert first['message']['in_reply_to']==mid;assert first['message']['acked_at']==second['message']['acked_at']
 case('ack-reply resolves actual received reply and repeat keeps exact receipt',ack_reply)
 def ack_reply_wrong():
  err=cli('message','ack-reply','--agent',b[0]['id'],'--expected-run',b[1]['id'],'--reply-to',mid,ok=False);assert 'owner/run' in err['error']
  empty=send(a,b)['message']['id'];err=cli('message','ack-reply','--agent',a[0]['id'],'--expected-run',a[1]['id'],'--reply-to',empty,ok=False);assert 'not found' in err['error']
 case('ack-reply rejects wrong owner and never acknowledges a future reply',ack_reply_wrong)
 def ack_reply_ambiguous():
  later=send(b,a,reply=mid,kind='result')['message']['id'];err=cli('message','ack-reply','--agent',a[0]['id'],'--expected-run',a[1]['id'],'--reply-to',mid,ok=False);assert err['error_code']=='ambiguous_reply';assert next(m for m in inbox(a)['messages'] if m['id']==later)['acked_at'] is None
 case('ack-reply refuses multiple replies including an already-acked predecessor',ack_reply_ambiguous)

 def boundary():
  assert send(a,c,ok=False)['error_code']=='cross_project';assert 'disabled' in send(a,d,ok=False)['error'];assert 'disabled' in send(d,a,ok=False)['error']
 case('cross-project and either-side disabled delivery rejected',boundary)
 def bounded_body():
  for text in ['x'*8193,'\x1b]11;rgb:1234\x07','', 'x\r y']:
   assert send(a,b,body=text,ok=False)['ok'] is False
 case('empty oversized and terminal-control message bodies rejected',bounded_body)
 def pagination():
  for i in range(3):send(a,b,body=f'page {i}')
  first=inbox(b,limit=2);assert len(first['messages'])==2;assert first['next_after_sequence'] is not None
  second=inbox(b,limit=2,after_sequence=first['next_after_sequence']);assert len(second['messages'])==2;assert not set(m['id'] for m in first['messages'])&set(m['id'] for m in second['messages'])
 case('bounded pagination is ordered and nonoverlapping',pagination)
 def status():
  report=cli('agent','report','--agent',b[0]['id'],'--expected-run',b[1]['id'],'--status','completed','--summary','Synthetic claim, still requires verification','--ttl-seconds','1')
  v=cli('agent','inspect','--agent',b[0]['id']);assert v['task']=='unknown';assert v['report']['fresh'];assert v['report']['report']['status']=='completed'
  time.sleep(1.1);v=cli('agent','inspect','--agent',b[0]['id']);assert not v['report']['fresh'];assert v['task']=='unknown'
 case('self-reported completed never changes observed task and expires',status)
 def wait():
  watermark=inbox(b)['high_watermark']
  with concurrent.futures.ThreadPoolExecutor() as pool:
   future=pool.submit(inbox,b,after_sequence=watermark,wait_seconds=5)
   time.sleep(.35);begin=time.monotonic();m=send(a,b);latency=time.monotonic()-begin;assert latency<2
   v=future.result();assert v['messages'][0]['id']==m['message']['id'];assert not v['timed_out'];assert v['waited_ms']>=300
  empty=inbox(b,after_sequence=v['high_watermark'],wait_seconds=1);assert empty['timed_out'];assert empty['messages']==[]
 case('bounded wait releases locks and receives concurrent sender without courier',wait)
 def concurrent_dedup():
  request=str(uuid.uuid4());f=root/f'{request}.txt';f.write_text('concurrent')
  def one():return cli('message','send','--from-agent',a[0]['id'],'--expected-run',a[1]['id'],'--to-agent',b[0]['id'],'--to-run',b[1]['id'],'--kind','update','--subject','parallel same request','--body-file',f,'--request-id',request)
  with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:all_results=list(pool.map(lambda _:one(),range(4)))
  assert sum(not r['deduplicated'] for r in all_results)==1;assert sum(m['id']==request for m in inbox(b)['messages'])==1
 case('concurrent exact retries produce one durable message',concurrent_dedup)
 def stopped_guard():
  assert 'current run' in cli('agent','configure-collaboration','--agent',a[0]['id'],'--mode','disabled',ok=False)['error']
 case('capability cannot change on current run',stopped_guard)
 def skill():
  result=cli('skill','install','--project',pr['id']);assert pathlib.Path(result['skill_path']).read_text().startswith('---\nname: pctl-prototype');assert result['skill_path'].endswith('.opencode/skills/pctl-prototype/SKILL.md')
  cli('skill','install','--project',pr['id']);assert pathlib.Path(result['runtime_path']).is_file()
 case('project-only native skill install is explicit and idempotent',skill)
 def restart():
  before=inbox(b);new=cli('agent','restart','--agent',b[0]['id'],'--expected-run',b[1]['id']);assert new['id']!=b[1]['id']
  assert send(a,b,ok=False)['error_code']=='stale_run'
  assert cli('message','ack','--agent',b[0]['id'],'--expected-run',b[1]['id'],'--message',mid,ok=False)['error_code']=='stale_run'
  latest=(b[0],new,b[2]);assert not inbox(latest)['messages'];assert cli('agent','inspect','--agent',b[0]['id'])['report'] is None
  stored=json.loads((state/f"mailbox-{pr['id']}.json").read_text());assert len(stored['messages'])>=len(before['messages'])
 case('restart rejects stale delivery/ack and never inherits inbox or status',restart)
 report={'passed':True,'tests':results,'binary':B,'scope':'isolated real tmux and generic sleep fixtures; no model claims; terminal scraping only asserts absence of delivery','temporary_root':str(root)}
 pathlib.Path(args.report).parent.mkdir(parents=True,exist_ok=True);pathlib.Path(args.report).write_text(json.dumps(report,indent=2)+'\n')
finally:
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
