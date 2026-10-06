import {repositoryCards,freshness} from './forgejo.js';
import {launchLabel} from './launches.js';
import {questionStatus} from './questions.js';
export function latestPublished(records) {
  const groups=new Map();
  for(const row of records){const r=row.report;if(r.publication==='published')groups.set(JSON.stringify([r.platform,r.channel]),row);}
  return [...groups.values()];
}
export function safeResultURL(value){try{const u=new URL(value);return u.protocol==='https:'&&!u.username&&!u.password?u.href:null;}catch{return null;}}
export function validateStatus(draft){
 for(const [key,n] of [['status',256],['next_step',4096]])if(draft[key].includes('\0')||new TextEncoder().encode(draft[key]).length>n)throw new Error(`${key}: maximum ${n} UTF-8 bytes.`);
}
export function validateResult(r){
 const bounded=(value,n)=>typeof value==='string'&&value.trim()&&!value.includes('\0')&&new TextEncoder().encode(value).length<=n;
 for(const key of ['platform','channel','version'])if(!bounded(r[key],128))throw new Error(`${key}: enter 1–128 UTF-8 bytes.`);
 if(!bounded(r.what_to_check,8192))throw new Error('What to check: enter 1–8192 UTF-8 bytes.');
 if(!safeResultURL(r.url)||new TextEncoder().encode(r.url).length>2048)throw new Error('Enter an HTTPS result link up to 2048 bytes.');
}
export function projectJournal(storage,owner,project,kind){const key=`tessera-project-${kind}-v1:${owner}:${project}`;return{get:()=>JSON.parse(storage.getItem(key)||'null'),put:v=>storage.setItem(key,JSON.stringify(v)),clear:()=>storage.removeItem(key)};}
export function mountProjects({api,post,owner,online,openQuestion,storage=localStorage}){
 const $=id=>document.getElementById(id),el=(tag,text)=>{const n=document.createElement(tag);n.textContent=text;return n;};
 let who=null,epoch=0,busy=false,refreshAgain=false,writing=false,project=null,launches=[],drafts=new Map(),notice='';
 const journal=kind=>projectJournal(storage,who,project.id,kind);
 const valid=(mine,e)=>mine===owner()&&e===epoch;
 function reset(){who=null;epoch++;project=null;launches=[];drafts.clear();notice='';for(const id of ['project-summary','project-next','result-commit','result-platform','result-channel','result-version','result-url','result-qa'])$(id).value='';$('project-panel').hidden=true;for(const id of ['project-questions','project-executors','project-results','project-repos'])$(id).replaceChildren();}
 function status(s){notice=s;$('project-status').textContent=s;}
 async function pages(path,key,numeric=false){let cursor=numeric?0:'',all=[];for(let i=0;i<100;i++){const data=await api(`${path}?after=${encodeURIComponent(cursor)}&limit=100`);all.push(...data[key]);if(numeric?!data.has_more:data[key].length<100)return all;const next=numeric?data.next_cursor:data.next_after;if(!next||next===cursor)throw new Error('Incomplete project data.');cursor=next;}throw new Error('Project data limit reached.');}
 async function refresh(){
  if(!owner()){reset();return;}if(busy){refreshAgain=true;return;}if(writing)return;
  if(who!==owner()){reset();who=owner();}
  const mine=who,e=epoch;busy=true;
  try{
   const list=await pages('/projects','projects');if(!valid(mine,e))return;
   const chosen=$('project-select').value;$('project-select').replaceChildren(...list.map(p=>{const o=el('option',p.draft.title);o.value=p.id;return o;}));if(list.some(p=>p.id===chosen))$('project-select').value=chosen;
   $('project-panel').hidden=false;if(!list.length){status('No projects connected.');return;}
   project=list.find(p=>p.id===$('project-select').value);await load(mine,e);
  }catch(error){if(valid(mine,e))status(`Unavailable: ${error.message}. Previous observations may be stale.`);}
  finally{busy=false;if(refreshAgain){refreshAgain=false;await refresh();}}
 }
 async function load(mine,e){
  const id=project.id;
  const results=await Promise.allSettled([pages(`/projects/${id}/questions`,'questions'),pages(`/projects/${id}/launches`,'operations',true),pages(`/projects/${id}/results`,'results',true),api(`/forgejo?project=${id}`)]);
  if(!valid(mine,e)||id!==project?.id)return;
  const pending=journal('status').get();
  if(document.activeElement!==$('project-summary')&&document.activeElement!==$('project-next')){
   $('project-summary').value=pending?.draft.status??drafts.get(id)?.status??project.draft.status;$('project-next').value=pending?.draft.next_step??drafts.get(id)?.next_step??project.draft.next_step;
  }
  for(const name of ['project-summary','project-next'])$(name).disabled=Boolean(pending);
  $('project-save').textContent=pending?'Retry saved status':'Save status';$('project-clear-status').hidden=!pending;
  $('project-status').textContent=notice||`Project status revision ${project.revision} · checked ${new Date().toLocaleTimeString()}`;
  const [qs,ls,rs,fs]=results;
  if(qs.status==='fulfilled'){
   $('project-questions').replaceChildren(...qs.value.map(q=>{const b=el('button',`${q.fields.map(f=>f.prompt).join(' · ')} · ${questionStatus(q,null,online())}`);b.className='thought-row';b.onclick=()=>openQuestion(q.id);return b;}));
   if(!qs.value.length)$('project-questions').append(el('p','No source questions.'));
  }else $('project-question-status').textContent='Questions unavailable; previous observations may be stale.';
  if(qs.status==='fulfilled')$('project-question-status').textContent='';
  if(ls.status==='fulfilled'){
   launches=ls.value;
   $('project-executors').replaceChildren(...[...launches].reverse().map(op=>{
    const d=el('details','');d.append(el('summary',`${op.brief.title} · ${launchLabel(op)}`),el('p',`Executor: ${op.target.target.model_selection.model} · ${op.target.target.label}`),el('p',`Thread: ${op.thread_id}`));
    if(op.worktree_path)d.append(el('p',`Worktree: ${op.worktree_path}`));
    const result=el('pre',op.state==='completed'?'Result not yet received from T3.':'');d.append(result);
    if(op.state==='completed')d.addEventListener('toggle',()=>{if(!d.open||d.dataset.loaded)return;d.dataset.loaded='1';api(`/launches/${op.request.operation_id}/output`).then(v=>{if(valid(mine,e)&&id===project?.id)result.textContent=v.output?.text??'Result not yet received from T3.';}).catch(()=>{if(valid(mine,e))result.textContent='Source result unavailable.';delete d.dataset.loaded;});});
    return d;
   }));
   if(!launches.length)$('project-executors').append(el('p','No executors launched for this project.'));
   const selected=$('result-launch').value;
   $('result-launch').replaceChildren(...launches.filter(o=>['completed','failed'].includes(o.state)&&o.run_id).map(o=>{const n=el('option',`${o.brief.title} · ${launchLabel(o)}`);n.value=o.request.operation_id;return n;}));
   if(launches.some(o=>o.request.operation_id===selected))$('result-launch').value=selected;
  }else status('Execution source unavailable; previous observations may be stale.');
  if(rs.status==='fulfilled'){
   const all=rs.value,latest=latestPublished(all),fragment=document.createDocumentFragment();
   fragment.append(el('h4','Latest reported publication per platform / channel'));
   for(const r of latest)fragment.append(resultCard(r));
   if(!latest.length)fragment.append(el('p','No published result has been recorded. A completed executor is not proof of publication.'));
   const history=el('details','');history.append(el('summary',`Publication history (${all.length})`));for(const r of [...all].reverse())history.append(resultCard(r));fragment.append(history);$('project-results').replaceChildren(fragment);
  }else $('project-results').prepend(el('p','Results unavailable; previously shown records may be stale.'));
  if(fs.status==='fulfilled'&&fs.value.enabled){$('project-repo-status').textContent=freshness(fs.value);$('project-repos').replaceChildren(...repositoryCards(fs.value));if(!fs.value.repos.length)$('project-repos').append(el('p','No repositories explicitly linked to this project. The all-repository overview remains available below.'));}
  else $('project-repo-status').textContent=fs.status==='fulfilled'?'Forgejo is not connected.':'Forgejo unavailable; previous observations may be stale.';
  renderPendingResult();
 }
 function resultCard(row){const r=row.report,d=el('article','');d.className='project-result';d.append(el('strong',`${r.platform} / ${r.channel} · ${r.version} · ${r.publication}`),el('p',`Reported by you · ${new Date(row.recorded_at*1000).toLocaleString()}`),el('p',`Commit: ${r.commit}`),el('p',`Run: ${r.run_id}`));const a=el('a','Open result / source');const url=safeResultURL(r.url);if(url){a.href=url;a.target='_blank';a.rel='noopener noreferrer';}d.append(a,el('h4','What to check'),el('pre',r.what_to_check));return d;}
 const resultFields=['launch','commit','platform','channel','version','publication','url','qa'];
 function renderPendingResult(){const pending=journal('result').get();for(const field of resultFields)$('result-'+field).disabled=Boolean(pending);if(pending){for(const field of resultFields)$('result-'+field).value=pending[field==='launch'?'launch_id':field==='qa'?'what_to_check':field];}$('result-save').textContent=pending?'Retry exact saved report':'Record result';$('result-clear').hidden=!pending;}
 async function saveStatus(event){event.preventDefault();if(writing||busy||!project||!online())return;const mine=who,e=epoch;writing=true;$('project-select').disabled=true;const j=journal('status');
  try{let request=j.get();if(!request){request={operation_id:crypto.randomUUID(),project_id:project.id,expected_revision:project.revision,draft:{...project.draft,status:$('project-summary').value,next_step:$('project-next').value}};validateStatus(request.draft);j.put(request);}const p=await post('/projects',request);if(!valid(mine,e))return;j.clear();drafts.delete(project.id);project=p;status('Project status saved.');}
  catch(error){if(valid(mine,e))status(`Status not confirmed: ${error.message}. Retry the saved request or clear it to edit.`);}finally{writing=false;$('project-select').disabled=false;if(valid(mine,e))await refresh();}
 }
 async function saveResult(event){event.preventDefault();if(writing||busy||!project||!online())return;const mine=who,e=epoch;writing=true;$('project-select').disabled=true;const j=journal('result');
  try{let request=j.get();if(!request){const op=launches.find(o=>o.request.operation_id===$('result-launch').value);if(!op?.run_id)throw new Error('Choose a completed executor run.');const commit=$('result-commit').value.trim();if(!/^[a-fA-F0-9]{40}$/.test(commit)||!safeResultURL($('result-url').value))throw new Error('Use a full commit SHA and an HTTPS result link.');request={operation_id:crypto.randomUUID(),project_id:project.id,launch_id:op.request.operation_id,run_id:op.run_id,commit,platform:$('result-platform').value.trim(),channel:$('result-channel').value.trim(),version:$('result-version').value.trim(),publication:$('result-publication').value,url:$('result-url').value,what_to_check:$('result-qa').value};validateResult(request);j.put(request);}await post('/results',request);if(!valid(mine,e))return;j.clear();$('result-status').textContent='Result recorded. This reports publication; it does not publish anything.';}
  catch(error){if(valid(mine,e))$('result-status').textContent=error.message;}finally{writing=false;$('project-select').disabled=false;if(valid(mine,e))await refresh();}
 }
 $('project-refresh').onclick=refresh;$('project-select').onchange=()=>{if(writing)return;epoch++;project=null;notice='';for(const id of ['result-commit','result-platform','result-channel','result-version','result-url','result-qa'])$(id).value='';$('result-status').textContent='';for(const id of ['project-questions','project-executors','project-results','project-repos'])$(id).replaceChildren();refresh();};
 for(const id of ['project-summary','project-next'])$(id).oninput=()=>{if(project)drafts.set(project.id,{status:$('project-summary').value,next_step:$('project-next').value});};
 $('project-form').onsubmit=saveStatus;$('result-form').onsubmit=saveResult;
 $('project-clear-status').onclick=()=>{if(writing||!project)return;drafts.set(project.id,{status:$('project-summary').value,next_step:$('project-next').value});journal('status').clear();for(const id of ['project-summary','project-next'])$(id).disabled=false;$('project-clear-status').hidden=true;$('project-save').textContent='Save status';status('Saved request cleared; text remains. Refresh the current revision before saving again.');};
 $('result-clear').onclick=()=>{if(writing||!project)return;journal('result').clear();renderPendingResult();};
 return{refresh,reset};
}
