"""Actual compiled GPUI Help text on stock hosted Mac; no helper registration."""
import hashlib,json,os,pathlib,signal,subprocess,sys,tempfile,time
base=pathlib.Path(sys.argv[1]);driver=sys.argv[2];root=pathlib.Path(tempfile.mkdtemp(prefix='okilum-docs-ui-'));vault=root/'ordinary-vault';vault.mkdir();(vault/'seed.md').write_text('# Disposable Mac Help capture\n')
rows=[]
def run(*args):return subprocess.check_output([str(a) for a in args],text=True,timeout=30)
def wait(name,fn,timeout=60):
 end=time.monotonic()+timeout
 while True:
  value=fn()
  if value:return value
  if time.monotonic()>end:raise RuntimeError(name)
  time.sleep(.5)
def ocr():
 run('/usr/sbin/screencapture','-x',base/'current.png');rows=json.loads(run(driver,'ocrjson',base/'current.png'));(base/'current-ocr.json').write_text(json.dumps(rows,indent=2));return rows
def click(text,contains=False):
 found=wait('visible '+text,lambda:next((r for r in ocr() if (text.casefold() in r['text'].casefold() if contains else r['text'].strip().lstrip('›>▾ ').casefold()==text.casefold())),None))
 run(driver,'click',found['x'],found['y']);time.sleep(1)
for label in ['before','after']:
 profile=root/label;profile.mkdir();(profile/'features.json').write_text('{"sync":true}');(profile/'reader-ui.json').write_text('{"version":1,"settings_section":"sync"}')
 env=dict(os.environ,OKILUM_STATE_DIR=str(profile),DYLD_FRAMEWORK_PATH=str(base/'Frameworks'));binary=base/label/'okilum'
 process=subprocess.Popen([str(binary),'--vault',str(vault),'--note','seed.md'],env=env,stdout=open(base/(label+'-app.log'),'w'),stderr=subprocess.STDOUT,start_new_session=True)
 try:
  wait('Reader window',lambda:json.loads(run(driver,'list',process.pid)));run(driver,'activate',process.pid)
  wait('rendered Help seed note',lambda:any('Disposable Mac Help capture' in r['text'] for r in ocr()))
  next_key=0
  def settings():
   global next_key
   if any('Settings' in w.get('kCGWindowName','') for w in json.loads(run(driver,'list',process.pid))):return True
   if time.monotonic()>next_key:run(driver,'settings',process.pid);next_key=time.monotonic()+5
   return False
  wait('Settings window',settings);run(driver,'resize',process.pid);time.sleep(2)
  for _ in range(7):
   if any('Advanced' in r['text'] for r in ocr()):break
   run(driver,'scroll',880,570,-5);time.sleep(.7)
  click('Advanced',contains=True)
  run('/usr/sbin/screencapture','-x',base/(label+'-advanced.png'))
  (base/(label+'-advanced-ocr.json')).write_text(json.dumps(ocr(),indent=2))
  for _ in range(7):
   if any('Help: connect a computer' in r['text'] for r in ocr()):break
   run(driver,'scroll',880,570,-5);time.sleep(.7)
  click('connect a computer',contains=True);wait('native Help title',lambda:any('Sync setup help' in r['text'] for r in ocr()));time.sleep(2)
  image=base/(label+'-Mac-Sync-Help.png');run('/usr/sbin/screencapture','-x',image);text=run(driver,'ocr',image);(base/(label+'-ocr.txt')).write_text(text)
  assert ('macos is coming' in text.casefold()) if label=='before' else ('29262' in text and 'macos' in text.casefold()),text
  sections=[]
  for name,fragment,expected in [('Request','request the','Request connection'),('Save','Check both directions','Cmd+S' if label=='after' else 'Ctrl+S')]:
   for _ in range(45):
    visible=ocr();heading=next((r for r in visible if fragment.casefold() in r['text'].casefold() and r['x']>420),None)
    if heading and heading['y']<300:break
    run(driver,'scroll',850,550,-2 if heading else -4);time.sleep(.6)
   def instruction_visible():
    return any(expected in r['text'].replace('CtrI+S','Ctrl+S') for r in ocr())
   wait('visible '+name+' instruction',instruction_visible)
   section_image=base/(label+'-Mac-'+name+'-Help.png');run('/usr/sbin/screencapture','-x',section_image)
   (base/(label+'-'+name.lower()+'-ocr.txt')).write_text(run(driver,'ocr',section_image))
   sections.append(dict(name=name,screenshot_sha256=hashlib.sha256(section_image.read_bytes()).hexdigest(),expected_instruction=expected))
  rows.append(dict(label=label,build_info=(base/label/'build-info.txt').read_text(),screenshot_sha256=hashlib.sha256(image.read_bytes()).hexdigest(),native_help_rendered=True,ocr_control_passed=True,sections=sections))
 except BaseException:
  run('/usr/sbin/screencapture','-x',base/(label+'-failure.png'))
  (base/(label+'-failure-ocr.json')).write_text(json.dumps(ocr(),indent=2))
  raise
 finally:
  if process.poll() is None:
   os.killpg(process.pid,signal.SIGTERM)
   try:process.wait(timeout=15)
   except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
  assert process.poll() is not None
managed=pathlib.Path.home()/'Library/Application Support/Okilum/Sync';assert not managed.exists(),'Help started managed runtime'
(base/'native-ui-receipt.json').write_text(json.dumps(dict(captures=rows,no_managed_runtime_created=True,all_owned_actors_reaped=True),indent=2)+'\n');print('NATIVE_COMPILED_HELP_BEFORE_AFTER_PASS')
