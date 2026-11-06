"""Two isolated production CLI peers, published beta to exact PR source; no real vault."""
import hashlib,json,os,pathlib,subprocess,urllib.request
assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('RUNNER_OS')=='Linux'
temp=pathlib.Path(os.environ['RUNNER_TEMP']);tools=temp/'native-tools';out=temp/'native-evidence';qa=temp/'q';source=os.environ['OKILUM_SOURCE_COMMIT']
assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==source

def executable(file,name):
 rows=[json.loads(l) for l in file.read_text().splitlines() if l.startswith('{')]
 matches=[r['executable'] for r in rows if r.get('reason')=='compiler-artifact' and r.get('executable') and r['target']['name']==name]
 assert len(matches)==1,matches
 return pathlib.Path(matches[0])

def get(url):
 with urllib.request.urlopen(urllib.request.Request(url,headers={'User-Agent':'curl'}),timeout=120) as r:return r.read()

after=executable(tools/'product.jsonl','okilum');prepare=executable(tools/'prepare.jsonl','qa_native_prepare')
info=subprocess.check_output([str(after),'--build-info'],text=True);assert source in info and 'gpui-test-support: no' in info; (out/'after-build-info.txt').write_text(info)
build=7632;feed=json.loads(get(f'https://updates.befeast.com/okilum/linux/builds/{build}/release.json'));assert feed['build']==build and feed['source']=='7614a66c11f0d2a82e5be5b435cc92667b79b84a'
(out/'before-release.json').write_text(json.dumps(feed,indent=2)+'\n')
package=qa/'Okilum.AppImage';package.write_bytes(get(f'https://updates.befeast.com/okilum/linux/builds/{build}/Okilum-x86_64.AppImage'));assert hashlib.sha256(package.read_bytes()).hexdigest()==feed['sha256'];package.chmod(0o755)
subprocess.run([str(package),'--appimage-extract'],cwd=qa,stdout=subprocess.DEVNULL,check=True)
before=qa/'squashfs-root/AppRun';client=qa/'squashfs-root/usr/lib/okilum/syncthing/syncthing'
info=subprocess.check_output([str(before),'--build-info'],text=True);assert feed['source'] in info;(out/'before-build-info.txt').write_text(info)
for order in ['source-first','receiver-first']:
 args=['sudo','unshare','--mount','--pid','--fork','--mount-proc','python3','-u',str(tools/'qa_upgrade.py'),'--before',str(before),'--before-protocol','4','--after',str(after),'--prepare',str(prepare),'--syncthing',str(client),'--evidence',str(qa)]
 if order=='receiver-first':args.append('--receiver-first')
 with (out/(order+'.log')).open('w') as log:
  result=subprocess.run(args,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,timeout=900)
 print((out/(order+'.log')).read_text(),flush=True);assert result.returncode==0,(order,result.returncode)
 log=(out/(order+'.log')).read_text();assert 'UPGRADE_READY ' in log and 'UPGRADE_CLEANUP complete' in log
 receipt=json.loads((qa/'upgrade-receipt.json').read_text());receipt.update(product_source=source,beta_build=build,beta_source=feed['source'],explicit_fixture_cleanup=True)
 assert receipt['pairing_preserved'] and receipt['mixed_version_bidirectional'] and receipt['bidirectional_before'] and receipt['bidirectional_after'] and receipt['remote_history_preimage_preserved'] and receipt['retention_days']==30
 (out/(order+'-receipt.json')).write_text(json.dumps(receipt,indent=2)+'\n')
print('EXACT_SOURCE_BETA_UPGRADE_PASS '+source,flush=True)
