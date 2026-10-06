// Forgejo is the source of truth. This panel has no mutation or dispatch actions.
export function freshness(row) {
  const time=row.synced_at ?? row.discovered_at;
  const stamp=time ? new Date(time*1000).toLocaleString() : 'never';
  return `${row.stale || row.error ? 'Stale / unavailable' : 'Synced'} · last complete sync: ${stamp}${row.error ? ` · ${row.error}` : ''}`;
}
export function sourceURL(value,instance) {
  try {const u=new URL(value),base=new URL(instance);return u.protocol==='https:'&&u.origin===base.origin&&!u.username&&!u.password?u.href:null;}catch{return null;}
}
export function mountForgejo({api,owner}) {
  const $=id=>document.getElementById(id), el=(tag,text)=>{const n=document.createElement(tag);n.textContent=text;return n;};
  let epoch=0,busy=false,who=null;
  function reset(){epoch++;who=null;$('forgejo-panel').hidden=true;$('forgejo-list').replaceChildren();$('forgejo-status').textContent='';$('forgejo-project').replaceChildren(el('option','All accessible repositories'));$('forgejo-project').firstChild.value='';}
  const link=(label,url,instance)=>{const a=el('a',label),safe=sourceURL(url,instance);if(safe){a.href=safe;a.target='_blank';a.rel='noopener noreferrer';}return a;};
  async function refresh(){
    if(!owner()){reset();return;}if(busy)return;
    if(who!==owner()){reset();who=owner();}
    const mine=who,e=epoch;busy=true;
    try{
      const projects=[];let after='';
      for(let page=0;page<100;page++){
        const p=await api(`/projects?after=${encodeURIComponent(after)}&limit=100`);
        if(e!==epoch||mine!==owner())return;projects.push(...p.projects);
        if(p.projects.length<100)break;
        if(!p.next_after||p.next_after===after||page===99)throw new Error('Project list is incomplete.');after=p.next_after;
      }
      const selected=$('forgejo-project').value;
      $('forgejo-project').replaceChildren(...[{id:'',draft:{title:'All accessible repositories'}},...projects].map(p=>{const o=el('option',p.draft.title);o.value=p.id;return o;}));
      if(projects.some(p=>p.id===selected))$('forgejo-project').value=selected;
      const project=$('forgejo-project').value;
      const data=await api(`/forgejo${project?'?project='+encodeURIComponent(project):''}`);
      if(e!==epoch||mine!==owner()||project!==$('forgejo-project').value)return;
      $('forgejo-panel').hidden=!data.enabled;
      if(!data.enabled)return;
      $('forgejo-status').textContent=`${data.instance} · ${data.account_login??'Account not verified'} · ${freshness(data)}. Open issues and PRs; published releases. Assignees are not executor status.`;
      const nodes=data.repos.map(repo=>{
        const card=el('details',''),summary=el('summary',`${repo.name} · ${repo.issues.length} issues · ${repo.pulls.length} PRs`);card.append(summary,el('p',freshness(repo)),link('Open repository',repo.url,data.instance));
        for(const issue of repo.issues){const p=el('p','');p.append(link(`#${issue.number} ${issue.title}`,issue.url,data.instance),el('span',` · assignees: ${issue.assignees.join(', ')||'none'}`));card.append(p);}
        for(const pr of repo.pulls){const p=el('p','');p.append(link(`PR #${pr.number} ${pr.title}`,pr.url,data.instance),el('span',` · head ${pr.head_commit.slice(0,12)} · assignees: ${pr.assignees.join(', ')||'none'}`));card.append(p);
          // Status endpoints may contain multiple attempts: show each with time, never flatten to an invented pass.
          for(const c of pr.checks)card.append(el('p',`${c.context}: ${c.state} · ${c.updated_at}`));
        }
        for(const release of repo.releases){const p=el('p','');p.append(link(`${release.prerelease?'Pre-release':'Release'} ${release.tag}`,release.url,data.instance),el('span',` · ${release.published_at}`));card.append(p);for(const a of release.assets)card.append(link(a.name,a.url,data.instance));}
        for(const run of (data.execution_links??[]).filter(r=>r.repo_id===repo.id))card.append(el('p',`Linked executor: ${run.state} · thread ${run.thread_id} · base ${run.base_commit.slice(0,12)} (not current HEAD)`));
        return card;
      });
      $('forgejo-list').replaceChildren(...nodes);
      if(!nodes.length)$('forgejo-list').append(el('p',project?'No repositories are explicitly linked to this project.':'No repository observations available.'));
    }catch(error){if(e===epoch&&mine===owner()){$('forgejo-panel').hidden=false;$('forgejo-status').textContent=`Unavailable: ${error.message}. Previously shown observations may be stale.`;}}
    finally{busy=false;}
  }
  $('forgejo-refresh').onclick=refresh;$('forgejo-project').onchange=()=>{$('forgejo-list').replaceChildren();refresh();};
  return{reset,refresh};
}
