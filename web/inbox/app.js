import { mountSyncPairing } from './sync-pairing.js';
import { mountDevices } from './devices.js';
import { mountShell } from './ui.js';
mountShell();
import { mountProjects } from './projects.js';
import { mountForgejo } from './forgejo.js';
import { mountLaunches } from './launches.js';
import { mountQuestions } from './questions-view.js';
import { publicationProblem, publicationProblems, markdownFilename, suggestedFilename, publicationLabel } from './publication-form.js';
import { openOutbox, flushOutbox } from './outbox.js';
import { creationOptions, requestOptions, credentialJSON } from './webauthn.js';

const $ = id => document.getElementById(id);
const syncRequestId = new URLSearchParams(location.hash.slice(1)).get('sync');
const deviceToken = new URLSearchParams(location.hash.slice(1)).get('device');
let enrollmentToken = new URLSearchParams(location.hash.slice(1)).get('enroll');
if (location.hash) history.replaceState(null, '', location.pathname); // Never retain bootstrap in history.
let outbox, rememberedOwner = null, sessionOwner = null, online = navigator.onLine;
let filenameSuggestion = '', publicationAttempted = false;
let selectedItem = null, discussionBusy = false, publicationBusy = false;
let publicationFolders = [], publicationReady = false, discussionRendered = null, publicationRecovery = false, publicationConflict = false;
let syncing = false, captures = new Map(), page = null, synchronizedThrough = 0;
const syncPairingUI = mountSyncPairing({api,post,owner:()=>sessionOwner,requestId:syncRequestId,signIn:()=>signIn(false)});
const devicesUI = mountDevices({api,post,owner:()=>sessionOwner,joinToken:deviceToken,signIn:()=>signIn(false)});
const forgejoUI = mountForgejo({api, owner: () => sessionOwner});
const launchesUI = mountLaunches({api, post, owner: () => sessionOwner, online: () => online && navigator.onLine});
const questionsUI = mountQuestions({api, post, owner: () => sessionOwner, online: () => online && navigator.onLine});
const projectsUI = mountProjects({api,post,owner:()=>sessionOwner,online:()=>online&&navigator.onLine,openQuestion:id=>questionsUI.open(id)});
function clearRemote() { syncPairingUI.reset(); devicesUI.reset(); projectsUI.reset(); forgejoUI.reset(); launchesUI.reset(); questionsUI.reset(); captures.clear(); page = null; synchronizedThrough = 0; selectedItem = null; $('discussion').hidden = true; $('publication-sheet').close(); }
function notice(message) { $('notice').textContent = message; $('notice').hidden = !message; }
function message(error) {
  if (error.name === 'NotAllowedError') return 'Passkey request cancelled or unavailable. You can try again.';
  if (error.status === 401) return 'Sign in to sync. Your unsent thoughts are still on this device.';
  if (error.status === 429) return 'Too many sign-in attempts. Wait a minute, then try again.';
  if (error instanceof TypeError) return 'Cannot reach Inbox. Unsent thoughts stay on this device.';
  return error.message || 'Something went wrong. Your local thoughts have not been removed.';
}
async function api(path, options = {}) {
  let response;
  try {
    response = await fetch(`/api/v1${path}`, { credentials: 'same-origin', cache: 'no-store',
      ...options, headers: { 'Content-Type': 'application/json', ...options.headers } });
  } catch (error) { online = false; throw error; }
  online = true;
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    if (response.status === 401) { sessionOwner = null; clearRemote(); }
    const errors = {
      sync_not_configured: 'Folder sync is not configured on this service.',
      sync_request_unavailable: 'This sync request expired or was cancelled. Start again in Okilum.',
      invalid_sync_request: 'The sync request does not match this computer or vault.',
      sync_identity_conflict: 'This computer already has a registration or the request changed. Wait for pending removal to finish before starting a fresh approval.',
      sync_rate_limited: 'Too many pairing requests. Try again later.',
      sync_capacity_reached: 'This service cannot add more computers. Contact its owner.',
      invalid_passkey_name: 'Enter a name of up to 100 characters.',
      last_passkey: 'Add another passkey before revoking this one.',
      recent_authentication_required: 'Confirm with your passkey, then try again.',
      publication_conflict: 'This path is occupied or changed. Nothing was overwritten. Keep the saved request for checking, or explicitly forget it and choose another filename.',
      vault_unavailable: 'Fixture vault is unavailable. The publication is not confirmed; retry the same request later.',
      invalid_publication: 'Choose an allowed folder, a relative Markdown filename and up to 64 KB of Markdown.',
      ai_not_configured: 'AI is not configured on this server yet. Your question is kept locally.',
      ai_busy: 'AI is busy. Retry this question shortly.',
      discussion_running: 'Another question is still running for this thought. Retry after it finishes.',
      discussion_limit: 'This conversation reached its context limit. No request was sent to AI.',
    };
    const error = new Error(errors[data.error] || (response.status === 409 ? 'This thought has a delivery conflict. Export it before taking further action.' : 'Inbox could not complete this request.'));
    error.status = response.status;
    throw error;
  }
  return data;
}
function post(path, body) { return api(path, { method: 'POST', body: JSON.stringify(body) }); }
async function verifySession() {
  const identity = await api('/session');
  if (typeof identity.owner_id !== 'string' || !identity.owner_id) throw new Error('Inbox did not confirm the account.');
  if (rememberedOwner !== identity.owner_id) clearRemote();
  sessionOwner = identity.owner_id;
  rememberedOwner = identity.owner_id;
  await outbox.setOwner(rememberedOwner);
  await syncPairingUI.ready();
}
function showDetail(text, status, itemId, time) {
  publicationAttempted = false; $('publication-sheet').close();
  selectedItem = itemId || null;
  discussionRendered = null;
  $('question').value = '';
  $('exchanges').replaceChildren();
  $('discussion-status').textContent = '';
  $('discussion').hidden = !selectedItem || !sessionOwner;
  publicationFolders = []; publicationReady = false; publicationRecovery = false; publicationConflict = false;
  $('destination').replaceChildren(); $('destination').disabled = true;
  $('draft').readOnly = false; $('filename').readOnly = false; $('forget-publication').hidden = true;
  $('draft').value = text; filenameSuggestion = suggestedFilename(text); $('filename').value = filenameSuggestion; $('publications').replaceChildren();
  updatePublicationForm();
  $('publication-status').textContent = '';
  if (selectedItem && sessionOwner) refreshPublication(true).catch(error => { $('publication-status').textContent = message(error); });
  $('detail-text').textContent = text;
  $('detail-status').textContent = time ? new Date(time).toLocaleString(undefined,{month:'short',day:'numeric',hour:'numeric',minute:'2-digit'}) : status;
  $('open-publication').hidden = !selectedItem || !sessionOwner;
  $('detail').showModal();
  if (selectedItem && sessionOwner) refreshDiscussion().catch(error => { $('discussion-status').textContent = message(error); });
}
async function render() {
  const local = outbox ? await outbox.forOwner(rememberedOwner) : [];
  $('login').hidden = Boolean(sessionOwner);
  $('logout').hidden = !sessionOwner;
  $('enrollment').hidden = !enrollmentToken;
  $('connection').textContent = !online ? 'Offline · saved locally' : sessionOwner ? 'Connected' : 'Sign in to sync';
  $('save').disabled = !outbox || !rememberedOwner;
  $('sync').disabled = !outbox || syncing;
  $('export').hidden = local.length === 0;
  $('capture-help').textContent = !rememberedOwner ? 'Sign in once to capture on this device.' : sessionOwner && online ? 'Saved locally first, then synced.' : 'Saved here. Sign in when online to sync.';
  const pending = local.filter(row => row.owner_id === rememberedOwner);
  const pendingIds = new Set(pending.map(row => row.item_id));
  const rows = [
    ...pending.map(row => ({ text: row.text, time: row.created_at, pending: true,
      status: row.state === 'conflict' ? 'Delivery conflict · export available' : 'On this device · waiting to sync' })),
    ...[...captures.values()].filter(row => !pendingIds.has(row.id)).reverse().map(row => ({ text: row.original_text,
      id: row.id, time: row.received_at_ms, status: 'Saved to Inbox', pending: false })),
  ];
  $('thoughts').replaceChildren();
  for (const row of rows) {
    const li = document.createElement('li');
    const button = document.createElement('button');
    button.className = 'thought-row';
    const preview = document.createElement('span');
    preview.className = 'thought-preview'; preview.textContent = row.text.slice(0, 200) + (row.text.length > 200 ? '…' : '');
    const meta = document.createElement('span');
    meta.className = `thought-meta${row.pending ? ' pending' : ''}`;
    meta.textContent = `${row.status} · ${new Date(row.time).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}`;
    button.append(preview, meta); button.onclick = () => showDetail(row.text, row.status, row.id, row.time);
    li.append(button); $('thoughts').append(li);
  }
  $('count').textContent = rows.length ? `${rows.length}${page?.has_more ? '+' : ''}` : '';
  $('empty').hidden = rows.length > 0;
  $('more').hidden = !sessionOwner || !page?.has_more;
}
async function fetchItems() {
  const query = page?.has_more ? `?after=${page.next_after}&through=${page.through}&limit=50` : `?after=${synchronizedThrough}&limit=50`;
  const result = await api(`/items${query}`);
  if (!Array.isArray(result.changes)) throw new Error('Inbox returned an incomplete list.');
  for (const row of result.changes) captures.set(row.item.id, row.item);
  page = result;
  if (!page.has_more) synchronizedThrough = page.through;
}
async function sync() {
  if (syncing || !outbox) return;
  syncing = true;
  try {
    await verifySession(); // Do not infer authority from remembered owner or navigator.onLine.
    const result = await flushOutbox(outbox, sessionOwner, body => post('/items', body));
    await fetchItems();
    await questionsUI.refresh();
    await launchesUI.refresh(); forgejoUI.refresh(); projectsUI.refresh();
    if (result.conflicts) notice('A thought has a delivery conflict. Its original is still on this device; export it for safekeeping.');
    else if (result.sent) notice(`${result.sent === 1 ? 'Thought' : 'Thoughts'} saved to Inbox.`);
  } catch (error) { notice(message(error)); }
  finally { syncing = false; await render(); }
}
async function signIn(register) {
  const button = register ? $('register') : $('login');
  button.disabled = true;
  try {
    if (!window.PublicKeyCredential || !navigator.credentials) throw new Error('Passkeys need a supported browser over HTTPS.');
    if (register) {
      const options = await post('/auth/register/start', { token: enrollmentToken });
      const credential = await navigator.credentials.create(creationOptions(options));
      await post('/auth/register/finish', credentialJSON(credential));
      enrollmentToken = null;
    } else {
      const options = await post('/auth/login/start', {});
      const credential = await navigator.credentials.get(requestOptions(options));
      await post('/auth/login/finish', credentialJSON(credential));
    }
    notice('You’re signed in.');
    await sync();
  } catch (error) { notice(message(error)); }
  finally { button.disabled = false; await render(); }
}
async function exportUnsent() {
  const records = await outbox.forOwner(rememberedOwner);
  if (!records.length) return;
  const blob = new Blob([JSON.stringify({ format: 'okilum-unsent-captures-v1', captures: records }, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob), link = document.createElement('a');
  link.href = url; link.download = 'okilum-unsent-thoughts.json'; document.body.append(link); link.click(); link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}
async function signOut() {
  try {
    await post('/auth/logout', {});
    sessionOwner = null; rememberedOwner = null; clearRemote();
    $('detail').close(); $('detail-text').textContent = ''; $('thought').value = '';
    await outbox.setOwner(null);
    $('signout-dialog').close(); notice('Signed out.'); await render();
  } catch (error) { notice(message(error)); }
}
$('capture-form').onsubmit = async event => {
  event.preventDefault(); const button = $('save'); button.disabled = true;
  const original = $('thought').value;
  try {
    await outbox.add(rememberedOwner, original);
    // Do not erase additional typing that occurred while IndexedDB committed.
    if ($('thought').value === original) $('thought').value = '';
    notice('Saved on this device.'); await render(); await sync();
  } catch (error) { notice(message(error)); await render(); }
};
$('login').onclick = () => signIn(false);
$('register').onclick = () => signIn(true);
$('sync').onclick = sync;
$('more').onclick = async () => { try { await fetchItems(); await render(); } catch (e) { notice(message(e)); } };
$('export').onclick = () => exportUnsent().catch(error => notice(message(error)));
$('close-detail').onclick = () => $('detail').close();
$('open-publication').onclick = () => { if(selectedItem && sessionOwner) $('publication-sheet').showModal(); };
$('close-publication').onclick = () => { if(!publicationBusy) $('publication-sheet').close(); };
$('publication-sheet').addEventListener('cancel',event=>{if(publicationBusy)event.preventDefault();});
function resizeComposer(){const input=$('question');input.style.height='auto';input.style.height=`${Math.min(input.scrollHeight,160)}px`;}
$('question').addEventListener('input',resizeComposer);
$('question').addEventListener('keydown',event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();if(!$('ask').disabled)$('discussion-form').requestSubmit();}});
$('logout').onclick = async () => {
  try { if ((await outbox.forOwner(rememberedOwner)).length) $('signout-dialog').showModal(); else await signOut(); }
  catch (error) { notice(message(error)); }
};
$('cancel-signout').onclick = () => $('signout-dialog').close();
$('export-signout').onclick = async () => { try { await exportUnsent(); await signOut(); } catch (e) { notice(message(e)); } };
window.addEventListener('online', sync);
window.addEventListener('offline', () => { online = false; render().catch(() => {}); });
document.addEventListener('visibilitychange', () => { if (!document.hidden) sync(); });
setInterval(() => { if (!document.hidden && sessionOwner) sync(); }, 15000);
try {
  outbox = await openOutbox(); rememberedOwner = await outbox.owner();
  await render(); await sync();
  if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => notice('Offline page loading is unavailable. Keep this tab open to capture without a connection.'));
} catch (error) { notice(`Local storage is unavailable. Copy your thought before closing this page. ${message(error)}`); }

async function refreshDiscussion() {
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner) return;
  const turns = await api(`/items/${item}/discussion`);
  if (item !== selectedItem || owner !== sessionOwner) return;
  let pending = await outbox.pendingDiscussion(owner, item);
  if (pending && turns.some(turn => turn.operation_id === pending.operation_id && turn.prompt === pending.text)) {
    await outbox.acknowledgeDiscussion(owner, item, pending.operation_id);
    if (item === selectedItem && owner === sessionOwner && $('question').value === pending.text) $('question').value = '';
    pending = null;
  }
  if (item !== selectedItem || owner !== sessionOwner) return;
  const rendered = JSON.stringify(turns);
  if (rendered !== discussionRendered) {
  discussionRendered = rendered;
  $('exchanges').replaceChildren();
  for (const turn of turns) {
    const section = document.createElement('section');
    const question = document.createElement('pre'); question.textContent = turn.prompt; question.className='message-bubble from-you';
    const answer = document.createElement('pre');
    answer.textContent = turn.state === 'succeeded' ? turn.answer : turn.state === 'running' ? 'Thinking…' : 'Outcome uncertain. This request will not be sent again automatically. You can ask again explicitly.';
    section.className='chat-turn'; answer.className='message-bubble from-ai'; section.append(question, answer);
    if (turn.state === 'succeeded') {
      const use = document.createElement('button'); use.type = 'button'; use.className = 'quiet'; use.textContent = 'Publish this answer ↗';
      use.onclick = () => { if (!$('draft').readOnly) { $('draft').value = turn.answer; if (!$('filename').value || $('filename').value === filenameSuggestion) { filenameSuggestion = suggestedFilename(turn.answer); $('filename').value = filenameSuggestion; } publicationAttempted=false; updatePublicationForm(); $('publication-sheet').showModal(); } };
      section.append(use);
    }
    $('exchanges').append(section);
  }
  }
  const running = turns.some(turn => turn.state === 'running');
  $('ask').disabled = discussionBusy || running;
  $('ask').textContent = '↑'; $('ask').title = pending ? 'Retry same message' : 'Send message'; $('ask').setAttribute('aria-label',$('ask').title);
  resizeComposer();
  $('question').readOnly = Boolean(pending);
  $('forget-question').hidden = !pending;
  $('forget-question').disabled = discussionBusy;
  if (pending) $('question').value = pending.text;
  $('discussion-status').textContent = pending ? 'Delivery is not confirmed. Your question is kept on this device. Retrying uses the same request.' : running ? 'Waiting for AI…' : '';
}
$('discussion-form').onsubmit = async event => {
  event.preventDefault();
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner || discussionBusy) return;
  discussionBusy = true; $('ask').disabled = true;
  try {
    const request = await outbox.prepareDiscussion(owner, item, $('question').value);
    const turn = await post(`/items/${item}/discussion`, request);
    if (turn.operation_id !== request.operation_id || turn.prompt !== request.text) throw new Error('Inbox did not confirm this question. Retry the same request.');
    await outbox.acknowledgeDiscussion(owner, item, request.operation_id);
    if (item === selectedItem && owner === sessionOwner && $('question').value === request.text) $('question').value = '';
  } catch (error) {
    if (item === selectedItem) $('discussion-status').textContent = message(error);
  } finally {
    discussionBusy = false;
    if (item === selectedItem && owner === sessionOwner) {
      $('ask').disabled = false;
      // Refresh recovers a lost acknowledgement without starting another AI call.
      await refreshDiscussion().catch(() => {});
    }
  }
};
setInterval(() => {
  if (!document.hidden && $('detail').open && selectedItem && sessionOwner && !discussionBusy) refreshDiscussion().catch(() => {});
}, 3000);

$('forget-question').onclick = async () => {
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner || discussionBusy) return;
  try {
    const pending = await outbox.pendingDiscussion(owner, item);
    if (!pending || item !== selectedItem || owner !== sessionOwner) return;
    if (!confirm('Forget this local retry? This does not cancel server work. Your question stays in the text box to copy. Sending it again creates a new request.')) return;
    await outbox.acknowledgeDiscussion(owner, item, pending.operation_id);
    if (item === selectedItem && owner === sessionOwner) await refreshDiscussion();
  } catch (error) { if (item === selectedItem) $('discussion-status').textContent = message(error); }
};

async function refreshPublication(restoreDraft = false) {
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner) return;
  const [destinations, rows] = await Promise.all([api('/destinations'), api(`/items/${item}/publications`)]);
  let pending = await outbox.pendingPublication(owner, item);
  if (pending && rows.some(row => row.operation_id === pending.operation_id && row.state === 'published' && row.folder === pending.folder && row.filename === pending.filename && row.content === pending.content)) {
    await outbox.forgetPublication(owner, item, pending.operation_id); pending = null;
  }
  if (item !== selectedItem || owner !== sessionOwner) return;
  const previous = $('destination').value;
  publicationFolders = destinations.folders; publicationReady = true;
  const placeholder = document.createElement('option'); placeholder.value = ''; placeholder.textContent = 'Choose a PARA folder…';
  $('destination').replaceChildren(placeholder);
  for (const name of destinations.folders) { const option = document.createElement('option'); option.value = name; option.textContent = name; $('destination').append(option); }
  if (destinations.folders.includes(previous)) $('destination').value = previous;
  const malformed = pending && publicationProblem(pending);
  publicationRecovery = Boolean(malformed);
  publicationConflict = pending && rows.find(row => row.operation_id === pending.operation_id)?.conflict || false;
  const locked = Boolean(pending && !malformed);
  $('publish').textContent = locked ? 'Retry same publication' : 'Publish';
  $('forget-publication').hidden = !pending || publicationConflict;
  $('forget-publication').textContent = malformed ? 'Start over (keep text)' : 'Forget local publication retry';
  $('destination').disabled = locked; $('filename').readOnly = locked; $('draft').readOnly = locked;
  const savedDraft = !pending && restoreDraft ? await outbox.publicationDraft(owner, item) : null;
  if (item !== selectedItem || owner !== sessionOwner) return;
  const values = pending || savedDraft;
  if (values) { $('destination').value = values.folder; $('filename').value = values.filename; $('draft').value = values.content; }
  $('publications').replaceChildren();
  for (const row of rows) {
    if (row.conflict && await outbox.publicationForgotten(owner, item, row.operation_id)) continue;
    if (item !== selectedItem || owner !== sessionOwner) return;
    const li = document.createElement('li'); li.textContent = `${row.folder}/${row.filename} — ${publicationLabel(row)}`;
    if (row.conflict) for (const chooseName of [true, false]) {
      const action = document.createElement('button'); action.type = 'button'; action.className = 'quiet';
      action.textContent = chooseName ? 'Choose another name' : 'Forget';
      action.onclick = async () => {
        if (publicationBusy || item !== selectedItem || owner !== sessionOwner) return;
        try {
          if (row.conflict === 'occupied' && !confirm('This older request may have published before losing its acknowledgement. Check the existing file before proceeding. No files will be deleted. Continue?')) return;
          await outbox.resolvePublicationConflict(owner, item, row, chooseName);
          if (item !== selectedItem || owner !== sessionOwner) return;
          await refreshPublication(chooseName);
          $('publication-status').textContent = chooseName ? 'Your text is preserved. Choose a new filename.' : 'Conflict hidden on this device. No vault files were changed.';
          if (chooseName) $('filename').focus();
        } catch (error) { $('publication-status').textContent = message(error); }
      };
      li.append(action);
    }
    $('publications').append(li);
  }
  updatePublicationForm();

  if (!destinations.folders.length) $('publication-status').textContent = 'Fixture connector is disabled on this server.';
}
$('publish-form').onsubmit = async event => {
  event.preventDefault();
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner || publicationBusy) return;
  publicationAttempted = true;
  const payload = publicationPayload();
  const problem = !publicationReady ? 'Wait for destination folders to load.' : publicationProblem(payload, publicationFolders);
  if (problem) { $('publication-status').textContent = problem; updatePublicationForm(); return; }
  if (!confirm('Create this new Markdown file in the fixture vault? Existing files will not be overwritten.')) return;
  publicationBusy = true; $('publish').disabled = true;
  let status;
  try {
    const request = await outbox.preparePublication(owner, item, payload);
    const reply = await post(`/items/${item}/publications`, request);
    if (reply.operation_id !== request.operation_id || reply.content !== request.content || reply.folder !== request.folder || reply.filename !== request.filename || reply.state !== 'published') throw new Error('Publication was not confirmed. Keep and retry the same request.');
    await outbox.forgetPublication(owner, item, request.operation_id);
    status = `Published: ${reply.folder}/${reply.filename}. Your original thought remains in Inbox.`;
  } catch (error) { status = message(error); }
  finally {
    publicationBusy = false; updatePublicationForm();
    if (item === selectedItem && owner === sessionOwner) { await refreshPublication().catch(() => {}); $('publication-status').textContent = status; }
  }
};
$('forget-publication').onclick = async () => {
  const item = selectedItem, owner = sessionOwner;
  if (!item || !owner || publicationBusy) return;
  try {
    const pending = await outbox.pendingPublication(owner, item);
    if (!pending || item !== selectedItem || owner !== sessionOwner) return;
    if (publicationProblem(pending)) {
      const draft = publicationPayload();
      const errors = publicationProblems(draft, publicationFolders);
      if (errors.destination) draft.folder = '';
      if (errors.filename) draft.filename = '';
      await outbox.restartPublicationDraft(owner, item, pending.operation_id, draft);
      if (item !== selectedItem || owner !== sessionOwner) return;
      await refreshPublication(true);
      $('publication-status').textContent = 'Started over. Your text is saved on this device. Choose a folder and filename to publish.';
      (errors.destination ? $('destination') : errors.filename ? $('filename') : $('draft')).focus();
      return;
    }
    if (!confirm('Forget this local retry? This does not delete a published file or cancel server work. Keep the text and check the existing destination before creating another file.')) return;
    await outbox.forgetPublication(owner, item, pending.operation_id); await refreshPublication();
  } catch (error) { $('publication-status').textContent = message(error); }
};

function publicationPayload() { return { folder: $('destination').value, filename: $('filename').readOnly ? $('filename').value : markdownFilename($('filename').value), content: $('draft').value }; }
function updatePublicationForm() {
  const errors = publicationReady ? publicationProblems(publicationPayload(), publicationFolders) : {};
  const problem = publicationReady ? Object.values(errors)[0] || '' : 'Loading destination folders…';
  for (const id of ['destination', 'filename', 'draft']) {
    $(id).setAttribute('aria-invalid', String(publicationAttempted && Boolean(errors[id])));
    $(`${id}-error`).textContent = errors[id] || '';
    $(`${id}-error`).hidden = !publicationAttempted || !errors[id];
  }
  if (publicationRecovery) $('publication-status').textContent = problem ? `Your previous draft is preserved. ${problem} Or use Start over to discard only the failed retry.` : 'Your previous draft is ready to publish.';
  $('publish').disabled = publicationBusy || publicationConflict || !publicationReady;
  $('publication-help').textContent = publicationConflict ? `${publicationLabel({ conflict: publicationConflict })}. Choose another name or Forget below.` : '';
}
for (const id of ['destination', 'filename', 'draft']) $(id).addEventListener('input', updatePublicationForm);
$('destination').addEventListener('change', updatePublicationForm);

setInterval(() => { questionsUI.expire(); }, 1000);
setInterval(() => { if (sessionOwner && online && !document.hidden) questionsUI.refresh(); }, 15000);

setInterval(() => { if (sessionOwner && online && !document.hidden) launchesUI.refresh(); }, 15000);
setInterval(() => { if (sessionOwner && online && !document.hidden) forgejoUI.refresh(); }, 60000);

setInterval(() => { if(sessionOwner&&online&&!document.hidden) projectsUI.refresh(); }, 30000);
