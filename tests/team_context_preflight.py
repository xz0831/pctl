#!/usr/bin/env python3
"""No tmux required: actor CLI context rejection must precede all state mutation."""
import argparse,json,os,pathlib,subprocess,tempfile
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--report',default='test-results/team-context-preflight.json');q=p.parse_args()
B=str(pathlib.Path(q.bin).resolve());T=str(pathlib.Path(q.tmux).resolve());root=pathlib.Path(tempfile.mkdtemp(prefix='team-ctx-',dir=os.path.realpath('/tmp')));state=root/'s';plain={k:v for k,v in os.environ.items() if not k.startswith('PCTL_')}
def call(s,args,env):return subprocess.run([B,'--state',str(s),'--tmux',T,*args],env=env,text=True,capture_output=True,timeout=8)
assert call(state,['project','add','--name','test','--root',str(root)],plain).returncode==0
state.chmod(0o755);original={p.name:p.read_bytes() for p in state.iterdir() if p.is_file()}
ctx={**plain,'PCTL_STATE':str(state),'PCTL_TMUX':T,'PCTL_AGENT_ID':'a','PCTL_RUN_ID':'r','PCTL_PROJECT_ID':'p'};tests=[]
for name,selected,cmd,env in [
 ('wrong new state actor',root/'wrong-actor',['team','children'],ctx),
 ('wrong new state operator in launcher context',root/'wrong-operator',['team','policy'],ctx),
 ('global theme actor',state,['--theme','mono','team','children'],ctx),
 ('global theme operator in launcher context',state,['--theme','mono','team','policy'],ctx),
 *[(f'missing {key}',state,['team','children'],{k:v for k,v in ctx.items() if k!=key}) for key in ['PCTL_STATE','PCTL_TMUX','PCTL_AGENT_ID','PCTL_RUN_ID','PCTL_PROJECT_ID']],
]:
 result=call(selected,cmd,env);assert result.returncode!=0,(name,result.stdout)
 if selected!=state:assert not selected.exists(),name
 assert {p.name:p.read_bytes() for p in state.iterdir() if p.is_file()}==original,name
 assert state.stat().st_mode&0o777==0o755,name
 tests.append({'test':name,'passed':True,'error':json.loads(result.stderr)['error']});print('PASS',name)
out=pathlib.Path(q.report);out.parent.mkdir(exist_ok=True,parents=True);out.write_text(json.dumps({'passed':True,'tests':tests,'scope':'preflight rejection and zero state content/mode changes; no native tmux required'},indent=2)+'\n')
