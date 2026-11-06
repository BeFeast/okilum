"""Signed-beta fresh Mac UI + real isolated headless protocol peer; hosted VM only."""
import hashlib,json,os,pathlib,plistlib,queue,re,shutil,signal,socket,subprocess,sys,tempfile,threading,time,urllib.request,xml.etree.ElementTree as ET
assert sys.platform=='darwin' and os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('OKILUM_QA_DISPOSABLE_HOST')=='github-hosted-macos-15','disposable hosted Mac only'
config=json.loads(pathlib.Path(sys.argv[1]).read_text());driver=pathlib.Path(sys.argv[2]);peer_binary=pathlib.Path(sys.argv[3]);out=pathlib.Path(sys.argv[4]);out.mkdir(exist_ok=True)
root=pathlib.Path(tempfile.mkdtemp(prefix='okilum-beta-ui-')).resolve();app=pathlib.Path('/Applications/Okilum.app');account=pathlib.Path.home()/'Library/Application Support/Okilum';managed=account/'Sync';direct=account/'SyncDirect'
assert not app.exists() and not managed.exists() and not direct.exists(),'host already has Okilum; never replace existing state'
label=f'gui/{os.getuid()}/com.befeast.okilum.sync';assert subprocess.run(['launchctl','print',label],capture_output=True).returncode!=0,'host already has registered Sync'
started=time.monotonic();events=[];source_api=None;receiver_api=None;peer=None;daemon=None;app_pid=None;recorder=None;record_started=None;result=None;runtime_pids=[]
def event(name):
 events.append({'event':name,'seconds':round(time.monotonic()-started,3)});(out/'events.json').write_text(json.dumps(events,indent=2)+'\n');print(name,flush=True)
def run(*args,timeout=45,**kw):return subprocess.run([str(a) for a in args],check=True,text=True,capture_output=True,timeout=timeout,**kw)
def get(url):
 with urllib.request.urlopen(urllib.request.Request(url,headers={'User-Agent':'curl'}),timeout=90) as r:return r.read()
def sha(p):return hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()
def wait(name,fn,timeout=90):
 end=time.monotonic()+timeout
 while True:
  value=fn()
  if value:return value
  if time.monotonic()>end:raise RuntimeError('timeout: '+name)
  time.sleep(.5)
def screen(name):
 p=out/(name+'.png');run('/usr/sbin/screencapture','-x',p);return p
def ocr(name='current'):
 p=screen(name);return json.loads(run(driver,'ocrjson',p).stdout)
def click(text,timeout=30):
 def locate():return next((r for r in ocr() if r['text'].strip().casefold()==text.casefold()),None)
 row=wait('visible '+text,locate,timeout);run(driver,'activate',app_pid);run(driver,'click',row['x'],row['y']);event('clicked '+text);time.sleep(1)
def api_from(path):
 xml=ET.parse(path).getroot();address=xml.findtext('gui/address');key=xml.findtext('gui/apikey')
 def api(path,post=False):
  q=urllib.request.Request('http://'+address+path,method='POST' if post else 'GET',headers={'X-API-Key':key})
  with urllib.request.urlopen(q,timeout=5) as r:data=r.read();return json.loads(data) if data else None
 return api
peer_lines=queue.Queue()
def peer_reader():
 for line in peer.stdout:
  if line.startswith('PEER '):peer_lines.put(json.loads(line[5:]))
def peer_command(op,**values):
 peer.stdin.write(json.dumps(dict(op=op,**values))+'\n');peer.stdin.flush();reply=peer_lines.get(timeout=45)
 if 'error' in reply:raise RuntimeError('headless '+op+': '+reply['error'])
 return reply['ok']
def journal():return json.loads((direct/'direct.json').read_text())
def owned_runtime_pids():
 needle=str(managed/'runtime')
 return [int(line.split(None,1)[0]) for line in run('ps','-axo','pid=,command=').stdout.splitlines() if needle in line and 'syncthing' in line]
def stop_recording():
 global recorder
 if not recorder:return None
 try:recorder.stdin.write('q\n');recorder.stdin.flush();code=recorder.wait(timeout=20)
 except (BrokenPipeError,subprocess.TimeoutExpired):
  recorder.send_signal(signal.SIGINT)
  try:code=recorder.wait(timeout=10)
  except subprocess.TimeoutExpired:recorder.kill();code=recorder.wait()
 recorder=None;return code
try:
 release=json.loads(get('https://updates.befeast.com/okilum/releases/'+config['source']+'/macos.json'));assert release['source']==config['source'] and release['build']==config['build']
 asset=next(a for a in release['assets'] if a['name']=='Okilum-macos.zip');archive=root/'beta.zip';archive.write_bytes(get('https://updates.befeast.com/'+asset['key']));assert sha(archive)==asset['sha256'];(out/'release.json').write_text(json.dumps(release,indent=2)+'\n')
 expanded=root/'expanded';run('ditto','-x','-k',archive,expanded,timeout=120);bundles=list(expanded.rglob('Okilum.app'));assert len(bundles)==1,bundles
 run('sudo','ditto',bundles[0],app,timeout=120)
 plist=plistlib.loads((app/'Contents/Info.plist').read_bytes());assert plist['OkilumSourceCommit']==config['source'];assert int(plist['CFBundleVersion'])==config['build']
 run('codesign','--verify','--deep','--strict',app);assessment=run('spctl','--assess','--type','execute','--verbose=2',app,timeout=120);signature=run('codesign','-dv','--verbose=4',app).stderr;assert 'Authority=Developer ID Application:' in signature
 runtime=app/'Contents/MacOS/syncthing';run('codesign','--verify','--strict',runtime);assert 'syncthing v2.1.6' in run(runtime,'--version').stdout
 info=run(app/'Contents/MacOS/okilum','--build-info').stdout;assert config['source'] in info and 'gpui-test-support: no' in info;(out/'build-info.txt').write_text(info);(out/'signature-summary.txt').write_text(signature+assessment.stdout+assessment.stderr)
 source=root/'ordinary-vault';source.mkdir();seed=b'# Fresh signed-beta Mac\n\nOriginal seed bytes.\n';(source/'seed.md').write_bytes(seed);profile=root/'reader-state';profile.mkdir();(profile/'features.json').write_text('{"sync":true}');(profile/'reader-ui.json').write_text('{"version":1,"settings_section":"sync"}')
 app_pid=int(run(driver,'launch',app,source,profile).stdout.strip().splitlines()[-1]);wait('Reader window',lambda:json.loads(run(driver,'list',app_pid).stdout));run(driver,'settings',app_pid);wait('Settings window',lambda:any('Settings' in w.get('kCGWindowName','') for w in json.loads(run(driver,'list',app_pid).stdout)));run(driver,'resize',app_pid);time.sleep(2);screen('01-fresh-settings');assert not managed.exists(),'opening Settings bootstrapped runtime';event('fresh signed-beta Settings opened without runtime')
 devices=subprocess.run(['ffmpeg','-hide_banner','-f','avfoundation','-list_devices','true','-i',''],capture_output=True,text=True,timeout=20);match=re.search(r'\[(\d+)\] Capture screen 0',devices.stderr);assert match,devices.stderr
 recorder=subprocess.Popen(['ffmpeg','-hide_banner','-loglevel','warning','-f','avfoundation','-framerate','10','-capture_cursor','1','-i',match[1]+':none','-an','-vf','scale=1024:-2','-c:v','libx264','-preset','ultrafast','-crf','24','-pix_fmt','yuv420p','-movflags','+frag_keyframe+empty_moov',str(out/'raw-pairing.mp4')],stdin=subprocess.PIPE,stdout=open(out/'recorder.log','w'),stderr=subprocess.STDOUT,text=True,start_new_session=True);record_started=time.monotonic();time.sleep(2);assert recorder.poll() is None,'recorder exited'
 click('Share with my computers')
 def shared():
  try:return next((s for s in journal().get('sources',[]) if s.get('path')==str(source) and s.get('active')),None)
  except (OSError,ValueError):return None
 owned=wait('UI-created active managed source',shared,120);source_api=api_from(managed/'config/config.xml');source_id=source_api('/rest/system/status')['myID'];folder=owned['id'];assert owned['daemon']['device_id']==source_id
 sf=source_api('/rest/config/folders/'+folder);assert sf['versioning']['type']=='staggered' and sf['versioning']['params']['maxAge']=='2592000';screen('02-share-ready');event('explicit UI Share prepared signed claimed runtime and active vault')
 click('Add computer');click('Create pairing link');click('Copy link');url=run('pbpaste').stdout.strip();assert len(url)>80
 screen('03-created-invitation');event('UI invitation copied')
 # Prepare an independent real Syncthing/Controller receiver only after the Mac's fresh bootstrap.
 peer_home=root/'peer-runtime';peer_state=root/'peer-controller';target=root/'peer-vault';target.mkdir();peer_state.mkdir(mode=0o700)
 run(runtime,'generate','--home',peer_home,timeout=45)
 ports=[]
 for _ in range(2):
  s=socket.socket();s.bind(('127.0.0.1',0));ports.append(s)
 rest,listen=[s.getsockname()[1] for s in ports];xml=ET.parse(peer_home/'config.xml');rx=xml.getroot()
 for f in list(rx.findall('folder')):rx.remove(f)
 rx.find('gui/address').text='127.0.0.1:'+str(rest);options=rx.find('options')
 for k,v in {'globalAnnounceEnabled':'false','localAnnounceEnabled':'false','relaysEnabled':'false','natEnabled':'false','urAccepted':'-1','autoUpgradeIntervalH':'0','startBrowser':'false'}.items():
  e=options.find(k)
  if e is None:e=ET.SubElement(options,k)
  e.text=v
 for e in list(options.findall('listenAddress')):options.remove(e)
 ET.SubElement(options,'listenAddress').text='tcp://127.0.0.1:'+str(listen);xml.write(peer_home/'config.xml')
 for s in ports:s.close()
 daemon=subprocess.Popen([str(runtime),'serve','--no-browser','--no-restart','--no-upgrade','--home',str(peer_home)],stdout=open(root/'peer-daemon.log','w'),stderr=subprocess.STDOUT,start_new_session=True,env=dict(os.environ,GOMAXPROCS='2',STMONITORED='1'));receiver_api=api_from(peer_home/'config.xml')
 def peer_ready():
  try:return receiver_api('/rest/system/status')['myID']
  except OSError:return None
 peer_id=wait('independent peer daemon',peer_ready)
 env=dict(os.environ,OKILUM_QA_PEER_STATE=str(peer_state),OKILUM_QA_PEER_CONFIG=str(peer_home/'config.xml'),OKILUM_STATE_DIR=str(peer_state))
 peer=subprocess.Popen([str(peer_binary),'--exact','native_peer','--nocapture'],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(root/'peer-controller.err','w'),text=True,start_new_session=True);threading.Thread(target=peer_reader,daemon=True).start();assert peer_lines.get(timeout=30).get('ready')
 peer_command('redeem',url=url)
 source_status=run(app/'Contents/MacOS/okilum','sync','status',env=dict(os.environ,OKILUM_STATE_DIR=str(profile))).stdout
 pending=json.loads(next(line for line in reversed(source_status.splitlines()) if line.startswith('{')))['pending']
 assert pending and pending['device_id']==peer_id and pending['name']=='Disposable headless peer','UI approval must bind the independently verified daemon identity'
 screen('04-before-approve');click('Approve');event('Mac UI approved independent peer')
 snap=wait('receiver learns approval',lambda:(v if (v:=peer_command('refresh')).get('peers') else None));pair=snap['peers'][0]['id'];catalog=peer_command('catalog',peer=pair);assert len(catalog)==1 and catalog[0]['id']==folder;peer_command('attach',peer=pair,vault=folder,path=str(target))
 def tick():
  source_api('/rest/db/scan?folder='+folder,True);receiver_api('/rest/db/scan?folder='+folder,True);return peer_command('refresh')
 wait('seed bytes and safe first-sync promotion',lambda:(not tick()['preparing'] and (target/'seed.md').exists() and (target/'seed.md').read_bytes()==seed),120)
 (source/'from-mac.md').write_bytes(b'Mac to independent peer, exact bytes.\n');(target/'from-peer.md').write_bytes(b'Independent peer back to Mac, exact bytes.\n')
 def both_ways():
  tick();return (target/'from-mac.md').exists() and (source/'from-peer.md').exists() and (target/'from-mac.md').read_bytes()==(source/'from-mac.md').read_bytes() and (source/'from-peer.md').read_bytes()==(target/'from-peer.md').read_bytes()
 wait('both directions exact',both_ways,120);event('both-direction file hashes verified')
 # Common policy positive/negative controls stay entirely inside the disposable vault.
 (source/'worktrees').mkdir();(source/'worktrees/ignored.md').write_bytes(b'not exported');(source/'.okilum-create-staged.md').write_bytes(b'not exported');(source/'positive.md').write_bytes(b'ordinary allowed note')
 wait('ordinary positive control after ignored files',lambda:(tick() is not None and (target/'positive.md').exists()),60);assert not (target/'worktrees/ignored.md').exists() and not (target/'.okilum-create-staged.md').exists()
 assert (source/'.okilum/sync-ignore').read_bytes()==(target/'.okilum/sync-ignore').read_bytes();assert (source/'.stignore').read_bytes().startswith(b'#include .okilum/sync-ignore\n')
 # Close the pairing wizard through native keyboard input, then show the final Sync page.
 run(driver,'escape');time.sleep(.5)
 if not any('Settings' in w.get('kCGWindowName','') for w in json.loads(run(driver,'list',app_pid).stdout)):run(driver,'settings',app_pid)
 run(driver,'resize',app_pid);time.sleep(3);screen('05-paired-bidirectional');event('final native Sync status captured')
 while time.monotonic()-record_started<65:time.sleep(.5)
 recorder_code=stop_recording();raw=out/'raw-pairing.mp4';probe=json.loads(run('ffprobe','-v','error','-show_streams','-show_format','-of','json',raw).stdout);duration=float(probe['format']['duration']);video=out/'fresh-Mac-beta-pairing.mp4'
 edited=False
 if duration>175:
  # Preserve chronological beginning, pairing/approval and final successful transport.
  cut1=min(30,duration);cut2=max(cut1,duration-140)
  select=f'between(t,0,{cut1})+between(t,{cut2},{duration})'
  run('ffmpeg','-v','error','-i',raw,'-vf',f"select='{select}',setpts=N/(10*TB)",'-an','-c:v','libx264','-crf','24','-pix_fmt','yuv420p',video,timeout=120);edited=True
 else:shutil.copy2(raw,video)
 run('ffmpeg','-v','error','-xerror','-i',video,'-f','null','-',timeout=120);vp=json.loads(run('ffprobe','-v','error','-show_streams','-show_format','-of','json',video).stdout);assert 60<=float(vp['format']['duration'])<=180
 hashes={name:sha(source/name) for name in ['seed.md','from-mac.md','from-peer.md','positive.md']};assert all(sha(target/name)==digest for name,digest in hashes.items())
 result={'build':config['build'],'source':config['source'],'package_sha256':asset['sha256'],'signed_notarized_beta':True,'fresh_mac_account_state':True,'settings_did_not_bootstrap':True,'source_enrolled_via_actual_UI':True,'invitation_created_and_copied_via_UI':True,'approved_via_Mac_UI':True,'receiver':'separate real Syncthing2.1.6 plus production Controller library, headless fixture','both_directions_exact':True,'file_hashes':hashes,'source_device':source_id,'peer_device':peer_id,'common_policy_equal':True,'mandatory_excludes_enforced':True,'managed_retention_days':30,'video_duration':float(vp['format']['duration']),'video_decode_exit':0,'video_edited_chronological':edited,'recorder_exit':recorder_code}
 event('SIGNED_BETA_FRESH_MAC_PAIRING_PASS')
except BaseException as error:
 event('failure: '+str(error))
 try:screen('failure')
 except Exception:pass
 (out/'failure.txt').write_text(str(error)+'\n');raise
finally:
 stop_recording()
 cleanup={}
 if peer:
  try:peer.stdin.close();peer.wait(timeout=15)
  except (BrokenPipeError,subprocess.TimeoutExpired):os.killpg(peer.pid,signal.SIGKILL);peer.wait()
  cleanup['headless_controller_reaped']=peer.poll() is not None
 if daemon:
  os.killpg(daemon.pid,signal.SIGTERM)
  try:daemon.wait(timeout=15)
  except subprocess.TimeoutExpired:os.killpg(daemon.pid,signal.SIGKILL);daemon.wait()
  cleanup['headless_daemon_reaped']=daemon.poll() is not None
 if app_pid:
  try:os.kill(app_pid,signal.SIGTERM)
  except ProcessLookupError:pass
  for _ in range(40):
   if not json.loads(run(driver,'list',app_pid).stdout):break
   time.sleep(.25)
  cleanup['app_windows_closed']=not json.loads(run(driver,'list',app_pid).stdout)
  exists=subprocess.run(['ps','-p',str(app_pid)],capture_output=True).returncode==0
  if exists:
   try:os.kill(app_pid,signal.SIGKILL)
   except ProcessLookupError:pass
   time.sleep(.5)
  cleanup['app_pid_gone']=subprocess.run(['ps','-p',str(app_pid)],capture_output=True).returncode!=0
 registration=subprocess.run(['launchctl','print',label],capture_output=True,text=True)
 if registration.returncode==0:
  assert str(app) in registration.stdout,'refuse cleanup of a different service'
  runtime_pids=owned_runtime_pids();subprocess.run(['launchctl','bootout',label],check=True,capture_output=True)
  for pid in runtime_pids:
   try:os.kill(pid,signal.SIGTERM)
   except ProcessLookupError:pass
  time.sleep(1);cleanup['managed_registration_removed']=subprocess.run(['launchctl','print',label],capture_output=True).returncode!=0;cleanup['managed_runtime_processes_gone']=not owned_runtime_pids()
 (out/'cleanup.json').write_text(json.dumps(cleanup,indent=2)+'\n')
 if result:
  result['cleanup']=cleanup;(out/'acceptance-receipt.json').write_text(json.dumps(result,indent=2)+'\n')
  assert all(cleanup.values()),cleanup
 print('MAC_BETA_FIXTURE_CLEANUP '+json.dumps(cleanup),flush=True)
