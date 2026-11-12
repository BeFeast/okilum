import json, pathlib, subprocess, tomllib
targets=json.loads(pathlib.Path('deps-targets.json').read_text())
for manifest in ['Cargo.toml','inbox/Cargo.toml']:
 lock=pathlib.Path(manifest).with_name('Cargo.lock')
 before=tomllib.loads(lock.read_text())['package']
 frozen={p['name']:p for p in before if p['name'].startswith('gpui-pre')}
 for name, target in targets.items():
  packages=[p for p in tomllib.loads(lock.read_text())['package'] if p['name']==name]
  for p in packages:
   current=p['version']
   # Older independent SemVer lines (webpki-roots 0.26) remain in place.
   if target==current or (current.startswith('0.') and current.split('.')[:2]!=target.split('.')[:2]): continue
   subprocess.run(['cargo','update','--manifest-path',manifest,'-p',name+'@'+current,'--precise',target],check=True)
   after={p['name']:p for p in tomllib.loads(lock.read_text())['package'] if p['name'].startswith('gpui-pre')}
   assert frozen==after, 'GPUI family changed'
 subprocess.run(['cargo','metadata','--locked','--manifest-path',manifest,'--format-version','1','--no-deps'],check=True,stdout=subprocess.DEVNULL)
