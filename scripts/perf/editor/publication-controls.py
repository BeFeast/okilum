#!/usr/bin/env python3
"""Overlay identical allocation/cache probes on a pinned main, on one CI host.

Run after vendor setup and dependency fetch. Does not mutate the worker checkout;
only the synthetic probe and instrumentation are overlaid on archived main.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def function(source, name):
    start = source.index('    #[gpui::test]\n    fn ' + name)
    end = source.find('    #[gpui::test]', start + 10)
    return source[start:end]


def main():
    base = sys.argv[1]
    root = Path.cwd()
    env = dict(os.environ, CARGO_TARGET_DIR=str(root / 'target'))
    baseline = subprocess.check_output(['git', 'rev-parse', base], text=True).strip()
    print(f'EDITOR_CONTROL_BASELINE={baseline}', flush=True)
    with tempfile.TemporaryDirectory(prefix='publication-control-') as directory:
        dest = Path(directory)
        archive = subprocess.Popen(['git', 'archive', baseline], stdout=subprocess.PIPE)
        run('tar', '-x', '-C', str(dest), stdin=archive.stdout)
        archive.stdout.close()
        if archive.wait():
            raise RuntimeError('baseline archive failed')
        shutil.rmtree(dest / 'vendor', ignore_errors=True)
        (dest / 'vendor').symlink_to(root / 'vendor', target_is_directory=True)
        editor = Path('crates/okilum-shell/src/reader_editor.rs')
        current = (root / editor).read_text()
        old = (dest / editor).read_text()
        if 'pub(crate) mod work;' not in old:
            old = old.replace('mod block_context;', 'mod block_context;\n#[cfg(test)]\npub(crate) mod work;')
        meter = (root / editor.parent / 'reader_editor/work.rs').read_text()
        # Archived main retains its original allocator. Only the accounting
        # scopes/counters are overlaid; it need not depend on head's backend.
        baseline_main = (dest / editor.parent / 'main.rs').read_text()
        if not re.search(r'const\s+NATIVE_ALLOCATION\b', baseline_main):
            meter = meter.replace('use crate::NATIVE_ALLOCATION;',
                                  'const NATIVE_ALLOCATION: std::alloc::System = std::alloc::System;')
        (dest / editor.parent / 'reader_editor/work.rs').write_text(meter)
        # Keep existing key scopes, including their deliberate early drop before
        # GPUI scheduling. A duplicate outer scope would measure a wider callback
        # on main than on head. Add scopes only where main has none.
        acknowledgement = re.search(r'    fn autosaved\([\s\S]*?\n    \) \{', old)
        if acknowledgement is None:
            raise RuntimeError('baseline autosave acknowledgement boundary changed')
        for marker in [
            'let changed = cx.subscribe(&input, move |this, input, mutation: &SourceMutation, cx| {',
            'pub(super) fn refresh_source_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {',
            acknowledgement.group(0),
        ]:
            if marker not in old:
                raise RuntimeError('baseline probe boundary changed: ' + marker)
            boundary = old.index(marker) + len(marker)
            if not re.match(r'\s*#\[cfg\(test\)\]\s*let _work = work::scope\(\);', old[boundary:]):
                old = old[:boundary] + '\n            let _publication_control_work = work::scope();' + old[boundary:]
        if 'static LIVE_PREPARES:' not in old:
            old = old.replace('mod block_context;', 'thread_local! { static LIVE_PREPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }\nmod block_context;', 1)
            marker = '                *entry = (\n                    input,'
            if marker not in old:
                raise RuntimeError('baseline prepared-cache probe boundary changed')
            old = old.replace(marker, '                LIVE_PREPARES.with(|count| count.set(count.get() + 1));\n' + marker)
        probes = ['issue2404_key_has_no_note_sized_foreground_allocation_or_reader_notification',
                  'issue2405_autosave_publication_work_and_prepared_cache']
        # Remove an existing probe before installing the exact current one.
        for name in probes:
            if '    fn ' + name in old:
                old = old.replace(function(old, name), '')
        pos = old.index('mod autosave_tests {') + len('mod autosave_tests {')
        old = old[:pos] + '\n' + ''.join(function(current, name) for name in probes) + old[pos:]
        (dest / editor).write_text(old)
        for rel, marker in [
            ('reader_incremental.rs', '                    match result {'),
            ('reader_session.rs', '        if self.loading.is_none() {'),
        ]:
            path = dest / editor.parent / rel
            text = path.read_text()
            if marker not in text:
                raise RuntimeError('baseline publication probe boundary changed')
            text = text.replace(marker, '        let _publication_control_work = reader_editor::work::scope();\n' + marker, 1)
            path.write_text(text)
        # Invalidate path-package outputs on both checkout transitions; Cargo
        # artifact identities alone do not distinguish these workspaces.
        run('cargo', 'clean', '--offline', '--locked', '-p', 'okilum-core', '-p', 'okilum-shell', cwd=dest, env=env)
        # Compilation success is mandatory: a compile failure is not a positive control.
        run('cargo', 'test', '--offline', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', '--no-run', cwd=dest, env=env)
        for name in probes:
            result = subprocess.run(['cargo', 'test', '--offline', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', name, '--', '--nocapture'], cwd=dest, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            print(result.stdout, flush=True)
            print(f'EDITOR_BASELINE_PROBE name={name} exit={result.returncode}', flush=True)
            if name.startswith('issue2405') and result.returncode == 0:
                raise RuntimeError('main publication positive control unexpectedly passed')
            if result.returncode not in (0, 101):
                raise RuntimeError('baseline produced no test verdict')
            if name.startswith('issue2405'):
                if 'own-save publication regression:' not in result.stdout:
                    raise RuntimeError('baseline failed before the intended publication assertions')
                for size in [25 * 1024, 130 * 1024, 1024 * 1024]:
                    for shared in ['false', 'true']:
                        if f'EDITOR_SAVE_WORK bytes={size} shared={shared}' not in result.stdout:
                            raise RuntimeError('baseline did not exercise every publication route and size')
            elif result.returncode and 'bounded per-key UI work failed:' not in result.stdout:
                raise RuntimeError('baseline key control did not reach its intended assertions')
    # Cargo can reuse a path package's artifact identity across two checkouts
    # in one target directory. Main's newer build output must never masquerade
    # as head's core/shell merely because head's unchanged files are older.
    # Dependencies stay warm, but these workspace packages must rebuild.
    run('cargo', 'clean', '--offline', '--locked', '-p', 'okilum-core', '-p', 'okilum-shell', env=env)
    # Exact final-head probes on the same host/session. No cross-run timing claims.
    run('cargo', 'test', '--offline', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', 'issue240', '--', '--nocapture', env=env)


if __name__ == '__main__':
    main()
