const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const fs=require('fs'),path=require('path');
(async()=>{
const b=await chromium.launch({executablePath:process.env.CHROMIUM_PATH});const p=await b.newPage();
await p.goto('file://'+path.resolve(__dirname,'../prototype/index.html'));
const rows=[];
for(const [width,height] of [[1440,900],[1280,800],[1024,768],[640,400],[390,844]]){
 await p.setViewportSize({width,height});
 for(const v of ['attention','brain','goals','connections','export','library','mobile']){
  let l=p.locator(`[data-nav="${v}"]`);l=await l.locator('visible=true').count()?l.locator('visible=true').first():l.first();for(const d of await l.locator('xpath=ancestor::details').all())if(await d.getAttribute('open')===null)await d.locator(':scope > summary').click();await l.click();await p.evaluate(()=>document.fonts.ready);
  rows.push({width,height,view:v,...await p.evaluate(()=>({scrollWidth:document.documentElement.scrollWidth,clientWidth:document.documentElement.clientWidth}))});
 }
}
fs.writeFileSync(path.join(__dirname,'layout-final.json'),JSON.stringify({brand:'1.1.0',rows},null,2));
console.log(JSON.stringify({checks:rows.length,overflow:rows.filter(x=>x.scrollWidth>x.clientWidth)}));await b.close();
})();
