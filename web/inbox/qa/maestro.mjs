// Linux Chromium/CDP fixture QA: current and baseline assets, no live API requests.
import {chromium} from 'playwright-core';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const root=process.env.QA_ARTIFACT_DIR||'qa-artifacts/maestro';await fs.mkdir(root,{recursive:true});
const assets=path.resolve(process.env.QA_ASSET_ROOT||'web/inbox'),before=process.env.QA_BEFORE==='1';
const origin='https://inbox-qa.oklabs.uk',owner='11111111-1111-4111-8111-111111111111',pid='22222222-2222-4222-8222-222222222222';
const browser=await chromium.connectOverCDP(process.env.QA_CDP_ENDPOINT||'http://10.10.0.50:18811');
const reports=[];
try{
 for(const width of [390,1280])for(const theme of ['light','dark']){
  const context=await browser.newContext({viewport:{width,height:width===390?844:900},colorScheme:theme,serviceWorkers:'block'});
  let count=0,refuse=false;const ops={},unhandled=[],errors=[];
  const source={kind:'maestro',instance_id:'fixture',project_id:'pilot',thread_id:'worker-thread',worker_id:'worker',generation:'attempt',question_id:'question'};
  const q={id:'q',project_id:pid,thread_title:'Maestro pilot',source,source_revision:'["1",["reply"]]',state:'pending',source_fresh:true,can_reply:true,fields:[{id:'answer',prompt:'Which environment should we use?',options:[{id:'staging',label:'Staging'},{id:'production',label:'Production'}],allow_text:true,multiple:false}]};
  const a={...q,id:'a',source:{...source,record_kind:'approval',question_id:'approval',thread_id:'approval',worker_id:undefined},approval:{action:'merge_pr',repo:'BeFeast/fixture',target:{pr:42,head_sha:'abcdef012345'},summary:'Merge the reviewed documentation update?',risk:'medium',payload_hash:'fixture-payload',target_state_hash:'fixture-head'},source_revision:'["digest-1",["approve","reject"]]',fields:[{id:'decision',prompt:'Merge the reviewed documentation update?',options:[{id:'approve',label:'Approve'},{id:'reject',label:'Reject'}],allow_text:false,multiple:false}]};
  const questions=[q,a];
  await context.route('**/*',async route=>{
   const request=route.request(),u=new URL(request.url());assert.equal(u.origin,origin);
   if(!u.pathname.startsWith('/api/')){
    const file=u.pathname==='/'?'index.html':u.pathname.slice(1);
    if(file.includes('/')||file.includes('..'))throw new Error('Unexpected asset path');
    await route.fulfill({path:path.join(assets,file)});return;
   }
   let data={},status=200;const p=u.pathname.replace('/api/v1',''),body=request.postDataJSON();
   if(p==='/session')data={owner_id:owner};
   else if(p==='/items')data={changes:[],through:0,has_more:false};
   else if(p==='/projects')data={projects:[{id:pid,revision:1,draft:{title:'Inbox execution pilot',status:'',next_step:''}}]};
   else if(p.endsWith('/questions'))data={questions};
   else if(p.endsWith('/launches'))data={operations:[],has_more:false};
   else if(p.endsWith('/results'))data={results:[],has_more:false};
   else if(p.endsWith('/launch-targets'))data={targets:[]};
   else if(p==='/forgejo')data={enabled:false,repos:[]};
   else if(p.endsWith('/reply')){
    count++;
    if(refuse){status=409;data={error:'execution_revision_conflict'};}
    else{const item=questions.find(q=>q.id===body.question_id);data={state:'accepted',question:structuredClone(item),request:body};ops[body.operation_id]=data;item.can_reply=false;item.pending_operation_id=body.operation_id;}
   }else if(p.startsWith('/reply-operations/')){data=ops[p.split('/').at(-1)];if(!data){status=404;data={error:'not_found'};}}
   else if(p.startsWith('/questions/'))data=questions.find(q=>q.id===p.split('/').at(-1));
   else{unhandled.push(p);status=404;data={error:'fixture_unhandled'};}
   await route.fulfill({status,contentType:'application/json',body:JSON.stringify(data)});
  });
  try{
   const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
   await page.goto(origin);await page.locator('[data-nav="questions"]').click();
   await page.locator('#executor-questions button').first().waitFor();
   await page.screenshot({path:`${root}/list-${width}-${theme}.png`});
   await page.locator('#executor-questions button').first().click();await page.getByRole('button',{name:'Staging',exact:true}).waitFor();
   await page.screenshot({path:`${root}/question-${width}-${theme}.png`});
   if(!before){
    await page.getByRole('button',{name:'Staging',exact:true}).click();await page.locator('#executor-send').click();
    await page.locator('#executor-answer-status').getByText('Accepted by Maestro',{exact:true}).waitFor();assert.equal(count,1);
    ops[q.pending_operation_id].state='delivered';await page.locator('#executor-answer-status').getByText('Received',{exact:true}).waitFor({timeout:15000});assert.equal(count,1);
   }
   await page.locator('#executor-close').click();await page.locator('#executor-questions button').nth(1).click();
   await page.getByRole('button',{name:'Approve',exact:true}).waitFor();await page.screenshot({path:`${root}/approval-${width}-${theme}.png`});
   if(!before){
    assert.match(await page.locator('#executor-approval').innerText(),/BeFeast\/fixture/);
    assert.equal(await page.locator('.approval-exact').isVisible(),false);
    assert.equal(await page.locator('#executor-answer-fields fieldset,input[type=radio]').count(),0);
    assert.equal(await page.locator('#executor-send').isDisabled(),true);
    await page.getByRole('button',{name:'Exact action and target'}).click();assert.match(await page.locator('.approval-exact').innerText(),/abcdef012345/);
    await page.getByRole('button',{name:'Exact action and target'}).click();
    await page.getByRole('button',{name:'Approve',exact:true}).click();
    // New revision invalidates the unsent choice; no click can reuse old consent.
    a.source_revision='["digest-2",["approve","reject"]]';
    await page.waitForFunction(()=>document.querySelector('#executor-send').disabled,{timeout:15000});assert.equal(count,1);
    await page.getByRole('button',{name:'Approve',exact:true}).click();refuse=true;await page.locator('#executor-send').click();
    await page.locator('#executor-answer-status').getByText('Not sent',{exact:true}).waitFor();assert.equal(count,2);
    await page.locator('#executor-discard').click();refuse=false;await page.locator('#executor-send').click();
    await page.locator('#executor-answer-status').getByText('Accepted by Maestro',{exact:true}).waitFor();assert.equal(count,3);
    ops[a.pending_operation_id].state='delivered';await page.locator('#executor-answer-status').getByText('Decision recorded',{exact:true}).waitFor({timeout:15000});assert.equal(count,3);
    assert.match(await page.locator('#executor-answer-status').getAttribute('title'),/execution is not confirmed/);
    await page.screenshot({path:`${root}/decision-${width}-${theme}.png`});
   }
   assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
   assert.deepEqual(errors,[]);assert.deepEqual(unhandled,[]);
   const platform=await page.evaluate(()=>navigator.platform);assert.match(platform,/Linux/);
   reports.push({width,theme,before,platform,passed:true});
  }finally{await context.close();}
 }
 await fs.writeFile(`${root}/functional.json`,JSON.stringify(reports,null,2));console.log(reports);
}finally{await browser.close();}
