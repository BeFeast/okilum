import {icon,age} from './ui.js';
const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!=null)n.textContent=text;if(cls)n.className=cls;return n;};
export function sourceURL(value,instance){try{const u=new URL(value),base=new URL(instance);return u.protocol==='https:'&&u.origin===base.origin&&!u.username&&!u.password?u.href:null;}catch{return null;}}
export function freshness(row){const time=row.synced_at??row.discovered_at;return `${row.stale||row.error?'Stale / unavailable':'Synced'} · ${time?new Date(time*1000).toLocaleString():'never'}${row.error?` · ${row.error}`:''}`;}
function link(label,url,instance){const a=el('a',label),safe=sourceURL(url,instance);if(safe){a.href=safe;a.target='_blank';a.rel='noopener noreferrer';}return a;}
export function workItems(repo){return [...repo.issues.map(v=>({...v,kind:'issue'})),...repo.pulls.map(v=>({...v,kind:'pull'})),...repo.releases.map(v=>({...v,kind:'release',title:v.name||v.tag,updated_at:v.published_at}))];}
// Attempts remain visible in the detail. A mixed check history is never flattened to an invented pass.
export function checkBadge(checks=[]){if(!checks.length)return null;const states=new Set(checks.map(c=>c.state));if((states.has('failure')||states.has('error'))&&states.size>1)return{label:'CI mixed',tone:'failure'};if(states.has('failure')||states.has('error'))return{label:'CI failed',tone:'failure'};if(states.size===1&&states.has('success'))return{label:'CI passed',tone:'success'};return{label:'CI pending',tone:''};}
function detail(item,repo,data){
 const dialog=document.getElementById('overview-detail'),body=document.getElementById('overview-detail-body');body.replaceChildren();
 body.append(el('p',`${repo.name} / ${item.kind==='release'?'Release':`#${item.number}`}`,'eyebrow'),el('h2',item.title));
 const props=el('dl',null,'detail-props');
 for(const [label,value] of [['Type',{issue:'Issue',pull:'Pull request',release:'Release'}[item.kind]],['State',item.state||(item.prerelease?'Pre-release':'Published')],['Assignees',(item.assignees||[]).join(', ')||'Unassigned'],['Updated',item.updated_at?new Date(item.updated_at).toLocaleString():'Not supplied'],['Repository',repo.name]])props.append(el('dt',label),el('dd',value));body.append(props);
 if(item.head_commit){const d=el('details');d.append(el('summary','Source commit'),el('pre',item.head_commit));body.append(d);}
 if(item.checks?.length){body.append(el('h3','Checks on this PR head'));for(const c of item.checks){const row=el('div',null,'check-row');row.append(el('span',c.context),el('span',c.state,`badge ${c.state==='success'?'success':c.state==='failure'?'failure':''}`));row.title=c.updated_at;body.append(row);}}
 for(const asset of item.assets||[])body.append(link(asset.name,asset.url,data.instance));
 const original=link('Open in Forgejo',item.url,data.instance);original.className='source-button';original.append(icon('arrow'));body.append(original,el('p',freshness(repo),'sync-stamp'));
 document.getElementById('overview-close').onclick=()=>dialog.close();if(!dialog.open)dialog.showModal();
}
export function repositoryCards(data,{kind='all',query=''}={}){
 return data.repos.flatMap(repo=>{
  const items=workItems(repo).filter(item=>(kind==='all'||item.kind===kind)&&`${repo.name} ${item.title} ${item.number||''}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()));
  if(!items.length)return[];
  const group=el('section',null,'repo-group'),heading=el('div',null,'repo-heading');
  heading.append(el('span',repo.name.split('/').at(-1).slice(0,1).toUpperCase(),'repo-letter'),el('span',repo.name),el('span',String(items.length),'repo-count'));
  const original=link('',repo.url,data.instance);original.append(icon('arrow'));original.title=`Open ${repo.name}`;original.setAttribute('aria-label',original.title);heading.append(original);group.append(heading);
  if(repo.stale||repo.error)group.append(el('p',freshness(repo),'repo-warning'));
  for(const item of items){const row=el('button',null,'work-row');row.type='button';row.dataset.kind=item.kind;
   row.append(icon(item.kind),el('span',item.kind==='release'?'REL':`#${item.number}`,'work-number'),el('span',item.title,'work-title'));
   const check=item.kind==='pull'?checkBadge(item.checks):null;
   if(check)row.append(el('span',check.label,`badge ${check.tone}`));
   if(item.kind==='release')row.append(el('span',item.prerelease?'Beta':'Release','badge success'));
   if(item.assignees?.length){const a=el('span',item.assignees[0].slice(0,2).toUpperCase(),'avatar');a.title=`Assignees: ${item.assignees.join(', ')}`;row.append(a);}
   const time=el('span',age(item.updated_at),'work-age');time.title=item.updated_at||'Updated time unavailable';row.append(time);row.onclick=()=>detail(item,repo,data);group.append(row);
  }
  if(!items.length)group.append(el('p','No open work or published releases.','repo-empty'));
  return[group];
 });
}
export function mountForgejo({api,owner}){
 const $=id=>document.getElementById(id);let epoch=0,busy=false,again=false,who=null,data=null,kind='all';
 function reset(){epoch++;who=null;data=null;kind='all';$('forgejo-panel').hidden=true;$('forgejo-list').replaceChildren();$('forgejo-status').textContent='';$('overview-search').value='';$('overview-detail').close();$('overview-detail-body').replaceChildren();$('forgejo-project').replaceChildren(el('option','All projects'));$('forgejo-project').firstChild.value='';}
 function render(){document.querySelectorAll('[data-kind]').forEach(b=>{if(b.matches('button[aria-pressed]'))b.setAttribute('aria-pressed',String(b.dataset.kind===kind));});if(!data)return;const nodes=repositoryCards(data,{kind,query:$('overview-search').value.trim()});$('forgejo-list').replaceChildren(...nodes);if(!nodes.length)$('forgejo-list').append(el('p','No work matches these filters.','empty'));}
 async function refresh(){if(!owner()){reset();return;}if(busy){again=true;return;}if(who!==owner()){reset();who=owner();}const mine=who,e=epoch;busy=true;
  try{const projects=[];let after='';for(let page=0;page<100;page++){const p=await api(`/projects?after=${encodeURIComponent(after)}&limit=100`);if(e!==epoch||mine!==owner())return;projects.push(...p.projects);if(p.projects.length<100)break;if(!p.next_after||p.next_after===after||page===99)throw Error('Project list is incomplete.');after=p.next_after;}
   const selected=$('forgejo-project').value;$('forgejo-project').replaceChildren(...[{id:'',draft:{title:'All projects'}},...projects].map(p=>{const o=el('option',p.draft.title);o.value=p.id;return o;}));if(projects.some(p=>p.id===selected))$('forgejo-project').value=selected;
   const project=$('forgejo-project').value,next=await api(`/forgejo${project?'?project='+encodeURIComponent(project):''}`);if(e!==epoch||mine!==owner()||project!==$('forgejo-project').value)return;
   $('forgejo-panel').hidden=!next.enabled;if(!next.enabled){data=null;$('forgejo-list').replaceChildren();return;}data=next;
   $('forgejo-status').textContent=`${freshness(data)} · ${data.repos.length} repositories`;$('forgejo-status').title=`${data.instance} · ${data.account_login||'Account not verified'}`;render();
  }catch(error){if(e===epoch&&mine===owner()){$('forgejo-panel').hidden=false;$('forgejo-status').textContent=`Unavailable · ${error.message}. Previous observations may be stale.`;}}
  finally{busy=false;if(again){again=false;await refresh();}}
 }
 $('forgejo-refresh').onclick=refresh;$('forgejo-project').onchange=()=>{epoch++;data=null;$('forgejo-list').replaceChildren();$('overview-detail').close();refresh();};
 $('overview-search').oninput=render;document.querySelectorAll('.filter-chips [data-kind]').forEach(b=>b.onclick=()=>{kind=b.dataset.kind;render();});
 return{reset,refresh};
}
