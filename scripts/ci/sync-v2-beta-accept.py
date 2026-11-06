"""Signed-beta fresh Mac UI + real isolated headless protocol peer; hosted VM only."""
import hashlib,json,os,pathlib,platform,plistlib,queue,re,shutil,signal,socket,subprocess,sys,tempfile,threading,time,urllib.request,xml.etree.ElementTree as ET
assert sys.platform=='darwin' and os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('OKILUM_QA_DISPOSABLE_HOST') in ('github-hosted-macos-15','github-hosted-macos-26'),'disposable hosted Mac only'
config=json.loads(pathlib.Path(sys.argv[1]).read_text());driver=pathlib.Path(sys.argv[2]);peer_binary=pathlib.Path(sys.argv[3]);out=pathlib.Path(sys.argv[4]);out.mkdir(exist_ok=True)
root=pathlib.Path(tempfile.mkdtemp(prefix='okilum-beta-ui-')).resolve();app=pathlib.Path('/Applications/Okilum.app');account=pathlib.Path.home()/'Library/Application Support/Okilum';managed=account/'Sync';direct=account/'SyncDirect'
assert not app.exists() and not managed.exists() and not direct.exists(),'host already has Okilum; never replace existing state'
label=f'gui/{os.getuid()}/com.befeast.okilum.sync';assert subprocess.run(['launchctl','print',label],capture_output=True).returncode!=0,'host already has registered Sync'
budget_source=pathlib.Path('crates/okilum-sync-controller/src/sidecar/macos/connect.rs').read_text()
preparation_budget=int(re.search(r'pub const MAX_PREPARATION: Duration = Duration::from_secs\((\d+)\)',budget_source)[1])
startup_budget=int(re.search(r'pub const MAX_STARTUP: Duration = Duration::from_secs\((\d+)\)',budget_source)[1])
source_wait=preparation_budget+startup_budget+10
started=time.monotonic();events=[];source_api=None;receiver_api=None;peer=None;daemon=None;app_pid=None;recorder=None;record_started=None;result=None;runtime_pids=[];reserved_ports=[]
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
 def locate():
  allow_local_network_dialog()
  return next((r for r in ocr() if r['text'].strip().lstrip('›>▾ ').casefold()==text.casefold()),None)
 row=wait('visible '+text,locate,timeout);run(driver,'activate',app_pid);run(driver,'click',row['x'],row['y']);event('clicked '+text);time.sleep(1)
def allow_local_network_dialog():
 rows=ocr()
 text=' '.join(r['text'] for r in rows).casefold()
 if 'local network' not in text or not any(name in text for name in ['okilum','syncthing']):return False
 allow=next((r for r in rows if r['text'].strip().casefold()=='allow'),None)
 if not allow:return False
 screen('01b-local-network-consent')
 run(driver,'click',allow['x'],allow['y'])
 event('allowed named Okilum/Syncthing local-network OS consent on disposable Mac')
 time.sleep(.5);return True

def bindings(value):
 if isinstance(value,dict):
  if all(k in value for k in ['owner','state_directory','supervisor']):yield value
  for v in value.values():yield from bindings(v)
 elif isinstance(value,list):
  for v in value:yield from bindings(v)

def api_from(path):
 xml=ET.parse(path).getroot();address=xml.findtext('gui/address');key=xml.findtext('gui/apikey')
 def api(path,post=False,body=None):
  q=urllib.request.Request('http://'+address+path,method='POST' if post else 'GET',data=None if body is None else json.dumps(body).encode(),headers={'X-API-Key':key,'Content-Type':'application/json'})
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
last_observation=0
def observe_runtime(force=False):
 global last_observation
 if not force and time.monotonic()-last_observation<10:return
 last_observation=time.monotonic()
 row={'seconds':round(time.monotonic()-started,3)}
 try:
  saved=json.loads((managed/'sidecar.json').read_text())
  row['sidecar']={k:saved.get(k) for k in ['schema','revision','intent']}
  launch=saved.get('launch') or {}
  row['launch_phase']=launch.get('phase')
  row['pending_stop']=saved.get('stop') is not None
  row['pending_update']=saved.get('update') is not None
 except (OSError,ValueError):row['sidecar']='not present'
 row['runtime_pids']=owned_runtime_pids()
 try:
  rest=api_from(managed/'config/config.xml')
  status=rest('/rest/system/status')
  row['rest']={'ready':True,'uptime':status.get('uptime'),'version':rest('/rest/system/version').get('version')}
 except Exception as e:row['rest']={'ready':False,'error_type':type(e).__name__}
 with (out/'runtime-observations.jsonl').open('a') as f:f.write(json.dumps(row)+'\n')
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
 app_pid=int(run(driver,'launch',app,source,profile).stdout.strip().splitlines()[-1]);wait('Reader window',lambda:json.loads(run(driver,'list',app_pid).stdout))
 wait('rendered seed note before native Settings command',lambda:any('Fresh signed-beta Mac' in r['text'] for r in ocr()))
 next_settings_command=0
 def settings_open():
  global next_settings_command
  if any('Settings' in w.get('kCGWindowName','') for w in json.loads(run(driver,'list',app_pid).stdout)):return True
  if time.monotonic()>=next_settings_command:
   run(driver,'settings',app_pid);next_settings_command=time.monotonic()+5
  return False
 wait('Settings window after native command',settings_open);run(driver,'resize',app_pid);time.sleep(2);screen('01-fresh-settings');assert not managed.exists(),'opening Settings bootstrapped runtime';event('fresh signed-beta Settings opened without runtime')
 devices=subprocess.run(['ffmpeg','-hide_banner','-f','avfoundation','-list_devices','true','-i',''],capture_output=True,text=True,timeout=20);match=re.search(r'\[(\d+)\] Capture screen 0',devices.stderr);assert match,devices.stderr
 recorder=subprocess.Popen(['ffmpeg','-hide_banner','-loglevel','warning','-f','avfoundation','-framerate','10','-capture_cursor','1','-i',match[1]+':none','-an','-vf','scale=1024:-2','-c:v','libx264','-preset','ultrafast','-crf','24','-pix_fmt','yuv420p','-movflags','+frag_keyframe+empty_moov',str(out/'raw-pairing.mp4')],stdin=subprocess.PIPE,stdout=open(out/'recorder.log','w'),stderr=subprocess.STDOUT,text=True,start_new_session=True);record_started=time.monotonic();time.sleep(2);assert recorder.poll() is None,'recorder exited'
 # A normal existing source on the independent headless counterpart; all paths are disposable.
 peer_home=root/'peer-runtime';peer_state=root/'peer-controller';peer_vault=root/'source-vault';peer_vault.mkdir();(peer_vault/'seed.md').write_bytes(seed);peer_state.mkdir(mode=0o700);destination=pathlib.Path(tempfile.mkdtemp(prefix='received-vault-',dir=pathlib.Path.home()/'Documents'))
 run(runtime,'generate','--home',peer_home,timeout=45)
 ports=[]
 for _ in range(2):
  sock=socket.socket();sock.bind(('127.0.0.1',0));ports.append(sock)
 rest,listen=[sock.getsockname()[1] for sock in ports];xml=ET.parse(peer_home/'config.xml');rx=xml.getroot()
 for f in list(rx.findall('folder')):rx.remove(f)
 rx.find('gui/address').text='127.0.0.1:'+str(rest);options=rx.find('options')
 for key,value in {'globalAnnounceEnabled':'false','localAnnounceEnabled':'true','relaysEnabled':'false','natEnabled':'false','urAccepted':'-1','autoUpgradeIntervalH':'0','startBrowser':'false'}.items():
  element=options.find(key)
  if element is None:element=ET.SubElement(options,key)
  element.text=value
 for element in list(options.findall('listenAddress')):options.remove(element)
 ET.SubElement(options,'listenAddress').text='tcp://0.0.0.0:'+str(listen);xml.write(peer_home/'config.xml')
 for sock in ports:sock.close()
 receiver_api=api_from(peer_home/'config.xml')
 def start_counterpart():
  global daemon
  daemon=subprocess.Popen([str(runtime),'serve','--no-browser','--no-restart','--no-upgrade','--home',str(peer_home)],stdout=open(root/'peer-daemon.log','a'),stderr=subprocess.STDOUT,start_new_session=True,env=dict(os.environ,GOMAXPROCS='2',STMONITORED='1'))
  def ready():
   try:return receiver_api('/rest/system/status')['myID']
   except OSError:return None
  return wait('independent source daemon',ready)
 peer_id=start_counterpart();folder='disposable-source-vault';settings=receiver_api('/rest/config/defaults/folder');settings.update(id=folder,label='ordinary-vault',path=str(peer_vault),type='sendreceive',devices=[{'deviceID':peer_id}],rescanIntervalS=1,fsWatcherEnabled=False);receiver_api('/rest/config/folders',True,settings)
 wait('source marker',lambda:(peer_vault/'.stfolder').exists())
 env=dict(os.environ,OKILUM_QA_PEER_STATE=str(peer_state),OKILUM_QA_PEER_CONFIG=str(peer_home/'config.xml'),OKILUM_STATE_DIR=str(peer_state))
 peer=subprocess.Popen([str(peer_binary),'--exact','native_peer','--nocapture'],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(root/'peer-controller.err','w'),text=True,start_new_session=True);threading.Thread(target=peer_reader,daemon=True).start();assert peer_lines.get(timeout=30).get('ready')
 peer_command('publish',path=str(peer_vault));offer=peer_command('invite');url=offer['offer_url'];assert len(url)>80
 # The receiving Mac must not mistake a fixture daemon on its own VM for an existing local installation.
 # Pause only the owned counterpart while its invitation listener stays alive. Restore the identical
 # daemon/config/identity after Mac registration is claimed; no product state is edited or bypassed.
 os.killpg(daemon.pid,signal.SIGTERM);assert daemon.wait(timeout=15)==0;daemon=None
 for port in [rest,listen]:
  sock=socket.socket();sock.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);sock.bind(('127.0.0.1',port));reserved_ports.append(sock)
 event('isolated counterpart invitation ready; its owned daemon paused before fresh Mac inventory')
 click('I already have a pairing link');click('Paste pairing URL');run('pbcopy',input=url);run(driver,'paste');time.sleep(.5);screen('02-pasted-invitation');event('Mac UI pairing invitation pasted')
 click('Request connection')
 mac_id=None
 def requested():
  global source_api,mac_id
  allow_local_network_dialog();observe_runtime()
  if daemon is None:
   try:
    saved=json.loads((managed/'sidecar.json').read_text())
    if (saved.get('launch') or {}).get('phase')!='Claimed':return None
    source_api=api_from(managed/'config/config.xml');mac_id=source_api('/rest/system/status')['myID']
   except (OSError,ValueError):return None
   for sock in reserved_ports:sock.close()
   reserved_ports.clear();assert start_counterpart()==peer_id;event('identical owned source daemon resumed after receiving Mac claimed its own runtime')
  return peer_command('snapshot').get('pending')
 pending=wait('native Mac request reaches headless source after its fresh signed bootstrap',requested,source_wait);assert pending['incoming'] and pending['device_id']==mac_id
 screen('03-waiting-for-approval');peer_command('approve',request=pending['request_id']);event('independent source approved exact prepared Mac device')
 def available():
  allow_local_network_dialog();peer_command('refresh');return any(row['text'].strip()=='Choose folder' for row in ocr())
 wait('approved native Mac catalog and Choose folder',available,90);screen('04-received-catalog');click('Choose folder')
 wait('native folder picker',lambda:any('new folder' in row['text'].casefold() for row in ocr()),30);screen('05-native-folder-picker')
 def choose_destination_row():
  rows=ocr();row=next((r for r in rows if r['text'].strip().startswith('received-vault-')),None)
  if row:return row
  documents=next((r for r in rows if r['text'].strip()=='Documents' and r['x']<240),None)
  if documents:run(driver,'click',documents['x'],documents['y'])
  return None
 row=wait('owned empty directory row in the native Documents picker',choose_destination_row,30);run(driver,'click',row['x'],row['y']);event('clicked owned empty destination directory in the native picker');time.sleep(1);screen('05b-native-destination-selected')
 click('Use folder for ordinary-vault');event('confirmed the existing empty destination through the native folder picker')
 wait('destination selected in native Mac UI',lambda:any(row['text'].strip()=='Start first sync' for row in ocr()),30);assert not list(destination.iterdir());screen('06-empty-destination-selected');click('Start first sync')
 def received():
  allow_local_network_dialog();peer_command('refresh')
  return (destination/'seed.md').exists() and (destination/'seed.md').read_bytes()==seed
 wait('native Mac first receive exact seed bytes',received,120)
 source=destination;target=peer_vault;source_id=mac_id
 sf=source_api('/rest/config/folders/'+folder);assert sf['versioning']['type']=='staggered' and sf['versioning']['params']['maxAge']=='2592000';event('explicit UI receive created an active managed destination with 30-day history')
 def tick():
  source_api('/rest/db/scan?folder='+folder,True);receiver_api('/rest/db/scan?folder='+folder,True);return peer_command('refresh')
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
  # Keep every native interaction (including OS consent), source-ready and final-data evidence.
  base_seconds=record_started-started
  points=[max(0,e['seconds']-base_seconds) for e in events if e['event'].startswith(('clicked ','allowed ','explicit UI Share','both-direction','final native'))]
  spans=[(max(0,t-4),min(duration,t+8)) for t in points]+[(0,min(15,duration)),(max(0,duration-30),duration)]
  def merge_spans(items):
   result=[]
   for lo,hi in sorted(items):
    if result and lo<=result[-1][1]:result[-1]=(result[-1][0],max(hi,result[-1][1]))
    else:result.append((lo,hi))
   return result
  spans=merge_spans(spans)
  if sum(hi-lo for lo,hi in spans)<65:spans=merge_spans(spans+[(max(0,duration-65),duration)])
  if sum(hi-lo for lo,hi in spans)>170:spans=merge_spans([(max(0,t-2),min(duration,t+5)) for t in points]+[(max(0,duration-25),duration)])
  select='+'.join(f'between(t,{lo},{hi})' for lo,hi in spans)
  run('ffmpeg','-v','error','-i',raw,'-vf',f"select='{select}',setpts=N/(10*TB)",'-an','-c:v','libx264','-crf','24','-pix_fmt','yuv420p',video,timeout=120);edited=True
  (out/'video-segments.json').write_text(json.dumps(spans,indent=2)+'\n')
 else:shutil.copy2(raw,video)
 run('ffmpeg','-v','error','-xerror','-i',video,'-f','null','-',timeout=120);vp=json.loads(run('ffprobe','-v','error','-show_streams','-show_format','-of','json',video).stdout);assert 60<=float(vp['format']['duration'])<=180
 hashes={name:sha(source/name) for name in ['seed.md','from-mac.md','from-peer.md','positive.md']};assert all(sha(target/name)==digest for name,digest in hashes.items())
 result={'build':config['build'],'source':config['source'],'package_sha256':asset['sha256'],'signed_notarized_beta':True,'fresh_mac_account_state':True,'hosted_macos_version':platform.mac_ver()[0],'preparation_budget_seconds':preparation_budget,'startup_budget_seconds':startup_budget,'harness_source_wait_seconds':source_wait,'settings_did_not_bootstrap':True,'role':'receiver','empty_destination_chosen_via_native_folder_dialog':True,'invitation_pasted_via_Mac_UI':True,'peer_approved_exact_prepared_Mac_device':True,'counterpart':'separate real Syncthing2.1.6 plus production Controller library as existing source, ordinary LAN discovery (no manual device dial hints)','both_directions_exact':True,'file_hashes':hashes,'source_device':source_id,'peer_device':peer_id,'common_policy_equal':True,'mandatory_excludes_enforced':True,'managed_retention_days':30,'video_duration':float(vp['format']['duration']),'video_decode_exit':0,'video_edited_chronological':edited,'recorder_exit':recorder_code}
 event('SIGNED_BETA_FRESH_MAC_RECEIVING_PASS')
except BaseException as error:
 event('failure: '+str(error))
 try:screen('failure')
 except Exception:pass
 try:
  observe_runtime(True)
  click('Details',timeout=5);screen('failure-details')
  (out/'failure-details-ocr.json').write_text(json.dumps(ocr(),indent=2)+'\n')
 except Exception as e:(out/'details-diagnostic-error.txt').write_text(type(e).__name__+'\n')
 # The app already redacts the full error chain before writing these events.
 # Select only Sync failures; never export private state/configuration/journals.
 sync_errors=[]
 for log in [profile/'reader-diagnostic.log',source/'.okilum-index/reader-diagnostic.log']:
  if log.exists():
   for line in log.read_text().splitlines():
    try:
     value=json.loads(line)
     if value.get('phase')=='sync_action_failed':sync_errors.append({k:value.get(k) for k in ['phase','elapsed_ms','details']})
    except ValueError:pass
 (out/'sync-failures.json').write_text(json.dumps(sync_errors,indent=2)+'\n')
 (out/'failure.txt').write_text(str(error)+'\n')
 diagnostics={'managed_exists':managed.exists(),'managed_config_exists':(managed/'config/config.xml').exists(),'direct_exists':direct.exists()}
 try:diagnostics['source_rows']=[{k:r.get(k) for k in ['id','path','active']} for r in journal().get('sources',[])]
 except (OSError,ValueError):pass
 (out/'failure-state.json').write_text(json.dumps(diagnostics,indent=2)+'\n');raise
finally:
 stop_recording()
 cleanup={}
 for sock in reserved_ports:sock.close()
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
  (out/'launchctl-before-cleanup.txt').write_text(registration.stdout)
  saved=json.loads((managed/'sidecar.json').read_text());owned_bindings=list(bindings(saved));assert owned_bindings,'missing exact service ownership binding'
  assert all(v['owner']==str(os.getuid()) and v['state_directory']==str(managed) and pathlib.Path(v['supervisor']).is_relative_to(app) for v in owned_bindings),'refuse cleanup of a different service binding'
  run('codesign','--verify','--strict',owned_bindings[0]['supervisor'])
  cleanup['exact_verified_service_binding']=True
  runtime_pids=owned_runtime_pids();runtime_births={pid:run('ps','-p',pid,'-o','lstart=').stdout.strip() for pid in runtime_pids};subprocess.run(['launchctl','bootout',label],check=True,capture_output=True)
  for pid in runtime_pids:
   try:os.kill(pid,signal.SIGTERM)
   except ProcessLookupError:pass
  deadline=time.monotonic()+20
  while owned_runtime_pids() and time.monotonic()<deadline:time.sleep(.25)
  forced=[]
  remaining=owned_runtime_pids()
  for pid in runtime_pids:
   if pid in remaining:
    live=subprocess.run(['ps','-p',str(pid),'-o','lstart='],capture_output=True,text=True)
    if live.returncode==0 and live.stdout.strip()==runtime_births[pid]:
     try:os.kill(pid,signal.SIGKILL);forced.append(pid)
     except ProcessLookupError:pass
  deadline=time.monotonic()+5
  while owned_runtime_pids() and time.monotonic()<deadline:time.sleep(.25)
  cleanup['managed_registration_removed']=subprocess.run(['launchctl','print',label],capture_output=True).returncode!=0;cleanup['managed_runtime_processes_gone']=not owned_runtime_pids()
  (out/'fixture-stop.json').write_text(json.dumps({'grace_seconds':20,'forced_only_same_captured_pid_and_birth':forced},indent=2)+'\n')
 (out/'cleanup.json').write_text(json.dumps(cleanup,indent=2)+'\n')
 if result:
  result['cleanup']=cleanup;(out/'acceptance-receipt.json').write_text(json.dumps(result,indent=2)+'\n')
  assert all(cleanup.values()),cleanup
 print('MAC_BETA_FIXTURE_CLEANUP '+json.dumps(cleanup),flush=True)
