#!/usr/bin/env python3
"""Beta-to-source acceptance on two new private profiles; supervisor owns every child."""
import argparse, hashlib, json, os, pathlib, signal, socket, subprocess, sys, tempfile, time, urllib.request, xml.etree.ElementTree as ET
parser=argparse.ArgumentParser();parser.add_argument('--before',required=True);parser.add_argument('--after',required=True);parser.add_argument('--prepare',required=True);parser.add_argument('--syncthing',required=True);parser.add_argument('--evidence',required=True);parser.add_argument('--pause-before',action='store_true');parser.add_argument('--receiver-first',action='store_true');parser.add_argument('--before-protocol',type=int,default=1);args=parser.parse_args()
base=pathlib.Path(args.evidence).resolve()
root=pathlib.Path(tempfile.mkdtemp(prefix='upgrade-',dir=base));root.rmdir()
assert len(os.fsencode(root/'second/sync/direct/control.sock'))<108,'fixture exceeds Unix socket path limit'
assert hashlib.sha256(pathlib.Path(args.syncthing).read_bytes()).hexdigest()=='d72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e'
children=[]
def start(command,env,label):
 out=root/(label+'.out');err=root/(label+'.err')
 process=subprocess.Popen(command,env=env,stdout=open(out,'w'),stderr=open(err,'w'),process_group=0)
 children.append(process);return process,out,err
def stop(process):
 if process.poll() is not None:return
 os.killpg(process.pid,signal.SIGTERM)
 try:process.wait(timeout=15)
 except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
def wait(label,ready,timeout=90):
 deadline=time.monotonic()+timeout
 while not ready():
  if time.monotonic()>=deadline:raise RuntimeError('timed out: '+label)
  time.sleep(.25)
def last_json(output):
 return json.loads(next(line for line in reversed(output.splitlines()) if line.startswith('{')))
class Profile:
 def __init__(self,row):
  self.row=row;self.path=pathlib.Path(row['profile']);home=self.path/'home';home.mkdir()
  self.env=dict(os.environ,HOME=str(home),OKILUM_STATE_DIR=str(self.path),XDG_CONFIG_HOME=str(home/'config'),XDG_STATE_HOME=str(home/'state'),XDG_DATA_HOME=str(home/'data'),XDG_CACHE_HOME=str(home/'cache'),GOMAXPROCS='2',STMONITORED='1')
  self.name=self.path.name;self.binary=args.before;self.vault=pathlib.Path(row['vault']);self.serve=None
  config=ET.parse(row['config']).getroot();self.rest='http://'+config.findtext('gui/address');self.key=config.findtext('gui/apikey')
  self.socket=self.path/'sync/direct/control.sock'
 def api(self,path,post=False):
  q=urllib.request.Request(self.rest+path,method='POST' if post else 'GET',headers={'X-API-Key':self.key})
  with urllib.request.urlopen(q,timeout=3) as response:
   data=response.read();return json.loads(data) if data else None
 def cli(self,*verbs):
  result=subprocess.run([self.binary,'sync',*map(str,verbs)],env=self.env,capture_output=True,text=True,timeout=180)
  if result.returncode:raise RuntimeError(f'{self.name} {verbs[0]} failed: {result.stderr} {result.stdout}')
  value=last_json(result.stdout)
  with open(root/'commands.jsonl','a') as log: log.write(json.dumps(dict(profile=self.name,command=verbs[0],result=value))+'\n')
  return value
 def serve_start(self):
  self.serve,_,_=start([self.binary,'sync','serve','--wait','600'],self.env,self.name+'-'+('before' if self.binary==args.before else 'after')+'-serve')
  def control_ready():
   if self.serve.poll() is not None:raise RuntimeError(self.name+' owner exited: '+(root/(self.name+'-'+('before' if self.binary==args.before else 'after')+'-serve.err')).read_text())
   try:
    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as probe:
     probe.settimeout(1);probe.connect(str(self.socket))
     protocol=args.before_protocol if self.binary==args.before else 4
     probe.sendall(json.dumps(dict(protocol=protocol,body='approval_delivered')).encode());probe.shutdown(socket.SHUT_WR)
     data=b''
     while chunk:=probe.recv(65536):
      data+=chunk
      if len(data)>1048576:raise RuntimeError('oversized readiness response')
     response=json.loads(data)
     return response.get('protocol')==protocol and 'Ok' in response.get('body',{})
   except (OSError,ValueError):return False
  wait(self.name+' authenticated control reply',control_ready)
 def daemon_start(self):
  process,_,_=start([args.syncthing,'serve','--no-browser','--no-restart','--no-upgrade','--home',str(pathlib.Path(self.row['config']).parent)],self.env,self.name+'-daemon')
  def ready():
   if process.poll() is not None:raise RuntimeError('daemon exited')
   try:return self.api('/rest/system/status')['myID']==self.row['id']
   except OSError:return False
  wait(self.name+' authenticated daemon',ready)
profiles=[]
try:
 env=dict(os.environ,OKILUM_QA_ROOT=str(root),OKILUM_SYNC_DIRECT_CLIENT=args.syncthing)
 subprocess.run([args.prepare,'--exact','prepare_isolated_native_profiles','--nocapture'],env=env,check=True,timeout=120)
 profiles=[Profile(row) for row in json.loads((root/'profiles.json').read_text())]
 first,second=profiles
 for profile in profiles:profile.daemon_start();profile.cli('select',profile.row['config'])
 (first.vault/'seed.md').write_bytes(b'# Sync history acceptance\n\nBefore upgrade.\n')
 first.cli('share',first.vault);first.serve_start()
 invitation,out,_=start([first.binary,'sync','invite','--approve-first','--name','Isolated desktop','--wait','120'],first.env,'invite')
 wait('old beta invitation',lambda:bool(out.read_text().splitlines()))
 url=out.read_text().splitlines()[0]
 second.cli('connect',url,'--name','Isolated laptop','--wait','120')
 assert invitation.wait(timeout=120)==0
 status=second.cli('status');peer=status['peers'][0]['id'];vault=status['catalog'][0]['id']
 second.cli('choose',peer,vault,second.vault,'--wait','120')
 before_peers=[[(p['id'],p['name']) for p in profile.cli('status')['peers']] for profile in profiles]
 def hashes(name,expected):
  for profile in profiles:profile.api('/rest/db/scan?folder='+vault,post=True)
  return all((profile.vault/name).exists() and (profile.vault/name).read_bytes()==expected for profile in profiles)
 def edits(label):
  for index,profile in enumerate(profiles):
   name=f'{label}-{index}.md';data=f'{label}: exact bytes from {profile.name}\n'.encode();(profile.vault/name).write_bytes(data)
   wait(name,lambda:hashes(name,data))
 edits('before-upgrade')
 stop(first.serve)
 if args.pause_before:
  print('BEFORE_READY '+json.dumps(dict(root=str(root),profiles=[p.row for p in profiles])),flush=True)
  if sys.stdin.readline().strip()!='upgrade':raise RuntimeError('before acceptance was not released')
 # Upgrade one peer first, retaining the other peer's current-beta owner.
 # This exercises mixed-version pairing and transport before upgrading both.
 early,later=(second,first) if args.receiver_first else (first,second)
 early.binary=args.after
 for profile in profiles:profile.serve_start()
 mixed_status=[profile.cli('status') for profile in profiles]
 assert [[(p['id'],p['name']) for p in status['peers']] for status in mixed_status]==before_peers,'pairing identities changed during staggered upgrade'
 edits('mixed-upgrade')
 mismatches=[]
 for profile,command_binary in [(early,args.before),(later,args.after)]:
  result=subprocess.run([command_binary,'sync','status'],env=profile.env,capture_output=True,text=True,timeout=30)
  if args.before_protocol != 4:
   assert result.returncode!=0 and 'restart' in result.stderr.lower(), 'mixed local control versions must ask for restart: '+result.stdout+result.stderr
   mismatches.append(dict(profile=profile.name,exit=result.returncode,restart_requested=True))
  else:
   assert result.returncode==0, 'unchanged control protocol must remain compatible: '+result.stdout+result.stderr
   assert [(p['id'],p['name']) for p in last_json(result.stdout)['peers']]==before_peers[profiles.index(profile)]
 stop(later.serve)
 later.binary=args.after;later.serve_start()
 after_status=[profile.cli('status') for profile in profiles]
 assert [[(p['id'],p['name']) for p in s['peers']] for s in after_status]==before_peers,'pairing identities changed'
 assert all(s['pending'] is None and s['invitation_open'] is False for s in after_status)
 for profile in profiles:
  config=profile.api('/rest/config/folders/'+vault)
  assert config['versioning']['type']=='staggered' and config['versioning']['params']['maxAge']=='2592000'
 edits('after-upgrade')
 before=(first.vault/'seed.md').read_bytes();(second.vault/'seed.md').write_bytes(b'# Sync history acceptance\n\nAfter upgrade.\n')
 wait('remote replace after upgrade',lambda:hashes('seed.md',b'# Sync history acceptance\n\nAfter upgrade.\n'))
 archives=[p for p in (first.vault/'.stversions').rglob('*') if p.is_file() and p.read_bytes()==before]
 assert archives,'remote preimage absent from managed history'
 for profile in profiles:stop(profile.serve)
 before_binary=pathlib.Path(args.before).parent/'usr/bin/okilum'
 receipt=dict(receiver_first=args.receiver_first,mixed_version_bidirectional=True,control_mismatches=mismatches,root=str(root),before_sha256=hashlib.sha256(before_binary.read_bytes()).hexdigest(),after_sha256=hashlib.sha256(pathlib.Path(args.after).read_bytes()).hexdigest(),pairing_preserved=True,bidirectional_before=True,bidirectional_after=True,retention_days=30,remote_history_preimage_preserved=True,archives=[str(p) for p in archives],profiles=[p.row for p in profiles])
 (base/'upgrade-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
 print('UPGRADE_READY '+json.dumps(receipt),flush=True)
 for line in sys.stdin:
  if line.strip()=='quit':break
finally:
 for process in reversed(children):stop(process)
 print('UPGRADE_CLEANUP complete',flush=True)
