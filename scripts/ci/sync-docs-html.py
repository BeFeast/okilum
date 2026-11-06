"""Capture the two changed real offline Help sections with unmodified DOM."""
import hashlib,json,pathlib,subprocess,sys
from playwright.sync_api import sync_playwright
out=pathlib.Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=True);rows=[]
with sync_playwright() as p:
 browser=p.chromium.launch(executable_path='/usr/bin/google-chrome',args=['--no-sandbox'])
 page=browser.new_page(viewport={'width':1440,'height':1000},device_scale_factor=1,reduced_motion='reduce')
 for label,source in [('before','3123a1b46a2078c3e3393830db776144df674657'),('after','657d1f347c44429c9ed31371778e5ab28d1f1b08')]:
  subprocess.run(['git','checkout','--detach',source],check=True)
  subprocess.run(['python3','docs/help/sync/check.py'],check=True)
  document=pathlib.Path('docs/help/sync/index.html').resolve()
  page.goto(document.as_uri(),wait_until='load');page.evaluate('document.fonts.ready');page.wait_for_timeout(1000)
  for section,selector in [('connect','section#connect'),('save','section[aria-labelledby="everyday-title"]')]:
   node=page.locator(selector);text=node.inner_text()
   if label=='after':assert ('29262' in text and 'Login Items' in text) if section=='connect' else ('Cmd+S' in text and 'Ctrl+S' in text)
   if label=='before':assert ('macOS is coming' in text) if section=='connect' else ('Ctrl+S' in text and 'Cmd+S' not in text)
   image=out/(label+'-HTML-'+section+'.png');node.screenshot(path=str(image),animations='disabled')
   (out/(label+'-'+section+'-text.txt')).write_text(text)
   rows.append(dict(label=label,section=section,source=source,html_sha256=hashlib.sha256(document.read_bytes()).hexdigest(),screenshot_sha256=hashlib.sha256(image.read_bytes()).hexdigest(),offline_checker_passed=True))
 browser.close()
(out/'html-ui-receipt.json').write_text(json.dumps(dict(captures=rows,unmodified_DOM=True),indent=2)+'\n')
