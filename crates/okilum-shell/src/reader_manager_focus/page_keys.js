// Page editing fallback. Context snapshots contain no input/selection contents.
(() => {
    if (window.__okilumKeysV2?.document === document) return;
    const ids = new WeakMap();
    let nextId = 1;
    const documentId = () => globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`;
    const state = {
        document, live: true,
        doc: documentId(),
        origin: location.origin, focus: 0, edit: 0, selection: 0, activity: 0,
        seq: 0, events: [],
    };
    const active = () => {
        let a = document.activeElement;
        while (a?.shadowRoot?.activeElement) a = a.shadowRoot.activeElement;
        return a;
    };
    const id = a => {
        if (!a) return 0;
        if (!ids.has(a)) ids.set(a, nextId++);
        return ids.get(a);
    };
    const selectionStamp = () => {
        const a = active(), selection = window.getSelection();
        return JSON.stringify(typeof a?.selectionStart === 'number'
            ? [id(a), a.selectionStart, a.selectionEnd]
            : [id(selection?.anchorNode), selection?.anchorOffset, id(selection?.focusNode), selection?.focusOffset]);
    };
    const context = () => ({
        doc: state.doc, origin: state.origin, target: id(active()),
        focus: state.focus, edit: state.edit, selection: selectionStamp(),
        activity: state.activity, seq: state.seq,
    });
    const sameDocumentTarget = c => state.live && c && c.doc === state.doc
        && c.origin === location.origin && c.origin === state.origin
        && c.target === id(active()) && c.focus === state.focus;
    const matches = c => sameDocumentTarget(c) && c.edit === state.edit
        && c.selection === selectionStamp() && c.activity === state.activity && c.seq === state.seq;
    const secure = a => a?.type?.toLowerCase() === 'password'
        || (a && typeof getComputedStyle === 'function'
            && !['', 'none'].includes(getComputedStyle(a).webkitTextSecurity || ''));
    const report = kind => {
        window.webkit?.messageHandlers?.okilumNativeKeysV2?.postMessage({kind, ...context()});
    };
    const record = e => {
        if (e.type === 'focusin' || e.type === 'pointerdown') state.focus++;
        if (e.type === 'selectionchange') state.selection++;
        if (e.type === 'input' || e.type === 'beforeinput') state.edit++;
        if (e.type === 'keydown' && !(e.metaKey || e.ctrlKey)) state.activity++;
        if (e.isTrusted && ['paste', 'copy', 'cut', 'input', 'beforeinput'].includes(e.type)) {
            state.events.push({seq: ++state.seq, type: e.type, inputType: e.inputType || '', target: id(active()), after: null});
            state.events.at(-1).after = context();
            if (state.events.length > 32) state.events.shift();
        }
        report('context');
    };
    ['focusin', 'pointerdown', 'selectionchange', 'input', 'beforeinput', 'paste', 'copy', 'cut', 'keydown']
        .forEach(name => document.addEventListener(name, record, true));
    window.addEventListener('pagehide', () => { state.live = false; report('invalid'); }, true);
    window.addEventListener('pageshow', () => {
        if (state.live) return;
        state.doc = documentId();
        state.live = true;
        state.events = [];
        state.focus++;
        state.activity++;
        report('context');
    }, true);
    const observe = (before, action) => {
        if (!sameDocumentTarget(before)) return {cancel: true, reason: 'document-or-focus-changed'};
        const expected = {'paste:': 'paste', 'copy:': 'copy', 'cut:': 'cut'}[action];
        const inputType = {'paste:': 'insertFromPaste', 'cut:': 'deleteByCut',
            'undo:': 'historyUndo', 'redo:': 'historyRedo'}[action];
        const events = state.events.filter(e => e.seq > before.seq && e.target === before.target);
        const terminal = events.filter(e => e.type === expected
            || (e.type === 'input' && e.inputType === inputType)).at(-1);
        if (terminal) {
            // Capture at the terminal native event, not at this delayed probe.
            // Never bless a later programmatic selection/edit as our prefix.
            if (!matches(terminal.after) || events.some(e => e.type === 'input' && e.inputType !== inputType)) {
                return {cancel: true, reason: 'foreign-change-after-native-command'};
            }
            return {native: true, context: terminal.after};
        }
        // Native select-all has no edit event; an unmodified context can only
        // prove no change. Let the guarded page command perform select-all.
        // Later typing, selection or mouse input cancels a delayed fallback;
        // it must never insert a deferred paste after subsequent user input.
        if (!matches(before)) return {cancel: true, reason: 'subsequent-input-or-selection'};
        return {native: false, context: context()};
    };
    const edit = (action, payload, expected) => {
        if (!matches(expected)) return {ok: false, reason: 'stale-document-focus-or-input'};
        const a = active();
        if ((action === 'copy:' || action === 'cut:' || action === 'delete:') && secure(a)) {
            return {ok: false, reason: 'secure-field'};
        }
        if (action === 'copy:' || action === 'cut:') {
            const text = typeof a?.selectionStart === 'number'
                ? a.value.slice(a.selectionStart, a.selectionEnd) : String(window.getSelection());
            return {ok: text.length > 0, text};
        }
        if (action === 'paste:' && payload?.kind === 'image') {
            const bytes = Uint8Array.from(atob(payload.data), c => c.charCodeAt(0));
            const transfer = new DataTransfer();
            transfer.items.add(new File([bytes], 'clipboard.png', {type: 'image/png'}));
            a.dispatchEvent(new ClipboardEvent('paste', {clipboardData: transfer, bubbles: true, cancelable: true}));
            return {ok: true, kind: 'image'};
        }
        if (action === 'paste:') {
            if (payload?.kind !== 'text') return {ok: false, reason: 'no-supported-clipboard-data'};
            return {ok: document.execCommand('insertText', false, payload.data), kind: 'text'};
        }
        const command = {'selectAll:': 'selectAll', 'undo:': 'undo', 'redo:': 'redo', 'delete:': 'delete'}[action];
        return {ok: !!command && document.execCommand(command)};
    };
    const run = (...args) => ({...edit(...args), context: context()});
    const rebase = (original, completed) => {
        if (!sameDocumentTarget(original) || original.activity !== state.activity || !matches(completed)) {
            return {cancel: true, reason: 'queued-command-context-changed'};
        }
        return {context: context()};
    };
    window.__okilumKeysV2 = {document, context, observe, run, rebase, reportContext: () => report('context')};
    report('context');
})();
