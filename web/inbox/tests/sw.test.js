import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

test('worker caches shell, never API, cross-origin, query or POST responses', async () => {
  const listeners = {}, writes = [];
  let offline = false;
  const cached = { cached: true };
  const cache = { put: async key => writes.push(key) };
  const context = {
    URL,
    self: { location: { origin: 'https://inbox.test' }, addEventListener: (name, fn) => { listeners[name] = fn; } },
    caches: { open: async () => cache, match: async () => cached },
    fetch: async () => { if (offline) throw new Error('offline'); return { ok: true, clone: () => ({}) }; },
  };
  vm.runInNewContext(await readFile(new URL('../sw.js', import.meta.url), 'utf8'), context);
  const request = (path, method = 'GET') => {
    let response;
    listeners.fetch({ request: { url: new URL(path, 'https://inbox.test').href, method }, respondWith: promise => { response = promise; } });
    return response;
  };
  for (const path of ['/api/v1/items', '/api/v1/session', '/api/v1/auth/login/start', '/unknown', '/?token=secret', 'https://other.test/app.js']) {
    assert.equal(request(path), undefined, path);
  }
  assert.equal(request('/', 'POST'), undefined);
  await request('/app.js');
  assert.deepEqual(writes, ['/app.js']);
  offline = true;
  assert.equal(await request('/'), cached);
  assert.equal(request('/api/v1/items'), undefined);
});
