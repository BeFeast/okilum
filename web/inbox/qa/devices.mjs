// Real WebAuthn and HTTP QA against an isolated CT119 backend, never the live DB.
// A route forwards the secure browser origin to QA_BACKEND; no API responses are mocked.
import {chromium} from 'playwright-core';
import fs from 'node:fs/promises';
import assert from 'node:assert/strict';
const backend=process.env.QA_BACKEND;
if(!backend||!process.env.QA_ENROLLMENT_FILE)throw new Error('An isolated backend and enrollment file are required');
const enrollment=(await fs.readFile(process.env.QA_ENROLLMENT_FILE,'utf8')).trim();
const origin=new URL(enrollment).origin,root=process.env.QA_ARTIFACT_DIR||'qa-artifacts/devices';
await fs.mkdir(root,{recursive:true});
const browser=await chromium.connectOverCDP(process.env.QA_CDP_ENDPOINT||'http://10.10.0.50:18811');
const contexts=[];
async function device(){
 const context=await browser.newContext({viewport:{width:1280,height:900},serviceWorkers:'block'});contexts.push(context);
 await context.route(`${origin}/**`,async route=>{
  const r=route.request(),u=new URL(r.url());
  if(process.env.QA_LOCAL_ASSETS==='1'&&!u.pathname.startsWith('/api/')){
   const file=u.pathname==='/'?'index.html':u.pathname.slice(1);
   if(!file.includes('/')&&!file.includes('..')){await route.fulfill({path:new URL('../'+file,import.meta.url).pathname});return;}
  }
  const response=await context.request.fetch(`${backend}${u.pathname}${u.search}`,{method:r.method(),headers:await r.allHeaders(),data:r.postDataBuffer()||undefined,maxRedirects:0});
  if(response.status()===422)console.error("QA validation:",u.pathname,await response.text());
  await route.fulfill({response});
 });
 const page=await context.newPage();page.on('dialog',d=>d.accept());
 const cdp=await context.newCDPSession(page);await cdp.send('WebAuthn.enable');
 await cdp.send('WebAuthn.addVirtualAuthenticator',{options:{protocol:'ctap2',transport:'internal',hasResidentKey:true,hasUserVerification:true,isUserVerified:true,automaticPresenceSimulation:true}});
 return{context,page};
}
async function api(page,path,body){return page.evaluate(async({path,body})=>{const r=await fetch('/api/v1'+path,{method:body===undefined?'GET':'POST',headers:{'content-type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});return{status:r.status,body:await r.json()};},{path,body});}
try{
 const a=await device(),b=await device();
 await a.page.goto(enrollment);await a.page.locator('#register').click();
 await a.page.waitForFunction(()=>!document.getElementById('logout').hidden);
 await a.page.locator('#devices-settings').click();
 await a.page.locator('#passkey-list li').waitFor();
 assert.equal(await a.page.locator('#passkey-list li').count(),1);
 assert.equal(await a.page.locator('#passkey-list button').isDisabled(),true);
 await a.page.locator('#invite-device').click();
 await a.page.waitForFunction(()=>document.getElementById('invite-link').value.includes('#device='));
 const link=await a.page.locator('#invite-link').inputValue();
 await b.page.goto(link);await b.page.locator('#join-device-name').fill('Second phone');await b.page.locator('#join-device-save').click();
 await b.page.waitForFunction(()=>document.getElementById('join-device-code').textContent.length===12);
 const code=await b.page.locator('#join-device-code').textContent();
 assert.equal((await api(b.page,'/session')).status,401);
 assert.equal((await api(a.page,'/passkeys')).body.keys.length,1);
 await a.page.getByText(`Match this code: ${code}`,{exact:true}).waitFor();
 for(const width of [390,1280])for(const theme of ['light','dark']){
  for(const [d,label] of [[a,'settings'],[b,'join']]){
   await d.page.setViewportSize({width,height:width===390?844:900});await d.page.emulateMedia({colorScheme:theme});await d.page.evaluate(t=>document.documentElement.dataset.theme=t,theme);
   await d.page.screenshot({path:`${root}/${label}-${width}-${theme}.png`,mask:[d.page.locator('#invite-link')]});
  }
 }
 await a.page.getByRole('button',{name:'Approve',exact:true}).click();
 await b.page.locator('#join-device-login').waitFor({state:'visible'});await b.page.locator('#join-device-login').click();
 await b.page.waitForFunction(()=>!document.getElementById('join-device-dialog').open);
 assert.equal((await api(b.page,'/session')).status,200);
 const keys=(await api(a.page,'/passkeys')).body.keys;assert.equal(keys.length,2);
 const second=keys.find(k=>k.name==='Second phone');assert.ok(second);
 await a.page.locator('#passkey-list li').filter({hasText:'Second phone'}).getByRole('button',{name:'Revoke'}).click();
 await a.page.waitForFunction(()=>document.querySelectorAll('#passkey-list li').length===1);
 assert.equal((await api(b.page,'/session')).status,401);
 assert.equal((await api(a.page,'/session')).status,200);
 const last=await api(a.page,'/passkeys/revoke',{id:keys.find(k=>k.id!==second.id).id});assert.equal(last.status,409);
 await fs.writeFile(`${root}/functional.json`,JSON.stringify({realWebAuthn:true,isolatedBackend:true,approvalBeforeLogin:true,revokeInvalidatesSession:true,lastKeyProtected:true},null,2));
 console.log('Device enrollment, owner approval, login, session revocation and last-key protection PASS');
}finally{for(const c of contexts)await c.close();await browser.close();}
