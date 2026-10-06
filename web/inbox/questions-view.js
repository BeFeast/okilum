import { buildReply, questionStatus, replyJournal, optionLabel, sourceName, operationLabel } from './questions.js';

export function mountQuestions({ api, post, owner, online, storage = localStorage }) {
  const $ = id => document.getElementById(id);
  let generation = 0, busy = false, sending = false, selected = null, loadedAt = 0, checking = false, refused = false, lastCheckAt = 0, refusalWarning = '';
  const drafts = new Map();
  let projects = [], rows = new Map(), operation = null, pending = null, currentOwner = null;
  const el = (tag, text, cls) => { const node = document.createElement(tag); if (text) node.textContent = text; if (cls) node.className = cls; return node; };
  const journal = () => replyJournal(storage, currentOwner);
  function status(message) { $('executor-status').textContent = message; }
  function reset() {
    generation++; selected = null; drafts.clear(); rows.clear(); projects = []; operation = null; pending = null;
    $('executor-questions').replaceChildren(); $('executor-project').replaceChildren();
    $('executor-answer-fields').replaceChildren(); $('executor-dialog').close();
    $('executor-panel').hidden = true; currentOwner = null;
  }
  function validSession(who, epoch) { return who === owner() && who === currentOwner && epoch === generation; }
  async function refresh() {
    if (!owner()) { reset(); return; }
    if (busy || sending) return;
    busy = true;
    if (currentOwner !== owner()) { reset(); currentOwner = owner(); }
    const who = currentOwner, epoch = generation;
    $('executor-panel').hidden = false;
    try {
      const found = []; let after = '';
      for (let page = 0; page < 100; page++) {
        const data = await api(`/projects?after=${encodeURIComponent(after)}&limit=100`);
        if (!validSession(who, epoch)) return;
        found.push(...data.projects);
        if (data.projects.length < 100) break;
        if (!data.next_after || data.next_after === after || page === 99) throw new Error('Project list is incomplete. Try refreshing.');
        after = data.next_after;
      }
      projects = found;
      const previous = $('executor-project').value;
      $('executor-project').replaceChildren(...projects.map(p => {
        const option = el('option', p.draft.title); option.value = p.id; return option;
      }));
      if (projects.some(p => p.id === previous)) $('executor-project').value = previous;
      if (!projects.length) { rows.clear(); renderList(); status('No execution projects connected yet.'); return; }
      await loadQuestions(who, epoch);
    } catch (error) { if (validSession(who,epoch)) status(error.message); }
    finally { busy = false; }
  }
  async function loadQuestions(who = currentOwner, epoch = generation) {
    const project = $('executor-project').value, found = new Map(); let after = '';
    for (let page = 0; page < 100; page++) {
      const data = await api(`/projects/${encodeURIComponent(project)}/questions?after=${encodeURIComponent(after)}&limit=100`);
      if (!validSession(who,epoch) || project !== $('executor-project').value) return;
      for (const q of data.questions) found.set(q.id,q);
      if (data.questions.length < 100) break;
      if (!data.next_after || data.next_after === after || page === 99) throw new Error('Question list is incomplete. Try refreshing.');
      after = data.next_after;
    }
    rows = found; renderList(); status(rows.size ? `${rows.size} question${rows.size === 1 ? '' : 's'}` : '');
  }
  function renderList() {
    $('executor-questions').replaceChildren();
    for (const q of rows.values()) {
      const li = el('li'), button = el('button', null, 'thought-row');
      button.append(el('span', q.fields.map(f => f.prompt).join(' · '), 'thought-preview'),
        el('span', `${q.source.kind.toUpperCase()} · ${questionStatus(q,null,online())}`, 'thought-meta'));
      button.onclick = () => open(q.id); li.append(button); $('executor-questions').append(li);
    }
  }
  async function open(id) {
    if (sending) return;
    if (selected && !pending && !operation) drafts.set(selected.id, readAnswers());
    const epoch = ++generation, who = currentOwner;
    selected = null; operation = null; pending = null; loadedAt = 0; refused = false; refusalWarning = '';
    $('executor-answer-fields').replaceChildren(); $('executor-approval').replaceChildren(); $('executor-approval').hidden = true; $('executor-send').disabled = true;
    $('executor-retry').hidden = true; $('executor-discard').hidden = true;
    $('executor-answer-status').textContent = 'Checking the original question…';
    if (!$('executor-dialog').open) $('executor-dialog').showModal();
    try {
      const q = await api(`/questions/${encodeURIComponent(id)}`);
      if (!validSession(who,epoch)) return;
      selected = q; loadedAt = Date.now(); pending = journal().get(q.id);
      refused = Boolean(pending && journal().isRefused(q.id,pending.operation_id));
      const operationId = q.pending_operation_id || pending?.operation_id;
      if (operationId) {
        try {
          const result = await api(`/reply-operations/${encodeURIComponent(operationId)}`);
          if (!validSession(who,epoch)) return;
          operation = result;
        }
        catch (error) { if (error.status !== 404) throw error; }
        if (!validSession(who,epoch)) return;
      }
      renderAnswer();
    } catch (error) { if (validSession(who,epoch)) $('executor-answer-status').textContent = error.message; }
  }
  function renderApproval(q) {
    const panel=$('executor-approval');panel.replaceChildren();panel.hidden=!q.approval;
    if(!q.approval)return;
    const a=q.approval,heading=el('div',null,'approval-heading');
    const action=a.action.split('_').map(w=>w==='pr'?'PR':w[0].toUpperCase()+w.slice(1)).join(' ');
    heading.append(el('span',action,'approval-action'),el('span',`${a.risk} risk`,'approval-risk'));
    const info=el('button',null,'icon-button');info.type='button';info.title='Exact action and target';info.setAttribute('aria-label','Exact action and target');info.setAttribute('aria-expanded','false');
    info.innerHTML='<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M12 11v6 M12 7h.01"/></svg>';
    const exact=el('pre',JSON.stringify({action:a.action,target:a.target,repo:a.repo,payload_hash:a.payload_hash,target_state_hash:a.target_state_hash},null,2),'approval-exact');exact.hidden=true;
    info.onclick=()=>{exact.hidden=!exact.hidden;info.setAttribute('aria-expanded',String(!exact.hidden));};heading.append(info);panel.append(heading);
    const target=el('dl',null,'approval-target');
    for(const [key,value] of Object.entries({...a.repo?{repo:a.repo}:{},...a.target})){
      if(value===null||value===''||/(?:^id$|_id$|hash|sha|path|session)/.test(key))continue;
      target.append(el('dt',key==='pr'?'PR':key.replaceAll('_',' ')),el('dd',typeof value==='object'?JSON.stringify(value):String(value)));
    }
    if(!target.children.length)target.append(el('dt','Target'),el('dd','See exact details'));
    panel.append(target,exact);
  }
  function renderAnswer() {
    const q = selected; if (!q) return;
    renderApproval(q);
    $('executor-dialog').querySelector('.section-label').textContent=q.approval?'Approval':'Question';
    $('executor-answer-fields').replaceChildren();
    const saved = operation?.request || pending;
    const display = saved || { answers: drafts.get(q.id) || [] };
    for (const field of q.fields) {
      const group = el('section',null,'answer-card'); group.dataset.fieldId = field.id;
      const heading = el('h3',field.prompt); heading.id = `question-field-${q.fields.indexOf(field)}`;
      group.setAttribute('role','group'); group.setAttribute('aria-labelledby',heading.id); group.append(heading);
      const chosen = display.answers.find(a=>a.id===field.id);
      const locked = Boolean(saved) || !q.can_reply || !q.source_fresh;
      for (const option of field.options) {
        const button = el('button',optionLabel(option.label),'answer-choice'); button.type='button';button.dataset.optionId=option.id;
        button.setAttribute('aria-pressed',String(chosen?.option_ids.includes(option.id)||false));button.disabled=locked;
        button.onclick=()=>{const was=button.getAttribute('aria-pressed')==='true';if(!field.multiple)group.querySelectorAll('[data-option-id]').forEach(b=>b.setAttribute('aria-pressed','false'));button.setAttribute('aria-pressed',String(!was));const custom=group.querySelector('.other-answer');if(custom){custom.value='';custom.hidden=true;group.querySelector('.other-choice').setAttribute('aria-expanded','false');}updateSend();};group.append(button);
      }
      if(field.allow_text){
        const other=el('button','Other…','answer-choice other-choice');other.type='button';other.disabled=locked;
        const input=el('input',null,'other-answer');input.type='text';input.setAttribute('aria-label','Your answer');input.placeholder='Your answer…';input.value=chosen?.text||'';input.hidden=!input.value;input.disabled=locked;other.setAttribute('aria-expanded',String(!input.hidden));
        other.onclick=()=>{group.querySelectorAll('[data-option-id]').forEach(b=>b.setAttribute('aria-pressed','false'));input.hidden=false;other.setAttribute('aria-expanded','true');input.focus();updateSend();};input.oninput=updateSend;group.append(other,input);
      }
      $('executor-answer-fields').append(group);
    }
    const projectName = projects.find(p => p.id === q.project_id)?.draft.title || 'Project';
    $('executor-source').textContent = `${projectName} · ${q.thread_title || `${sourceName(q)} conversation`}`;
    $('executor-source').title = `${q.source.kind.toUpperCase()} · ${q.source.thread_id}`;
    updateStatus();
  }
  function updateStatus(){
    if(!selected)return;
    const full=refused?`Not sent — the source refused this answer. Clear this refused draft to choose again. ${refusalWarning}`:pending&&!operation?'Delivery unconfirmed — retry the same saved answer':questionStatus(selected,operation,online());
    const short=refused?'Not sent':operation?operationLabel(selected,operation.state):pending?'Unconfirmed':selected.state==='answered'?'Answered':selected.state==='withdrawn'?'Withdrawn':!online()?'Offline':!selected.source_fresh?'Reconnecting':!selected.can_reply?'Unavailable':selected.approval?'Awaiting decision':'Awaiting answer';
    $('executor-answer-status').textContent=short;$('executor-answer-status').title=full;
    $('executor-retry').hidden=refused||!pending||Boolean(operation);$('executor-retry').disabled=!online()||sending;
    $('executor-discard').hidden=!refused&&operation?.state!=='rejected';updateSend();
  }
  function readAnswers() {
    return [...$('executor-answer-fields').children].map(group => ({
      id: group.dataset.fieldId, text: group.querySelector('.other-answer')?.value || '',
      option_ids: [...group.querySelectorAll('[data-option-id][aria-pressed=true]')].map(button=>button.dataset.optionId),
    }));
  }
  function updateSend(){
    let ready=false;
    if(selected)try{buildReply(selected,Object.fromEntries(readAnswers().map(a=>[a.id,a])),'preview');ready=true;}catch{}
    $('executor-send').textContent=selected?.approval?'Send decision':'Send';
    $('executor-send').disabled=!ready||!online()||sending||refused||Boolean(pending)||Boolean(operation)||Date.now()-loadedAt>20000;
  }
  async function checkSelected(){
    if(checking||sending||!selected||!$('executor-dialog').open||document.hidden||!online())return;
    checking=true;lastCheckAt=Date.now();const id=selected.id,who=currentOwner,epoch=generation;
    try{
      const q=await api(`/questions/${encodeURIComponent(id)}`);
      if(!validSession(who,epoch)||selected?.id!==id)return;
      const changed=q.source_revision!==selected.source_revision||JSON.stringify(q.fields)!==JSON.stringify(selected.fields);
      const before=Boolean(operation||pending)||!selected.can_reply||!selected.source_fresh;
      const operationId=q.pending_operation_id||pending?.operation_id;
      if(operationId){try{const result=await api(`/reply-operations/${encodeURIComponent(operationId)}`);if(!validSession(who,epoch)||selected?.id!==id)return;operation=result;}catch(error){if(error.status!==404)throw error;}}
      if(!validSession(who,epoch)||selected?.id!==id)return;
      if(changed&&!pending&&!operation)drafts.delete(id);
      else if(!pending&&!operation)drafts.set(id,readAnswers());
      selected=q;loadedAt=Date.now();
      const after=Boolean(operation||pending)||!q.can_reply||!q.source_fresh;
      if(changed||before!==after)renderAnswer();else updateStatus();
    }catch(error){if(validSession(who,epoch)){$('executor-answer-status').textContent='Reconnecting';$('executor-answer-status').title=error.message;}}
    finally{checking=false;}
  }
  async function send(event) {
    event.preventDefault(); if (sending || refused || !selected || !online()) return;
    const q = selected, who = currentOwner, epoch = generation;
    sending = true; $('executor-send').disabled = true; $('executor-retry').disabled = true;
    try {
      if (!pending) {
        if (Date.now() - loadedAt > 20000) throw new Error('Refreshing this question. Try again in a moment.');
        const values = Object.fromEntries(readAnswers().map(answer=>[answer.id,answer]));
        const body = buildReply(q,values,crypto.randomUUID());
        journal().put(body); pending = body; renderAnswer();
      }
      // One operation identity and immutable payload even when the response is lost.
      const result = await post(`/questions/${encodeURIComponent(q.id)}/reply`,pending);
      if (!validSession(who,epoch)) return;
      operation = result; renderAnswer();
    } catch (error) {
      if (!validSession(who,epoch)) return;
      // Only explicit 409 proves this new intent was refused; preserve text and
      // fetch current source before allowing a separately confirmed new answer.
      if (error.status === 409) {
        refused = true;
        try { journal().refuse(q.id,pending.operation_id); } catch { refusalWarning='Keep this page open until you clear the draft.'; }
        $('executor-discard').hidden = false;
        updateStatus();
      } else $('executor-answer-status').textContent = error.message;
      $('executor-retry').hidden = refused || !pending;
    } finally {
      sending = false;
      if (validSession(who,epoch)) {
        $('executor-retry').disabled = !online();
        updateSend();
      }
    }
  }
  $('executor-refresh').onclick = refresh;
  $('executor-project').onchange = () => loadQuestions().catch(error=>status(error.message));
  $('executor-close').onclick = () => { if (!sending) { generation++; $('executor-dialog').close(); selected=null; } };
  $('executor-dialog').addEventListener('cancel',event=> { if(sending) event.preventDefault(); else { generation++; selected=null; } });
  $('executor-answer-form').onsubmit = send;
  $('executor-retry').onclick = send;
  $('executor-discard').onclick = () => {
    if (!selected || sending) return;
    const saved = pending || operation?.request;
    if (saved) drafts.set(selected.id,saved.answers);
    journal().clear(selected.id); open(selected.id);
  };
  return { refresh, reset, open, expire() { if(selected){updateSend();if(Date.now()-Math.max(loadedAt,lastCheckAt)>10000)checkSelected();} } };
}
