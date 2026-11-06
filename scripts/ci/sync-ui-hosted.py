"""Capture real production GPUI Settings on stock hosted macOS; no tests, install or helper setup."""
import hashlib,json,os,pathlib,signal,subprocess,sys,tempfile,time
base=pathlib.Path(sys.argv[1]);driver=sys.argv[2]
root=pathlib.Path(tempfile.mkdtemp(prefix='okilum-sync-ui-'));vault=root/'ordinary-vault';vault.mkdir();(vault/'seed.md').write_text('# Disposable Mac UI vault\n\nBefore/after screenshots only.\n')
rows=[]
def run(*args):return subprocess.check_output([str(a) for a in args],text=True,timeout=25)
for label in ['before','after']:
 profile=root/label;profile.mkdir();(profile/'features.json').write_text('{"sync":true}')
 (profile/'reader-ui.json').write_text('{"version":1,"settings_section":"sync"}')
 env=dict(os.environ,OKILUM_STATE_DIR=str(profile),DYLD_FRAMEWORK_PATH=str(base/'Frameworks'))
 binary=base/label/'okilum';binary.chmod(0o755)
 app=subprocess.Popen([str(binary),'--vault',str(vault),'--note','seed.md'],env=env,stdout=open(base/(label+'-app.log'),'w'),stderr=subprocess.STDOUT,start_new_session=True)
 try:
  deadline=time.monotonic()+40
  while True:
   if app.poll() is not None:raise RuntimeError(label+' exited: '+(base/(label+'-app.log')).read_text())
   windows=json.loads(run(driver,'list',app.pid))
   if windows:break
   if time.monotonic()>deadline:raise RuntimeError('no Reader window')
   time.sleep(.5)
  run(driver,'activate',app.pid);time.sleep(2);run(driver,'settings',app.pid)
  deadline=time.monotonic()+20
  while True:
   windows=json.loads(run(driver,'list',app.pid));settings=[w for w in windows if 'Settings' in w.get('kCGWindowName','')]
   if settings:break
   if time.monotonic()>deadline:raise RuntimeError('no Settings window: '+json.dumps(windows))
   time.sleep(.5)
  resize=run(driver,'resize',app.pid);time.sleep(4)
  windows=json.loads(run(driver,'list',app.pid));settings=[w for w in windows if 'Settings' in w.get('kCGWindowName','')];assert len(settings)==1,windows
  image=base/(label+'-Mac-Settings-Sync.png');run('/usr/sbin/screencapture','-x','-l',settings[0]['kCGWindowNumber'],image)
  text=run(driver,'ocr',image);(base/(label+'-ocr.txt')).write_text(text)
  assert 'Sync' in text and ('Add computer' in text or 'pairing link' in text),text
  if label=='after':assert 'ordinary-vault' in text and 'Share with my computers' in text,text
  else:assert 'Share with my computers' not in text,text
  rows.append({'label':label,'build_info':(base/label/'build-info.txt').read_text(),'screenshot_sha256':hashlib.sha256(image.read_bytes()).hexdigest(),'settings_window':settings[0],'resize_result':resize,'production_rendered':True,'ocr_control_passed':True})
 finally:
  if app.poll() is None:
   os.killpg(app.pid,signal.SIGTERM)
   try:app.wait(timeout=10)
   except subprocess.TimeoutExpired:os.killpg(app.pid,signal.SIGKILL);app.wait()
(base/'native-ui-receipt.json').write_text(json.dumps({'captures':rows,'real_mac_hosted_ui':True,'helper_setup':False,'pairing':False,'all_owned_actors_reaped':True},indent=2)+'\n')
print('NATIVE_MAC_BEFORE_AFTER_PASS')
