import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {retiredSyncLinkMessage} from '../sync-links.js';

test('old sync approval links give plain guidance without exposing the request', () => {
  for (const hash of ['#sync=private-request', '#sync=', '#device=other&sync=request']) {
    const message = retiredSyncLinkMessage(hash);
    assert.match(message, /Settings → Sync/);
    assert.match(message, /Existing file sync connections have not been changed/);
    assert.ok(!message.includes('private-request'));
  }
  for (const hash of ['', '#device=inbox-device', '#enroll=inbox-owner']) {
    assert.equal(retiredSyncLinkMessage(hash), null);
  }
});

test('production Inbox has no menu, dialog or deep-link mount for folder pairing', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  const app = readFileSync(new URL('../app.js', import.meta.url), 'utf8');
  assert.ok(html.includes('id="devices-dialog"'), 'Inbox account devices remain');
  assert.ok(html.includes('id="register"'), 'Inbox account passkeys remain');
  assert.doesNotMatch(html, /sync-pairing-|Folder sync computers|Approve with passkey/);
  assert.doesNotMatch(app, /mountSyncPairing|syncPairingUI|\/sync\/(approve|requests|vaults)/);
  assert.match(app, /retiredSyncLinkMessage\(location.hash\)/);
  const sw = readFileSync(new URL('../sw.js', import.meta.url), 'utf8');
  assert.doesNotMatch(sw, /\/sync-pairing.js/);
  assert.match(sw, /\/sync-links.js/);
});
