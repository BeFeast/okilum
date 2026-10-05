import { buildReply, questionStatus, replyJournal } from './questions.js';

export function mountQuestions({ api, post, owner, online, storage = localStorage }) {
  const $ = id => document.getElementById(id);
  let generation = 0, busy = false, sending = false, selected = null, loadedAt = 0;
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
    rows = found; renderList(); status(`${rows.size} question${rows.size === 1 ? '' : 's'} · checked ${new Date().toLocaleTimeString()}`);
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
    selected = null; operation = null; pending = null; loadedAt = 0;
    $('executor-answer-fields').replaceChildren(); $('executor-send').disabled = true;
    $('executor-retry').hidden = true; $('executor-discard').hidden = true;
    $('executor-answer-status').textContent = 'Checking the original question…';
    if (!$('executor-dialog').open) $('executor-dialog').showModal();
    try {
      const q = await api(`/questions/${encodeURIComponent(id)}`);
      if (!validSession(who,epoch)) return;
      selected = q; loadedAt = Date.now(); pending = journal().get(q.id);
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
  function renderAnswer() {
    const q = selected; if (!q) return;
    $('executor-answer-fields').replaceChildren();
    const saved = operation?.request || pending;
    const display = saved || { answers: drafts.get(q.id) || [] };
    for (const field of q.fields) {
      const group = el('fieldset'); group.dataset.fieldId = field.id;
      group.append(el('legend',field.prompt));
      for (const option of field.options) {
        const label = el('label',null,'answer-option'), input = el('input');
        input.type = field.multiple ? 'checkbox' : 'radio'; input.name = `answer-${field.id}`;
        input.value = option.id;
        input.checked = display.answers.find(a => a.id === field.id)?.option_ids.includes(option.id) || false;
        input.disabled = Boolean(saved) || !q.can_reply || !q.source_fresh;
        label.append(input,el('span',option.label)); group.append(label);
      }
      if (field.allow_text) {
        const label = el('label','Or write your answer'), input = el('textarea'); input.rows = 3;
        input.value = display.answers.find(a=>a.id === field.id)?.text || '';
        input.disabled = Boolean(saved) || !q.can_reply || !q.source_fresh;
        label.append(input); group.append(label);
      }
      $('executor-answer-fields').append(group);
    }
    $('executor-source').textContent = `${q.source.kind.toUpperCase()} · thread ${q.source.thread_id}`;
    $('executor-answer-status').textContent = pending && !operation ? 'The previous send is unconfirmed. Check status or retry the exact saved answer; no new answer will be created.' : questionStatus(q,operation,online());
    $('executor-send').disabled = !online() || !q.source_fresh || !q.can_reply || Boolean(saved) || sending;
    $('executor-retry').hidden = !pending || Boolean(operation);
    $('executor-retry').disabled = !online() || sending;
    $('executor-discard').hidden = operation?.state !== 'rejected';
  }
  function readAnswers() {
    return [...$('executor-answer-fields').children].map(group => ({
      id: group.dataset.fieldId, text: group.querySelector('textarea')?.value || '',
      option_ids: [...group.querySelectorAll('input:checked')].map(input=>input.value),
    }));
  }
  async function send(event) {
    event.preventDefault(); if (sending || !selected || !online()) return;
    const q = selected, who = currentOwner, epoch = generation;
    sending = true; $('executor-send').disabled = true; $('executor-retry').disabled = true;
    try {
      if (!pending) {
        if (Date.now() - loadedAt > 20000) throw new Error('This question needs a fresh check. Use Check status, then answer again.');
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
        $('executor-discard').hidden = false;
        $('executor-answer-status').textContent = 'The question changed or another answer was reserved. Your answer is preserved. Check status, or explicitly clear this refused draft and refresh.';
      } else $('executor-answer-status').textContent = error.message;
      $('executor-retry').hidden = !pending;
    } finally {
      sending = false;
      if (validSession(who,epoch)) {
        $('executor-retry').disabled = !online();
        $('executor-send').disabled = !selected?.can_reply || !selected.source_fresh || !online() || Boolean(pending) || Boolean(operation) || Date.now()-loadedAt>20000;
      }
    }
  }
  $('executor-refresh').onclick = refresh;
  $('executor-project').onchange = () => loadQuestions().catch(error=>status(error.message));
  $('executor-close').onclick = () => { if (!sending) { generation++; $('executor-dialog').close(); selected=null; } };
  $('executor-dialog').addEventListener('cancel',event=> { if(sending) event.preventDefault(); else { generation++; selected=null; } });
  $('executor-answer-form').onsubmit = send;
  $('executor-retry').onclick = send;
  $('executor-check').onclick = () => { if (selected && !sending) open(selected.id); };
  $('executor-discard').onclick = () => {
    if (!selected || sending) return;
    const saved = pending || operation?.request;
    if (saved) drafts.set(selected.id,saved.answers);
    journal().clear(selected.id); open(selected.id);
  };
  return { refresh, reset, expire() { if (selected && Date.now()-loadedAt>20000) $('executor-send').disabled=true; } };
}
