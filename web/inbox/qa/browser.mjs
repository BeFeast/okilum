import {chromium} from 'playwright-core';
import fs from 'node:fs/promises';
import assert from 'node:assert/strict';
const root=process.env.QA_ARTIFACT_DIR||'qa-artifacts';await fs.mkdir(root,{recursive:true});
const browser=await chromium.connectOverCDP(process.env.QA_CDP_ENDPOINT||'http://10.10.0.50:18811');
const now=Date.now(),iso=new Date(now-3600000).toISOString(),owner='00000000-0000-4000-8000-000000000001',pid='00000000-0000-4000-8000-000000000002';
function fixtures(){
 const project={id:pid,revision:1,draft:{title:'Tessera',status:'UX review in progress',next_step:'Review the Inbox redesign on phone and desktop.'}};
 const notes=['An Inbox that feels like a quiet place to land\nOne place for ideas, questions and the next small step.','What if project updates came to me?\nA short daily summary, with a clear decision when one is needed.','Make the next step obvious\nEvery project should answer: what is waiting, and what happens next?','Notes from the morning walk\nKeep capture light. Let structure come later.','A reading list for the weekend\nDesigning calm tools, small systems and better defaults.'];
 const items=notes.map((text,i)=>({id:`note${i}`,owner_id:owner,original_text:text,received_at_ms:now-i*86400000}));
 const questions=['Should we keep the project summary short?','Which pilot colour?','Ready to review the new navigation?'].map((prompt,i)=>({id:`q${i}`,project_id:pid,state:i===2?'answered':'pending',source_revision:'r1',source_fresh:true,can_reply:i!==2,thread_title:'Inbox execution pilot',source:{kind:'t3',thread_id:`fixture-thread-${i}`} ,fields:[{id:'choice',prompt,options:[{id:'blue',label:i===1?'Blue — Choose Blue.':'Yes, keep it focused'},{id:'green',label:i===1?'Green — Green':'Let’s discuss it'}],multiple:false,allow_text:true}]}));
 const issue=(number,title,assignees=['oleg'])=>({number,title,assignees,state:'open',url:`https://git.oklabs.uk/BeFeast/tessera/issues/${number}`,updated_at:iso});
 const pull=(number,title,check)=>({...issue(number,title),head_commit:'a'.repeat(40),checks:[{context:'ci / check',state:check,updated_at:iso}],url:`https://git.oklabs.uk/BeFeast/tessera/pulls/${number}`});
 const repo=(id,name,issues,pulls,releases=[])=>({id,name,url:`https://git.oklabs.uk/${name}`,synced_at:Math.floor(now/1000)-120,stale:false,issues,pulls,releases});
 const repos=[repo(1,'BeFeast/tessera',[issue(555,'A calmer, more focused Inbox'),issue(510,'Inbox · execution and project overview'),issue(552,'Keep keyboard navigation predictable',[])],[pull(543,'Project screen and execution results','success'),pull(550,'Refine the reader’s navigation','pending')],[{tag:'v0.1.6340',name:'Reader beta · 6340',prerelease:true,published_at:iso,url:'https://git.oklabs.uk/BeFeast/tessera/releases',assets:[]}]),repo(2,'BeFeast/maestro',[issue(1288,'A guarded bridge for executor questions')],[pull(1289,'Durable reply delivery and recovery','success')]),repo(3,'BeFeast/homelab',[issue(204,'Nightly backups for Inbox'),issue(207,'Review the shared runner capacity',[])],[pull(208,'Backup health checks','failure')])];
 const op={request:{operation_id:'launch1'},brief:{title:'Polish the Inbox project screen'},target:{target:{label:'Isolated pilot',model_selection:{model:'gpt-6-astra'}}},thread_id:'fixture-thread-1',run_id:'run1',state:'completed',worktree_path:'/fixture/worktrees/pilot'};
 return{project,items,questions,repos,op,offline:false,replies:[],results:[],captures:[],errorPaths:[]};
}
async function setup(width,theme){
 const context=await browser.newContext({viewport:{width,height:width===390?844:900},colorScheme:theme,ignoreHTTPSErrors:true,serviceWorkers:'block'});const state=fixtures();
 await context.route('**/api/v1/**',async route=>{const request=route.request(),u=new URL(request.url()),p=u.pathname.replace('/api/v1','');let body=request.postDataJSON(),data,status=200;
  if(state.offline){await route.abort('failed');return;}
  if(p==='/session')data={owner_id:owner};
  else if(p==='/items'&&request.method()==='POST'){let item=state.items.find(i=>i.id===body.item_id);if(!item){item={id:body.item_id,owner_id:owner,original_text:body.text,received_at_ms:Date.now()};state.items.push(item);state.captures.push(body);}data={operation_id:body.operation_id,item};}
  else if(p==='/items')data={changes:state.items.map(item=>({item})),has_more:false,through:state.items.length};
  else if(p==='/projects'&&request.method()==='POST'){state.project={...state.project,draft:body.draft,revision:state.project.revision+1};data=state.project;}
  else if(p==='/projects')data={projects:[state.project,{id:'00000000-0000-4000-8000-000000000003',revision:1,draft:{title:'HomeLab',status:'Backups are healthy',next_step:'Review the weekly report.'}}]};
  else if(p.includes('/launch-targets'))data={targets:[]};
  else if(p.includes('/projects/')&&p.endsWith('/questions'))data={questions:state.questions};
  else if(p.includes('/projects/')&&p.endsWith('/launches'))data={operations:[state.op],has_more:false};
  else if(p.includes('/projects/')&&p.endsWith('/results'))data={results:state.results,has_more:false};
  else if(p.endsWith('/output'))data={output:{text:'The updated project screen is ready for review.\n\nCheck the navigation, question list and mobile layout.'}};
  else if(p==='/forgejo')data={enabled:true,instance:'https://git.oklabs.uk',account_login:'read-only',discovered_at:Math.floor(now/1000)-120,repos:u.searchParams.has('project')?[state.repos[0]]:state.repos,execution_links:[]};
  else if(p.startsWith('/questions/')&&p.endsWith('/reply')){state.replies.push(body);data={state:'accepted',request:body};}
  else if(p.startsWith('/questions/'))data=state.questions.find(q=>q.id===p.split('/').at(-1));
  else if(p.endsWith('/discussion'))data=[];
  else if(p.endsWith('/publications'))data=[];
  else if(p==='/destinations')data={folders:['Projects','Areas','Resources','Archives']};
  else{state.errorPaths.push(p);status=404;data={error:'fixture_unhandled'};}
  await route.fulfill({status,contentType:'application/json',body:JSON.stringify(data)});
 });
 const page=await context.newPage(),errors=[];page.on('pageerror',e=>errors.push(e.message));await page.goto(process.env.QA_BASE_URL||'https://10.10.0.23:18770/');await page.locator('#connection').filter({hasText:'Connected'}).waitFor();await page.locator('#thoughts li').first().waitFor();await page.evaluate(()=>document.fonts.ready);
 return{context,page,state,errors};
}
try{
 const reports=[];
 for(const width of [390,1280])for(const theme of ['light','dark']){
  const{context,page,state,errors}=await setup(width,theme);
  try{
   for(const screen of ['inbox','questions','projects','overview']){await page.locator(`[data-nav="${screen}"]`).click();await page.waitForTimeout(150);assert.equal(await page.locator(`[data-screen="${screen}"]`).isVisible(),true);assert.equal(await page.locator(`[data-screen="${screen}"] .page-heading p`).count(),0);assert.equal(await page.locator(`[data-screen="${screen}"] select[id$=project]:visible, [data-screen="${screen}"] #project-select:visible`).count(),0);assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'horizontal overflow');await page.screenshot({path:`${root}/${screen}-${width}-${theme}.png`});}
   await page.locator('[data-nav="projects"]').click();await page.locator('[data-screen="projects"] .project-choices').first().getByRole('button',{name:'HomeLab',exact:true}).click();await page.waitForFunction(()=>document.getElementById('project-summary').value==='Backups are healthy');await page.locator('[data-screen="projects"] .project-choices').first().getByRole('button',{name:'Tessera',exact:true}).click();await page.waitForFunction(()=>document.getElementById('project-summary').value==='UX review in progress');assert.equal(await page.getByText('Publication history (0)',{exact:true}).count(),0);await page.locator('[data-nav="overview"]').click();
   await page.locator('[data-kind="pull"][aria-pressed]').click();assert.equal(await page.locator('#forgejo-list .work-row').count(),4);
   await page.locator('#overview-search').fill('backup');assert.equal(await page.locator('#forgejo-list .work-row').count(),1);await page.locator('#forgejo-list .work-row').click();await page.locator('#overview-detail[open]').waitFor();assert.match(await page.locator('#overview-detail-body').innerText(),/failure/);await page.screenshot({path:`${root}/detail-${width}-${theme}.png`});await page.locator('#overview-close').click();
   await page.locator('[data-nav="questions"]').click();await page.locator('#executor-questions button').nth(1).click();await page.getByLabel('Blue',{exact:true}).waitFor();assert.equal(await page.locator('#executor-answer-fields fieldset').count(),0);assert.equal(await page.locator('#executor-source').innerText(),'Tessera · Inbox execution pilot');assert.equal(await page.locator('#executor-answer-fields').getByText('Blue — Choose Blue.',{exact:true}).count(),0);await page.screenshot({path:`${root}/question-detail-${width}-${theme}.png`});await page.getByLabel('Blue',{exact:true}).check();await page.locator('#executor-send').click();await page.getByText('Accepted by T3 — receipt by the executor is not yet confirmed',{exact:true}).waitFor();assert.equal(state.replies.length,1);await page.locator('#executor-close').click();
   await page.locator('[data-nav="projects"]').click();await page.locator('#project-summary').fill('Ready for design review');await page.locator('#project-save').click();await page.getByText('Project status saved.',{exact:true}).waitFor();assert.equal(state.project.draft.status,'Ready for design review');
   await page.locator('[data-nav="inbox"]').click();await page.locator('#thoughts button').first().click();await page.locator('#discussion:not([hidden])').waitFor();assert((await page.locator('#ask').boundingBox()).width<160);assert((await page.locator('#ask').boundingBox()).height<=40);await page.screenshot({path:`${root}/thought-detail-${width}-${theme}.png`});await page.locator('#close-detail').click();
   await page.locator('#new-thought').click();await page.locator('#thought').fill('Captured during browser QA');await Promise.all([page.waitForResponse(r=>r.url().endsWith('/api/v1/items')&&r.request().method()==='POST'),page.locator('#save').click()]);await page.waitForFunction(()=>!document.querySelector('#sync').disabled);await page.locator('#thoughts').getByText('Captured during browser QA',{exact:true}).waitFor();assert.equal(state.captures.length,1);
   state.offline=true;await page.locator('#thought').fill('Offline browser QA');await page.locator('#save').click();await page.locator('#thoughts').getByText('Offline browser QA',{exact:true}).waitFor();assert.equal(state.captures.length,1);await page.waitForFunction(()=>!document.querySelector('#sync').disabled);state.offline=false;await Promise.all([page.waitForResponse(r=>r.url().endsWith('/api/v1/items')&&r.request().method()==='POST'),page.locator('#sync').click()]);await page.waitForFunction(()=>!document.querySelector('#sync').disabled);assert.equal(state.captures.length,2);
   assert.deepEqual(errors,[]);assert.deepEqual(state.errorPaths,[], 'Unhandled fixture API requests');
   const destinations=await page.evaluate(()=>fetch('/api/v1/destinations').then(r=>r.json()));assert.deepEqual(destinations.folders,['Projects','Areas','Resources','Archives']);
   await page.evaluate(()=>fetch('/api/v1/qa-positive-control'));assert.throws(()=>assert.deepEqual(state.errorPaths,[]));assert.deepEqual(state.errorPaths,['/qa-positive-control']);state.errorPaths.length=0;
   reports.push({width,theme,passed:true,unhandled:state.errorPaths});
  }finally{await context.close();}
 }
 await fs.writeFile(`${root}/functional.json`,JSON.stringify(reports,null,2));console.log(reports);
}finally{await browser.close();}
