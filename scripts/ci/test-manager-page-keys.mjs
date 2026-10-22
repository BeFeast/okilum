// Pure JavaScript guard regressions, not a native/GUI acceptance fixture.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const file = 'crates/okilum-shell/src/reader_manager_focus/page_keys.js';
const source = fs.readFileSync(file, 'utf8');
const generated = fs.readFileSync(file.replace('.js', '.inc'), 'utf8');
assert.equal(JSON.parse(generated.slice(generated.indexOf('@"') + 1).trim().replace(/;$/, '')), source, 'shipped Objective-C script must equal reviewed JavaScript');
function page(doc = 'document-1') {
    const listeners = new Map();
    const field = {type: 'text', value: 'fixture-selection', selectionStart: 0, selectionEnd: 7};
    const calls = [];
    const document = {activeElement: field,
        addEventListener(name, fn) { listeners.set(name, fn); },
        execCommand(...args) { calls.push(args); return true; }};
    const window = {addEventListener(name, fn) { listeners.set(name, fn); }, getSelection() { return 'page-selection'; }};
    const location = {origin: 'https://t3.example'};
    vm.runInNewContext(source, {window, document, location, crypto: {randomUUID: () => doc},
        getComputedStyle: a => ({webkitTextSecurity: a.secureCss || 'none'})});
    return {state: window.__okilumKeysV2, field, document, location, calls,
        emit(type, extra = {}) { listeners.get(type)?.({type, isTrusted: true, ...extra}); }};
}
{
    const p = page();
    const before = p.state.context();
    assert.equal(p.state.run('copy:', null, before).text, 'fixture', 'positive control: normal selected text can be copied');
    for (const type of ['password', 'text']) {
        p.field.type = type; p.field.secureCss = type === 'text' ? 'disc' : 'none';
        let clipboard = 'unchanged-clipboard';
        for (const action of ['copy:', 'cut:', 'delete:']) {
            const result = p.state.run(action, null, before);
            if (result.ok && result.text) clipboard = result.text;
            assert.equal(result.ok, false, 'secure copy/cut must not expose a value');
            assert.equal(result.reason, 'secure-field');
        }
        assert.equal(clipboard, 'unchanged-clipboard');
    }
    assert.equal(p.calls.length, 0, 'secure cut must not delete');
}
{
    const p = page(); const before = p.state.context();
    assert.equal(p.state.observe(before, 'paste:').context.doc, before.doc, 'positive control: unchanged target permits guarded fallback');
    assert.equal(p.state.run('paste:', {kind: 'text', data: 'fixture'}, undefined).ok, false, 'missing state fails closed');
    const replacement = page('document-2');
    assert.equal(replacement.state.run('paste:', {kind: 'text', data: 'secret'}, before).ok, false, 'reload/navigation cancels old command');
    p.location.origin = 'https://different.example';
    assert.equal(p.state.run('paste:', {kind: 'text', data: 'secret'}, before).ok, false);
    assert.equal(p.calls.length + replacement.calls.length, 0);
}
{
    const p = page(); const before = p.state.context();
    p.document.activeElement = {...p.field}; p.emit('focusin');
    assert.equal(p.state.observe(before, 'cut:').cancel, true, 'field change cancels queued fallback');
    assert.equal(p.state.run('delete:', null, before).ok, false);
    const q = page(); const prior = q.state.context();
    q.emit('keydown', {metaKey: false, ctrlKey: false});
    assert.equal(q.state.observe(prior, 'paste:').cancel, true, 'subsequent typing must not be reordered after deferred paste');
    assert.equal(q.calls.length, 0);
}
{
    const p = page(); const before = p.state.context(); p.emit('paste');
    assert.equal(p.state.observe(before, 'paste:').native, true, 'trusted native paste suppresses fallback');
    const q = page(); const context = q.state.context();
    assert.equal(q.state.run('paste:', {kind: 'text', data: 'fixture'}, context).ok, true);
    assert.deepEqual(q.calls, [['insertText', false, 'fixture']]);
    for (const [action, name] of [['selectAll:', 'selectAll'], ['undo:', 'undo'], ['redo:', 'redo']]) {
        assert.equal(q.state.run(action, null, context).ok, true);
        assert.equal(q.calls.at(-1)[0], name);
    }
    q.emit('pagehide');
    assert.equal(q.state.run('paste:', {kind: 'text', data: 'fixture'}, context).ok, false);
}
console.log('PASS: secure clipboard, document/focus/revision guards, input ordering and native-observation controls');
