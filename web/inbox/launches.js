// Explicit launches are online actions, never capture outbox work.
export function launchRequest(brief, target, operationId) {
  if (brief.target_id !== target.snapshot.target.id || brief.project_id !== target.snapshot.project_id) throw new Error('Refresh the launch target.');
  return { operation_id: operationId, brief_id: brief.id, expected_revision: brief.revision, target_revision: target.revision };
}
export function launchLabel(op) {
  return ({queued:'Queued for T3',uncertain:'Launch unconfirmed — checking the original thread; no automatic relaunch',accepted:'Accepted by T3; waiting for preparation',preparing:'T3 is preparing the worktree',running:'Executor running',completed:'Executor completed',failed:'Executor failed or was interrupted'})[op.state] || 'Unknown launch status';
}
export function launchStorage(storage, owner) {
  const key = `tessera-launch-v1:${owner}`;
  return { get:()=>JSON.parse(storage.getItem(key)||'null'), put:value=>storage.setItem(key,JSON.stringify(value)), clear:()=>storage.removeItem(key) };
}
export function mountLaunches({api,post,owner,online,storage=localStorage}) {
  const $=id=>document.getElementById(id);
  const el=(tag,text)=>{const n=document.createElement(tag);if(text)n.textContent=text;return n;};
  let who=null,epoch=0,busy=false,targets=[],record=null,operation=null,notice='';
  const journal=()=>launchStorage(storage,who);
  const status=text=>{notice=text;$('launch-status').textContent=text;};
  function reset(){who=null;epoch++;notice='';targets=[];$('launch-retry').hidden=true;record=null;operation=null;$('launch-panel').hidden=true;$('launch-list').replaceChildren();$('launch-preview').textContent='';$('launch-title').value='';$('launch-text').value='';}
  function render(){
    const locked=Boolean(record), saved=record?.brief;
    for(const id of ['launch-project','launch-target','launch-title','launch-text'])$(id).disabled=locked||busy;
    $('launch-save').disabled=locked||busy||!online()||!targets.length;
    $('launch-confirm').hidden=!saved||Boolean(record?.launch)||Boolean(operation);
    $('launch-confirm').disabled=busy||!online();
    $('launch-check').hidden=!record;
    $('launch-new').hidden=Boolean(record?.launch)&&!['completed','failed'].includes(operation?.state);
    $('launch-new').disabled=busy;
    if(record){
      $('launch-title').value=record.save.title;$('launch-text').value=record.save.text;
      const t=record.target.snapshot.target;
      $('launch-preview').textContent=`${record.save.title}\n\n${record.save.text}\n\nRepository: ${t.repository}\nBase commit: ${t.base_commit}\nWorkspace: new isolated worktree prepared by T3\nExecutor: ${t.model_selection.instanceId} / ${t.model_selection.model}\nModel options: ${JSON.stringify(t.model_selection.options ?? {})}\nMode: ${t.runtime_mode} / ${t.interaction_mode}\nBrief revision: ${saved?.revision ?? 'saving…'}`;
    }else $('launch-preview').textContent='Preview the saved brief and launch settings before starting an executor.';
    if(notice)$('launch-status').textContent=notice;
    else if(operation)$('launch-status').textContent=launchLabel(operation);
    else if(record?.launch)status('Launch request saved. Check status; an unknown result never creates a replacement launch.');
  }
  async function refresh(){
    if(!owner()){reset();return;}if(busy)return;
    if(who!==owner()){reset();who=owner();record=journal().get();}
    const mine=who,e=epoch;busy=true;notice='';$('launch-panel').hidden=false;
    try{
      const projects={projects:[]};let after='';
      for(let page=0;page<100;page++){
        const data=await api(`/projects?after=${encodeURIComponent(after)}&limit=100`);
        if(mine!==owner()||e!==epoch)return;
        projects.projects.push(...data.projects);if(data.projects.length<100)break;
        if(!data.next_after||data.next_after===after||page===99)throw new Error('Project list is incomplete.');after=data.next_after;
      }
      const previous=$('launch-project').value;
      $('launch-project').replaceChildren(...projects.projects.map(p=>{const o=el('option',p.draft.title);o.value=p.id;return o;}));
      if(record)$('launch-project').value=record.save.project_id;
      else if(projects.projects.some(p=>p.id===previous))$('launch-project').value=previous;
      await loadTargets(mine,e);
      if(record)await recover(mine,e);
      await list(mine,e);
    }catch(error){if(mine===owner()&&e===epoch)status(error.message);}
    finally{busy=false;if(mine===owner()&&e===epoch)render();}
  }
  async function loadTargets(mine=who,e=epoch){
    const project=$('launch-project').value;if(!project){targets=[];return;}
    const data=await api(`/projects/${encodeURIComponent(project)}/launch-targets`);if(mine!==owner()||e!==epoch)return;
    targets=data.targets;$('launch-target').replaceChildren(...targets.map((t,i)=>{const o=el('option',t.snapshot.target.label);o.value=String(i);return o;}));
    if(!targets.length)status('No executor launch target is enabled for this project.');
  }
  async function list(mine=who,e=epoch){
    const project=$('launch-project').value;if(!project)return;
    let after=0;const all=[];
    for(let page=0;page<100;page++){
      const data=await api(`/projects/${encodeURIComponent(project)}/launches?after=${after}&limit=100`);
      if(mine!==owner()||e!==epoch||project!==$('launch-project').value)return;
      all.push(...data.operations);if(!data.has_more)break;
      if(data.next_cursor<=after||page===99)throw new Error('Launch history is incomplete.');after=data.next_cursor;
    }
    $('launch-list').replaceChildren(...all.reverse().map(op=>{
      const li=el('li');li.append(el('strong',op.brief.title),el('p',launchLabel(op)),Object.assign(el('p','T3 executor'),{title:op.thread_id}));
      if(op.worktree_path)li.append(el('p',`Worktree: ${op.worktree_path}`));return li;
    }));
  }
  async function recover(mine=who,e=epoch){
    if(!record)return;
    if(record.launch){
      try{const result=await api(`/launches/${record.launch.operation_id}`);if(mine!==owner()||e!==epoch)return;operation=result;$('launch-retry').hidden=true;}
      catch(error){if(error.status!==404)throw error;operation=null;status('No launch receipt found. Retry the exact saved request; do not create a replacement.');$('launch-retry').hidden=false;}
    }else if(!record.brief){
      const brief=await post('/briefs',record.save);if(mine!==owner()||e!==epoch)return;
      record.brief=brief;journal().put(record);
    }
  }
  async function save(event){
    event.preventDefault();if(busy||record||!online())return;
    const t=targets[Number($('launch-target').value)];if(!t)return;
    const title=$('launch-title').value.trim(),text=$('launch-text').value;
    if(!title||!text.trim()){status('Add a title and the exact brief text.');return;}
    if(new TextEncoder().encode(title).length>256||new TextEncoder().encode(text).length>65536){status('Title limit: 256 bytes; brief limit: 64 KB.');return;}
    const next={target:t,save:{operation_id:crypto.randomUUID(),project_id:$('launch-project').value,brief_id:crypto.randomUUID(),expected_revision:0,title,text,target_id:t.snapshot.target.id}};
    const mine=who,e=epoch;busy=true;notice='';
    try{journal().put(next);record=next;render();await recover(mine,e);if(mine===owner()&&e===epoch)status('Review the exact brief, repository, base and executor below. Saving has not launched anything.');}
    catch(error){if(mine===owner()&&e===epoch)status(error.message);}
    finally{busy=false;if(mine===owner()&&e===epoch)render();}
  }
  async function launch(retry=false){
    if(busy||!online()||!record?.brief)return;
    const mine=who,e=epoch;busy=true;notice='';
    try{
      if(!record.launch){
        const data=await api(`/projects/${record.brief.project_id}/launch-targets`);
        if(mine!==owner()||e!==epoch)return;
        if(!data.targets.some(t=>t.revision===record.target.revision))throw new Error('Launch settings changed. Start a new brief preview.');
        const next={...record,launch:launchRequest(record.brief,record.target,crypto.randomUUID())};
        journal().put(next);record=next;
      }else if(!retry)return;
      render();
      const result=await post('/launches',record.launch);
      if(mine!==owner()||e!==epoch)return;operation=result;$('launch-retry').hidden=true;
      await list(mine,e);
    }catch(error){if(mine!==owner()||e!==epoch)return;status(error.message);$('launch-retry').hidden=!record?.launch;}
    finally{busy=false;if(mine===owner()&&e===epoch)render();}
  }
  $('launch-form').onsubmit=save;
  $('launch-confirm').onclick=()=>launch();$('launch-retry').onclick=()=>launch(true);
  $('launch-check').onclick=refresh;$('launch-refresh').onclick=refresh;
  $('launch-project').onchange=()=>{epoch++;loadTargets().then(()=>list()).then(render).catch(error=>status(error.message));};
  $('launch-new').onclick=()=>{if(busy||record?.launch&&!['completed','failed'].includes(operation?.state))return;journal().clear();record=null;operation=null;$('launch-retry').hidden=true;status('Edit the brief, then save a new preview.');render();};
  return {refresh,reset};
}
