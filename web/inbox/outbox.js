import { publicationProblem } from './publication-form.js';
// Only unsent captures and the last verified account live here. No session token.
export function openOutbox(factory = indexedDB, name = 'tessera-inbox-v1') {
  return new Promise((resolve, reject) => {
    const request = factory.open(name, 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore('captures', { keyPath: 'operation_id' });
      request.result.createObjectStore('meta');
    };
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error('Close another Inbox tab to update local storage.'));
    request.onsuccess = () => resolve(new Outbox(request.result));
  });
}
export class Outbox {
  constructor(db) { this.db = db; db.onversionchange = () => db.close(); }
  transaction(store, mode, action) {
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction(store, mode);
      let result;
      const request = action(tx.objectStore(store));
      request.onsuccess = () => { result = request.result; };
      tx.oncomplete = () => resolve(result);
      tx.onabort = () => reject(tx.error || new Error('Local save did not complete.'));
      tx.onerror = () => {}; // Reject on abort; never claim persistence on request success.
    });
  }
  owner() { return this.transaction('meta', 'readonly', store => store.get('owner')); }
  setOwner(owner) { return this.transaction('meta', 'readwrite', store => store.put(owner, 'owner')); }
  discussionKey(owner, item) {
    if (!owner || !item) throw new Error('Sign in and select a saved thought first.');
    return `discussion:${owner}:${item}`;
  }
  pendingDiscussion(owner, item) {
    return this.transaction('meta', 'readonly', store => store.get(this.discussionKey(owner, item)));
  }
  async prepareDiscussion(owner, item, text) {
    if (!text.trim() || new TextEncoder().encode(text).length > 16384) throw new Error('Write a question of up to 16 KB.');
    const key = this.discussionKey(owner, item);
    // Atomic across tabs: never replace an unacknowledged operation.
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      let result;
      const read = store.get(key);
      read.onsuccess = () => {
        result = read.result || { operation_id: crypto.randomUUID(), text };
        if (!read.result) store.add(result, key);
      };
      tx.oncomplete = () => resolve(result);
      tx.onabort = () => reject(tx.error);
    });
  }
  acknowledgeDiscussion(owner, item, operation) {
    const key = this.discussionKey(owner, item);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      const read = store.get(key);
      read.onsuccess = () => { if (read.result?.operation_id === operation) store.delete(key); };
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error);
    });
  }
  publicationKey(owner, item) {
    if (!owner || !item) throw new Error('Sign in and select a saved thought first.');
    return `publication:${owner}:${item}`;
  }
  pendingPublication(owner, item) { return this.transaction('meta', 'readonly', s => s.get(this.publicationKey(owner, item))); }
  publicationDraft(owner, item) { return this.transaction('meta', 'readonly', s => s.get(`${this.publicationKey(owner, item)}:draft`)); }
  restartPublicationDraft(owner, item, operation, draft) {
    const key = this.publicationKey(owner, item);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      const read = store.get(key);
      read.onsuccess = () => {
        if (read.result?.operation_id !== operation || !publicationProblem(read.result)) { tx.abort(); return; }
        // Commit the editable draft before discarding its malformed retry, in
        // the same transaction. A reload must not lose the preserved text.
        store.put(draft, `${key}:draft`);
        store.delete(key);
      };
      tx.oncomplete = resolve;
      tx.onabort = () => reject(tx.error || new Error('The saved request changed. Reopen this thought before starting over.'));
    });
  }
  async publicationForgotten(owner, item, operation) {
    return this.transaction('meta', 'readonly', s => s.get(`${this.publicationKey(owner, item)}:forgotten:${operation}`));
  }
  resolvePublicationConflict(owner, item, row, chooseName) {
    const key = this.publicationKey(owner, item);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      const read = store.get(key);
      read.onsuccess = () => {
        if (read.result && read.result.operation_id !== row.operation_id) { tx.abort(); return; }
        // This action is explicit; keep the server journal and every vault file.
        if (chooseName) store.put({ folder: row.folder, filename: '', content: row.content }, `${key}:draft`);
        store.put(true, `${key}:forgotten:${row.operation_id}`);
        store.delete(key);
      };
      tx.oncomplete = resolve;
      tx.onabort = () => reject(tx.error || new Error('Another publication is pending. Resolve it first.'));
    });
  }
  preparePublication(owner, item, payload) {
    const problem = publicationProblem(payload);
    if (problem) return Promise.reject(new Error(problem));
    const key = this.publicationKey(owner, item);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      let result;
      const read = store.get(key);
      read.onsuccess = () => {
        // Older clients could persist structurally invalid requests. The server
        // rejects these before any intent; let the user repair that draft.
        result = read.result && !publicationProblem(read.result) ? read.result : { ...payload, operation_id: crypto.randomUUID() };
        if (result !== read.result) store.put(result, key);
        store.delete(`${key}:draft`);
      };
      tx.oncomplete = () => resolve(result); tx.onabort = () => reject(tx.error);
    });
  }
  forgetPublication(owner, item, operation) {
    const key = this.publicationKey(owner, item);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction('meta', 'readwrite'), store = tx.objectStore('meta');
      const read = store.get(key);
      read.onsuccess = () => { if (read.result?.operation_id === operation) store.delete(key); };
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error);
    });
  }
  async add(owner, text, ids = { operation_id: crypto.randomUUID(), item_id: crypto.randomUUID() }) {
    if (!owner) throw new Error('Sign in once before capturing offline.');
    if (!text.trim()) throw new Error('Write a thought first.');
    if (new TextEncoder().encode(text).length > 65536) throw new Error('This thought is too long (64 KB maximum).');
    const record = { ...ids, owner_id: owner, text, created_at: Date.now(), state: 'pending' };
    await this.transaction('captures', 'readwrite', store => store.add(record));
    return record;
  }
  async all() {
    return (await this.transaction('captures', 'readonly', store => store.getAll()))
      .sort((a, b) => a.created_at - b.created_at);
  }
  async forOwner(owner) { return owner ? (await this.all()).filter(row => row.owner_id === owner) : []; }
  remove(id) { return this.transaction('captures', 'readwrite', store => store.delete(id)); }
  async markConflict(id) {
    // Do not resurrect a row another tab has already acknowledged and removed.
    await new Promise((resolve, reject) => {
      const tx = this.db.transaction('captures', 'readwrite');
      const store = tx.objectStore('captures');
      const request = store.get(id);
      request.onsuccess = () => { if (request.result) store.put({ ...request.result, state: 'conflict' }); };
      tx.oncomplete = resolve;
      tx.onabort = () => reject(tx.error);
    });
  }
}

// Concurrent tabs may submit the same operation; the server's idempotency key
// makes that safe. Never mint a replacement identity during retry.
export async function flushOutbox(outbox, authenticatedOwner, send) {
  const result = { sent: 0, conflicts: 0 };
  for (const record of await outbox.all()) {
    if (record.owner_id !== authenticatedOwner || record.state === 'conflict') continue;
    let reply;
    try {
      reply = await send({ operation_id: record.operation_id, item_id: record.item_id, text: record.text });
    } catch (error) {
      if (error.status === 409) {
        await outbox.markConflict(record.operation_id);
        result.conflicts++;
        continue;
      }
      throw error; // Network, auth and server errors all retain the durable row.
    }
    if (reply.operation_id !== record.operation_id || reply.item?.id !== record.item_id
        || reply.item?.original_text !== record.text) {
      throw new Error('The server did not confirm this thought. It is still saved on this device.');
    }
    await outbox.remove(record.operation_id);
    result.sent++;
  }
  return result;
}
