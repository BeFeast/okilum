// Pure JavaScript guard regressions, not a native/GUI acceptance fixture.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const file = 'crates/okilum-shell/src/reader_manager_focus/page_keys.js';
const source = fs.readFileSync(file, 'utf8');
const generated = fs.readFileSync(file.replace('.js', '.inc'), 'utf8');
assert.equal(JSON.parse(generated.slice(generated.indexOf('@"') + 1).trim().replace(/;$/, '')), source, 'shipped Objective-C script must equal reviewed JavaScript');
function page(doc = 'document-1', engine = false) {
    const listeners = new Map();
    const field = {type: 'text', value: 'fixture-selection', selectionStart: 0, selectionEnd: 7};
    const calls = [];
    const history = [];
    const document = {activeElement: field,
        addEventListener(name, fn) { listeners.set(name, fn); },
        execCommand(...args) {
            calls.push(args);
            if (engine && args[0] === 'insertText') {
                history.push({value: field.value, start: field.selectionStart, end: field.selectionEnd});
                field.value = field.value.slice(0, field.selectionStart) + args[2] + field.value.slice(field.selectionEnd);
                field.selectionStart += args[2].length; field.selectionEnd = field.selectionStart;
                listeners.get('input')?.({type: 'input', isTrusted: true, inputType: 'insertText'});
            } else if (engine && args[0] === 'undo') {
                const prior = history.pop();
                field.value = prior.value; field.selectionStart = prior.start; field.selectionEnd = prior.end;
                listeners.get('input')?.({type: 'input', isTrusted: true, inputType: 'historyUndo'});
            }
            return true;
        }};
    const window = {addEventListener(name, fn) { listeners.set(name, fn); }, getSelection() { return 'page-selection'; }};
    const location = {origin: 'https://t3.example'};
    vm.runInNewContext(source, {window, document, location, crypto: {randomUUID: (() => { let generation = 0; return () => `${doc}-${++generation}`; })()},
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

for (const undo of [false, true]) {
    const p = page('queued-document', true);
    p.field.value = ''; p.field.selectionStart = p.field.selectionEnd = 0;
    const acceptedFirst = p.state.context();
    const acceptedSecond = p.state.context(); // accepted before first fallback completes
    const first = p.state.run('paste:', {kind: 'text', data: 'A'}, acceptedFirst);
    const next = p.state.rebase(acceptedSecond, first.context);
    assert.equal(next.cancel, undefined, 'only prior app edits may rebase a queued command');
    const second = p.state.run(undo ? 'undo:' : 'paste:', {kind: 'text', data: 'B'}, next.context);
    assert.equal(second.ok, true);
    assert.equal(p.field.value, undo ? '' : 'AB', 'rapid paste/paste and paste/undo preserve accepted order');
    p.emit('keydown', {metaKey: false, ctrlKey: false});
    assert.equal(p.state.rebase(acceptedSecond, second.context).cancel, true, 'foreign typing cannot be rebased');
}

for (const foreign of ['selection', 'input']) {
    const p = page();
    const before = p.state.context();
    p.emit('paste');
    p.field.selectionStart = p.field.selectionEnd = 4;
    p.emit('input', {inputType: 'insertFromPaste'});
    const native = p.state.observe(before, 'paste:');
    assert.equal(native.native, true, 'positive control: terminal native edit snapshot is accepted');
    if (foreign === 'selection') {
        p.field.selectionStart = p.field.selectionEnd = 9;
        p.emit('selectionchange');
    } else {
        p.field.value += 'foreign edit';
        p.emit('input', {isTrusted: false, inputType: 'insertText'});
    }
    assert.equal(p.state.observe(before, 'paste:').cancel, true, 'delayed native probe must not bless foreign page changes');
    assert.equal(p.state.rebase(before, native.context).cancel, true, 'queued command must not move to foreign selection/edit');
}

// Exercise the shipped init listener: keep T3's active form mounted through click.
{
    const panel = fs.readFileSync('crates/okilum-shell/src/reader_manager_panel.rs', 'utf8');
    const init = panel.match(/\.with_initialization_script\(\s*r#"([\s\S]*?)"#/)[1];
    const events = new Map();
    class Element {
        closest(selector) {
            if (selector === 'button, [role="button"]') return this.button ?? null;
            if (selector === 'form') return this.form ?? null;
            return this.draggable ?? null;
        }
    }
    const active = new Element();
    const button = new Element();
    const icon = new Element();
    icon.button = button;
    button.form = {contains: node => node === active};
    const document = {activeElement: active, addEventListener() {}};
    vm.runInNewContext(init, {Element, document, window: {addEventListener(name, fn) {events.set(name, fn);}}});
    const down = events.get('mousedown');
    assert.equal(typeof down, 'function', 'positive control: shipped listener registered');
    function press(overrides = {}) {
        let prevented = false;
        down({target: icon, button: 0, defaultPrevented: false,
            preventDefault() {prevented = true;}, ...overrides});
        return prevented;
    }
    assert.equal(press(), true, 'keep focused composer mounted for its remove button');
    assert.equal(press({button: 2}), false);
    assert.equal(press({defaultPrevented: true}), false, 'respect page handlers');
    button.disabled = true;
    assert.equal(press(), false);
    button.disabled = false;
    button.draggable = {};
    assert.equal(press(), false);
    button.draggable = null;
    document.activeElement = button;
    assert.equal(press(), false);
    document.activeElement = new Element();
    assert.equal(press(), false, 'do not change another form focus');
    assert.equal(press({target: new Element()}), false, 'non-buttons retain native defaults');
}
console.log('PASS: shipped button focus guard preserves composer and respects other targets');

{
    const p = page();
    const before = p.state.context();
    assert.equal(p.state.run('copy:', null, before).ok, true);
    p.emit('pagehide');
    assert.equal(p.state.run('copy:', null, before).ok, false);
    p.emit('pageshow', {persisted: true});
    const restored = p.state.context();
    assert.notEqual(restored.doc, before.doc);
    assert.equal(p.state.run('copy:', null, before).ok, false, 'queued pre-navigation commands stay invalid');
    assert.equal(p.state.run('copy:', null, restored).ok, true, 'restored page accepts fresh commands');
    p.emit('pageshow', {persisted: false});
    assert.equal(p.state.context().doc, restored.doc, 'ordinary pageshow does not invalidate a live page');
}
console.log('PASS: cached restoration renews context and rejects pre-navigation commands');
