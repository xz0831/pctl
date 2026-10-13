#!/usr/bin/env python3
"""Exercise the real Crossterm parser + manager guard through a disposable PTY.
No tmux server, model, user terminal or production profile is touched.
"""
import argparse,fcntl,json,os,pathlib,pty,select,subprocess,termios,time
p=argparse.ArgumentParser();p.add_argument('--probe',required=True);p.add_argument('--report',default='test-results/input-guard-pty.json');a=p.parse_args()
probe=str(pathlib.Path(a.probe).resolve())
osc=b'11;rgb:8787/8787/8787'
cases=[
 ('BEL complete with prefix and suffix',[(0,b'a\x1b]'+osc+b'\x07z')],0,['Char(\'a\')','Char(\'z\')'],[],[]),
 ('OSC4 palette102 exact incident response',[(0,b'a\x1b]4;102;rgb:8787/8787/8787\x07z')],0,['Char(\'a\')','Char(\'z\')'],[],[]),
 ('ST complete with prefix and suffix',[(0,b'a\x1b]'+osc+b'\x1b\\z')],0,['Char(\'a\')','Char(\'z\')'],[],[]),
 ('fragmented introducer and ST',[(0,b'a\x1b'),(.05,b']'+osc),(.05,b'\x1b'),(.05,b'\\z')],0,['Char(\'a\')','Char(\'z\')'],['code: Esc'],[]),
 ('every-byte fragmented BEL',[(.01,bytes([b])) for b in b'a\x1b]'+osc+b'\x07z'],0,['Char(\'a\')','Char(\'z\')'],[],[]),
 ('Unicode and SGR mouse after response',[(0,b'\x1b]'+osc+b'\x07'+ '한'.encode()+b'\x1b[<0;12;5M')],0,['Char(\'한\')','Down(Left)','column: 11','row: 4'],[],[]),
 ('standalone Escape and following key',[(0,b'\x1b'),(.05,b'a')],0,['code: Esc','Char(\'a\')'],[],[]),
 ('bracketed paste remains literal',[(0,b'\x1b[200~literal rgb text\x1b[201~')],0,['Paste("literal rgb text")'],[],[]),
 ('late fragmented introducer fails closed',[(0,b'\x1b'),(.4,b']11;rgb:8787/8787/8787\x07y')],2,['GUARD_ERROR:ambiguous late'],['Char(\'y\')'],[]),
 ('unrelated close bracket remains literal',[(0,b'a]z')],0,['Char(\']\')','Char(\'z\')'],[],[]),
 ('standalone Escape deadline',[(0,b'\x1b')],0,['code: Esc'],[],[]),
 ('unterminated reply fails closed',[(0,b'\x1b]'+osc)],2,['GUARD_ERROR:unterminated'],[],[]),
 ('overlong reply fails closed',[(0,b'\x1b]11;')]+[(.01,b'r'*256)]*17+[(.01,b'\x07y')],2,['GUARD_ERROR:terminal control response exceeds'],['Char(\'y\')'],[]),
 ('UNSUPPORTED raw 8bit C1 introducer is lost upstream',[(0,b'\x9d11;rgb:8787/8787/8787\x07')],0,["Char('r')"],[],[]),
 ('cursor reply still recognized while OSC queued',[(0,b'\x1b]'+osc+b'\x07\x1b[12;34Rz')],0,['CURSOR:33,11','Char(\'z\')'],[],['--cursor-check']),
]
results=[]
for name,chunks,want_exit,required,forbidden,extra in cases:
 m,s=pty.openpty();before=termios.tcgetattr(s)
 child=subprocess.Popen([probe,*extra],stdin=s,stdout=s,stderr=s,env=dict(os.environ,TERM='xterm-256color'),preexec_fn=lambda:(os.setsid(),fcntl.ioctl(0,termios.TIOCSCTTY,0)))
 os.set_blocking(m,False);output=b'';ready=False;index=0;offset=0;next_send=0;deadline=time.monotonic()+6
 while time.monotonic()<deadline:
  if select.select([m],[],[],.005)[0]:
   try:output+=os.read(m,65536)
   except (OSError,BlockingIOError):pass
  if not ready and b'READY' in output:ready=True;next_send=time.monotonic()+chunks[0][0]
  if ready and index<len(chunks) and time.monotonic()>=next_send:
   try:offset+=os.write(m,chunks[index][1][offset:])
   except BlockingIOError:pass
   if offset==len(chunks[index][1]):
    index+=1;offset=0
    if index<len(chunks):next_send=time.monotonic()+chunks[index][0]
  if child.poll() is not None:break
 if child.poll() is None:child.kill()
 child.wait();after=termios.tcgetattr(s);os.close(s);os.close(m)
 text=output.decode(errors='replace')
 unsupported=name.startswith("UNSUPPORTED")
 if not unsupported:forbidden=forbidden+["Char('r')","Char('8')","Char('/')"]
 ok=child.returncode==want_exit and before==after and all(x in text for x in required) and not any(x in text for x in forbidden)
 results.append({'name':name,'supported_encoding':not unsupported,'passed':ok,'exit':child.returncode,'termios_restored':before==after,'required':required,'forbidden':forbidden,'output':text,'chunk_hex':[x.hex() for _,x in chunks]})
 print(('PASS' if ok else 'FAIL'),name,flush=True)
report={'suite':'actual Crossterm PTY parser and pctl input boundary','tests':results,'passed':sum(r['passed'] and r['supported_encoding'] for r in results),'unsupported_scope_checks':sum(r['passed'] and not r['supported_encoding'] for r in results),'failed':sum(not r['passed'] for r in results)}
path=pathlib.Path(a.report);path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(report,indent=2)+'\n')
raise SystemExit(0 if not report['failed'] else 1)
