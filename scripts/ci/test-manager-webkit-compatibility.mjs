// Run the shipped preflight, including failures that would otherwise abort T3
// module parsing. This is a capability regression, not native UI acceptance.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const html = fs.readFileSync('crates/okilum-shell/src/reader_manager_compatibility.html', 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const features = [
    ['(?<!', 'negative lookbehind'],
    ['(?<=', 'positive lookbehind'],
    ['(?<name>', 'named capture groups'],
];
function run(RegExpConstructor) {
    const messages = [];
    // No location, fetch, or T3 modules: navigation belongs to the native host
    // only after it consumes a successful capability report.
    vm.runInNewContext(script, {
        RegExp: RegExpConstructor,
        window: { ipc: { postMessage: message => messages.push(message) } },
    });
    assert.equal(messages.length, 1, 'every run must produce exactly one observable result');
    return messages[0];
}

assert.equal(run(RegExp), 'webkit-capability:supported', 'positive control: a supported engine proceeds');
for (const [prefix, feature] of features) {
    function OlderEngine(pattern) {
        if (pattern.startsWith(prefix)) throw new SyntaxError('Invalid regular expression: invalid group specifier name');
        return new RegExp(pattern);
    }
    assert.equal(run(OlderEngine), `webkit-capability:unsupported:${feature}: SyntaxError: Invalid regular expression: invalid group specifier name`);
}
function IncorrectMatching() { return { test: () => true }; }
assert.match(run(IncorrectMatching), /unsupported:negative lookbehind: Error: incorrect matching$/);
function MissingNamedCapture(pattern) {
    return pattern.startsWith('(?<name>') ? { exec: () => ['b'] } : new RegExp(pattern);
}
assert.match(run(MissingNamedCapture), /unsupported:named capture groups: Error: incorrect capture$/);
console.log('Manager WebKit preflight: supported engine and five incompatible-engine cases passed');
