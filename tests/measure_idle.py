#!/usr/bin/env python3
"""Bounded Linux idle process measurement on a fresh, isolated synthetic demo."""
import argparse, hashlib, json, os, pathlib, platform, shutil, statistics, subprocess, tempfile, time
os.environ['PCTL_STANDALONE'] = '1'  # Only disposable test servers start themselves.
p=argparse.ArgumentParser();p.add_argument('--bin',required=True);p.add_argument('--tmux',required=True);p.add_argument('--seconds',type=int,default=60);p.add_argument('--report',default='test-results/idle-metrics.json');a=p.parse_args();B=str(pathlib.Path(a.bin).resolve());T=str(pathlib.Path(a.tmux).resolve());state=pathlib.Path(tempfile.mkdtemp(prefix='pctl-idle-',dir=os.path.realpath('/tmp')));hz=os.sysconf('SC_CLK_TCK')
def cli(*args):return json.loads(subprocess.check_output([B,'--state',str(state),'--tmux',T,*args],text=True))
def tm(*args):return subprocess.check_output([T,'-S',str(state/'tmux.sock'),*args],text=True).strip()
def sample(pid):
 stat=pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split();ticks=int(stat[11])+int(stat[12]);child_ticks=int(stat[13])+int(stat[14]);rss=0
 for line in pathlib.Path(f'/proc/{pid}/status').read_text().splitlines():
  if line.startswith('VmRSS:'):rss=int(line.split()[1])
 return ticks,rss,child_ticks
try:
 demo=cli('demo');roles={r['pid']:'synthetic_worker' for r in demo['runs']}
 for line in tm('list-panes','-a','-F','#{pane_pid}\t#{@pctl_run}').splitlines():
  pid,*run=line.split('\t')
  if pid not in roles:roles[pid]='manager_ui'
 server=tm('display-message','-p','-t',demo['runs'][0]['pane'],'#{pid}');roles[server]='tmux_server'
 time.sleep(2);initial={pid:sample(pid) for pid in roles};start=time.monotonic();samples={pid:[] for pid in roles};deadline=start+a.seconds
 while time.monotonic()<deadline:
  for pid in roles:samples[pid].append(sample(pid)[1])
  time.sleep(min(1,max(0,deadline-time.monotonic())))
 elapsed=time.monotonic()-start;rows=[]
 for pid,role in roles.items():
  ticks,rss,child_ticks=sample(pid);rows.append({'role':role,'pid':int(pid),'cpu_seconds':round((ticks-initial[pid][0])/hz,4),'cpu_percent_one_core':round(100*(ticks-initial[pid][0])/hz/elapsed,4),'reaped_children_cpu_seconds':round((child_ticks-initial[pid][2])/hz,4),'reaped_children_cpu_percent_one_core':round(100*(child_ticks-initial[pid][2])/hz/elapsed,4),'self_plus_reaped_cpu_percent_one_core':round(100*((ticks-initial[pid][0])+(child_ticks-initial[pid][2]))/hz/elapsed,4),'rss_kib_median':statistics.median(samples[pid]),'rss_kib_peak':max(samples[pid]),'samples':len(samples[pid])})
 result={'scope':'cloud-only detached idle synthetic 2-project x 2-worker demo; two manager UIs; no attached terminal client','build_sha256':hashlib.sha256(pathlib.Path(B).read_bytes()).hexdigest(),'binary':B,'pctl':subprocess.check_output([B,'--version'],text=True).strip(),'tmux':subprocess.check_output([T,'-V'],text=True).strip(),'platform':platform.platform(),'libc':platform.libc_ver(),'duration_seconds':round(elapsed,4),'clock_ticks_per_second':hz,'cpu_semantics':'100% = one logical CPU. Self fields use /proc utime+stime; reaped-child fields use cutime+cstime (waited-for child processes, including short-lived tmux queries). Self-plus-reaped is per process tree accounting, not a sum to add blindly across sampled roles; live/unreaped child CPU is not included in its parent.','rss_semantics':'VmRSS sampled once per second, KiB; not PSS and cannot be summed as unique memory','processes':rows,'limits':['No baseline tmux-only comparison','No active terminal/compositor or target desktop measurement','No event-to-draw latency instrumentation','No output burst/scale measurement','Not RHEL compatibility or performance certification']}
 out=pathlib.Path(a.report);out.parent.mkdir(parents=True,exist_ok=True);out.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2),flush=True)
finally:
 subprocess.run([T,'-S',str(state/'tmux.sock'),'kill-server'],capture_output=True)
 shutil.rmtree(state)
