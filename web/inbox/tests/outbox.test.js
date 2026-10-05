import test from 'node:test';
import assert from 'node:assert/strict';
import { IDBFactory } from 'fake-indexeddb';
import { openOutbox, flushOutbox } from '../outbox.js';

function ack(body) { return { operation_id: body.operation_id, item: { id: body.item_id, original_text: body.text } }; }
test('lost response keeps identity and exact text through reopen; confirmed retry deletes once', async () => {
  const factory = new IDBFactory();
  let box = await openOutbox(factory);
  await box.setOwner('owner-A');
  const original = await box.add('owner-A', '  Мысль\r\n📝');
  const requests = [];
  await assert.rejects(flushOutbox(box, 'owner-A', async body => {
    requests.push(body); throw new TypeError('response lost after commit');
  }));
  box.db.close(); box = await openOutbox(factory);
  assert.equal(await box.owner(), 'owner-A');
  assert.deepEqual(await box.all(), [original]);
  await flushOutbox(box, 'owner-A', async body => { requests.push(body); return ack(body); });
  assert.deepEqual(requests[0], requests[1]);
  assert.equal(requests[1].text, original.text);
  assert.deepEqual(await box.all(), []);
});
test('unauthenticated, wrong account, server errors and malformed acknowledgement retain rows', async () => {
  const box = await openOutbox(new IDBFactory());
  const original = await box.add('A', 'one');
  let sent = 0;
  await flushOutbox(box, 'B', async body => { sent++; return ack(body); });
  assert.equal(sent, 0);
  for (const status of [401, 500, 503]) {
    await assert.rejects(flushOutbox(box, 'A', async () => { throw Object.assign(new Error('failed'), { status }); }));
    assert.deepEqual(await box.all(), [original]);
  }
  await assert.rejects(flushOutbox(box, 'A', async body => ({ ...ack(body), operation_id: 'wrong' })));
  await assert.rejects(flushOutbox(box, 'A', async body => ({ ...ack(body), item: { id: body.item_id, original_text: 'wrong' } })));
  assert.deepEqual(await box.all(), [original]);
});
test('conflict is retained for export and never silently reminted or retried', async () => {
  const box = await openOutbox(new IDBFactory());
  const original = await box.add('A', 'conflict');
  const result = await flushOutbox(box, 'A', async () => { throw Object.assign(new Error('conflict'), { status: 409 }); });
  assert.equal(result.conflicts, 1);
  assert.deepEqual(await box.all(), [{ ...original, state: 'conflict' }]);
  await flushOutbox(box, 'A', () => { assert.fail('conflict must not be retried'); });
});
test('duplicate local ID aborts the transaction instead of replacing the original', async () => {
  const box = await openOutbox(new IDBFactory());
  const original = await box.add('A', 'original');
  await assert.rejects(box.add('A', 'replacement', original));
  assert.deepEqual(await box.all(), [original]);
});
test('parallel tabs replay one stable identity and converge after acknowledgement', async () => {
  const factory = new IDBFactory();
  const a = await openOutbox(factory), b = await openOutbox(factory);
  const original = await a.add('A', 'concurrent');
  const seen = new Set();
  await Promise.all([a, b].map(box => flushOutbox(box, 'A', async body => { seen.add(body.operation_id); return ack(body); })));
  assert.deepEqual([...seen], [original.operation_id]);
  assert.deepEqual(await a.all(), []);
  await a.markConflict(original.operation_id);
  assert.deepEqual(await b.all(), []);
});
test('empty, oversized and unbound captures never claim a local save', async () => {
  const box = await openOutbox(new IDBFactory());
  await assert.rejects(box.add(null, 'unbound'));
  await assert.rejects(box.add('A', ' \n'));
  await assert.rejects(box.add('A', 'я'.repeat(32769)));
  assert.deepEqual(await box.all(), []);
  await box.add('A', 'я'.repeat(32768));
  assert.equal((await box.all()).length, 1);
});

test('export/list selection hides retained rows after logout or an account switch', async () => {
  const box = await openOutbox(new IDBFactory());
  const a = await box.add('A', 'private A');
  const b = await box.add('B', 'private B');
  await box.setOwner(null);
  assert.deepEqual(await box.forOwner(await box.owner()), []);
  await box.setOwner('B');
  assert.deepEqual(await box.forOwner(await box.owner()), [b]);
  await box.setOwner('A');
  assert.deepEqual(await box.forOwner(await box.owner()), [a]);
  assert.equal((await box.all()).length, 2, 'isolation must not delete unsent originals');
});

test('unacknowledged AI intent survives reopen and keeps identity across tabs/accounts', async () => {
  const factory = new IDBFactory();
  let box = await openOutbox(factory);
  const request = await box.prepareDiscussion('A', 'item', 'first question');
  assert.deepEqual(await box.prepareDiscussion('A', 'item', 'new typing'), request);
  box.db.close(); box = await openOutbox(factory);
  assert.deepEqual(await box.pendingDiscussion('A', 'item'), request);
  assert.equal(await box.pendingDiscussion('B', 'item'), undefined);
  await box.acknowledgeDiscussion('A', 'item', 'wrong');
  assert.deepEqual(await box.pendingDiscussion('A', 'item'), request);
  await box.acknowledgeDiscussion('A', 'item', request.operation_id);
  assert.equal(await box.pendingDiscussion('A', 'item'), undefined);
  assert.notEqual((await box.prepareDiscussion('A', 'item', 'explicit next request')).operation_id, request.operation_id);
});

test('publication retries preserve exact preview, destination and identity across reopen', async () => {
  const factory = new IDBFactory(); let box = await openOutbox(factory);
  const original = { folder: 'Projects', filename: 'Идея.md', content: '# Exact\n' };
  const request = await box.preparePublication('A', 'item', original);
  box.db.close(); box = await openOutbox(factory);
  assert.deepEqual(await box.preparePublication('A', 'item', { ...original, filename: 'other.md' }), request);
  assert.equal(await box.pendingPublication('B', 'item'), undefined);
  await box.forgetPublication('A', 'item', 'wrong');
  assert.deepEqual(await box.pendingPublication('A', 'item'), request);
  await box.forgetPublication('A', 'item', request.operation_id);
  assert.equal(await box.pendingPublication('A', 'item'), undefined);
});

test('invalid publication cannot poison the durable outbox; legacy invalid draft can be repaired', async () => {
  const box = await openOutbox(new IDBFactory());
  for (const payload of [
    { folder: '', filename: 'Draft.md', content: '# Kept draft' },
    { folder: 'Projects', filename: '../Draft.md', content: '# Kept draft' },
    { folder: 'Projects', filename: 'Draft.md', content: '' },
  ]) {
    await assert.rejects(box.preparePublication('A', 'item', payload));
    assert.equal(await box.pendingPublication('A', 'item'), undefined);
  }
  await box.transaction('meta', 'readwrite', s => s.put({ operation_id: 'legacy', folder: '', filename: 'Draft.md', content: '# Kept draft' }, box.publicationKey('A', 'item')));
  const repaired = await box.preparePublication('A', 'item', { folder: 'Projects', filename: 'Draft.md', content: '# Kept draft' });
  assert.notEqual(repaired.operation_id, 'legacy');
  assert.equal(repaired.content, '# Kept draft');
  assert.equal(repaired.folder, 'Projects');
});

test('start over atomically preserves exact edited draft across reload, scoped to owner/item', async () => {
  const factory = new IDBFactory(); let box = await openOutbox(factory);
  const legacy = { operation_id: 'legacy', folder: '', filename: '', content: '# Original AI draft\r\n📝' };
  await box.transaction('meta', 'readwrite', s => s.put(legacy, box.publicationKey('A', 'item')));
  const draft = { folder: '', filename: '', content: '# Edited draft\r\n📝  ' };
  await box.restartPublicationDraft('A', 'item', 'legacy', draft);
  box.db.close(); box = await openOutbox(factory);
  assert.equal(await box.pendingPublication('A', 'item'), undefined);
  assert.deepEqual(await box.publicationDraft('A', 'item'), draft);
  assert.equal(await box.publicationDraft('B', 'item'), undefined);
  const valid = { ...draft, folder: 'Projects', filename: 'Idea.md' };
  const request = await box.preparePublication('A', 'item', valid);
  assert.equal(request.content, draft.content);
  assert.equal(await box.publicationDraft('A', 'item'), undefined);
  await assert.rejects(box.restartPublicationDraft('A', 'item', request.operation_id, draft));
  assert.deepEqual(await box.pendingPublication('A', 'item'), request, 'a valid in-flight operation must never be reset');
});

test('choose another name preserves exact draft, forget hides only this conflict, other pending is protected', async () => {
 const factory = new IDBFactory();
 let box = await openOutbox(factory);
 const request = await box.preparePublication('owner', 'item', { folder: 'Projects', filename: 'a.md', content: '# Exact\n\nтекст' });
 await box.resolvePublicationConflict('owner', 'item', request, true);
 box.db.close(); box = await openOutbox(factory);
 assert.equal(await box.pendingPublication('owner', 'item'), undefined);
 assert.deepEqual(await box.publicationDraft('owner', 'item'), { folder: 'Projects', filename: '', content: request.content });
 assert.equal(await box.publicationForgotten('owner', 'item', request.operation_id), true);
 assert.equal(await box.publicationForgotten('other', 'item', request.operation_id), undefined);
 const next = await box.preparePublication('owner', 'item', { ...request, filename: 'b.md' });
 assert.notEqual(next.operation_id, request.operation_id);
 await assert.rejects(box.resolvePublicationConflict('owner', 'item', request, true));
 assert.equal((await box.pendingPublication('owner', 'item')).operation_id, next.operation_id);
 await box.resolvePublicationConflict('owner', 'item', next, false);
 assert.equal(await box.pendingPublication('owner', 'item'), undefined);
});
