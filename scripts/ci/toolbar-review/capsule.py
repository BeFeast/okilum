#!/usr/bin/env python3
"""Prepare source-pinned observer-only capsules. Never change the PR checkout."""
import argparse
import difflib
import hashlib
import json
from pathlib import Path
import subprocess

MAIN = '774cef4abf61d39147069918f8ba8a1000008187'
TOOLBAR = '863bfdd3a50f699087f5d81d154dd59d1cdcd202'
ROOT = Path(__file__).resolve().parents[3]
SRC = 'crates/okilum-shell/src/'


def git(*args, cwd=ROOT):
    return subprocess.check_output(['git', *args], cwd=cwd)


def replace(text, old, new, count=1):
    assert text.count(old) == count, (old, text.count(old), count)
    return text.replace(old, new, count)


def prepare(destination, kind, in_place=False):
    destination = destination.resolve()
    base = MAIN if kind == 'main' else TOOLBAR
    controller_sha = git('rev-parse','HEAD').decode().strip()
    observer = (ROOT/'scripts/ci/toolbar-review/observer.rs').read_text(encoding="utf-8")
    main_patch = (ROOT/'scripts/ci/toolbar-review-main-observers.diff').read_bytes()
    if in_place:
        assert destination == ROOT
        assert not git('status','--porcelain').strip(), 'in-place capsule requires a clean disposable checkout'
        subprocess.run(['git','checkout','--detach',base],cwd=ROOT,check=True)
    else:
        subprocess.run(['git', 'worktree', 'add', '--detach', str(destination), base], cwd=ROOT, check=True)
    original = {}

    def write(name, raw):
        path = destination / name
        original.setdefault(name, path.read_bytes() if path.exists() else b'')
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw if isinstance(raw, bytes) else raw.encode())

    def edit(name, old, new):
        write(name, replace((destination / name).read_text(encoding="utf-8"), old, new))

    # Match public benchmark support and its diagnostic counters, not viewer behavior.
    if kind == 'main':
        for name in ['Cargo.lock', 'crates/okilum-shell/Cargo.toml',
                     SRC+'platform/shaped_line_cache.rs', SRC+'platform/text_diagnostics.rs']:
            write(name, git('show', TOOLBAR+':'+name))
        edit(SRC+'reader_code_file.rs', 'pub(crate) mod native_harness;',
             'pub(crate) mod native_harness;\n#[cfg(feature = "data-view-native-harness")]\nmod toolbar_native;')
        edit(SRC+'reader_code_file/native_harness.rs', 'pub(crate) fn entry() -> bool {',
             'pub(crate) fn entry() -> bool {\n    if super::toolbar_native::entry() { return true; }')
        edit(SRC+'reader_code_file.rs',
             'fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {',
             'fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {\n        #[cfg(feature="data-view-native-harness")]\n        let _work = toolbar_native::trace::Work::start(12);')
        edit(SRC+'reader_code_file.rs', '        view.into_any_element()\n    }\n}\n\nimpl CodePreview {',
             '        let view = view.into_any_element();\n        #[cfg(feature="data-view-native-harness")]\n        let view = toolbar_native::trace::Trace::new(view, 0).into_any_element();\n        view\n    }\n}\n\nimpl CodePreview {')
        name = SRC+'reader_code_file/structure.rs'
        original.setdefault(name, (destination/name).read_bytes())
        subprocess.run(['git','apply','-'],input=main_patch,cwd=destination,check=True)
        text = (destination/name).read_text(encoding="utf-8")
        assert 'pub(super) fn folded(&self, index: usize) -> bool {' in text
        text = replace(text, '    pub(super) fn structure_element(&self, cx: &mut Context<Self>) -> AnyElement {\n        div()',
                       '    pub(super) fn structure_element(&self, cx: &mut Context<Self>) -> AnyElement {\n        let tree = div()')
        text = replace(text, '            .into_any_element()\n    }\n\n    fn structure_row',
                       '            .into_any_element();\n        toolbar_native::trace::Trace::new(tree,1).into_any_element()\n    }\n\n    fn structure_row')
        start = text.index('    fn structure_row')
        end = text.index('\n}\n\n#[cfg(test)]', start)
        row = text[start:end]
        row = replace(row, '        let doc = self.document.as_ref().unwrap();',
                      '        let _work = toolbar_native::trace::Work::start(13);\n        let doc = self.document.as_ref().unwrap();')
        row = replace(row, '        h_flex()', '        let element = h_flex()')
        row = replace(row, '            .into_any_element()\n    }',
                      '            .into_any_element();\n        toolbar_native::trace::Trace::new(element,2).into_any_element()\n    }')
        write(name, text[:start]+row+text[end:])
        write(SRC+'reader_code_file/toolbar_native/trace.rs',
              git('show', TOOLBAR+':'+SRC+'reader_code_file/toolbar_native/trace.rs'))

    harness = git('show', TOOLBAR+':'+SRC+'reader_code_file/toolbar_native.rs').decode()
    harness = replace(harness,
        '#[cfg(not(target_os = "windows"))]\nfn thread_cpu_ms() -> Option<f64> {\n    None\n}',
        observer)
    harness = replace(harness, '    let directory = PathBuf::from(directory);',
        '    let census = std::env::var_os("OKILUM_DIAG_CENSUS").is_some();\n    let directory = PathBuf::from(directory);\n    diagnostic_println!("CODE2810_ID pid={} tid={} base={} controller={} wall_unit={}", std::process::id(), native_thread_id(), env!("OKILUM_DIAG_SOURCE_BASE"), env!("OKILUM_SOURCE_COMMIT"), if cfg!(target_os="windows") {"filetime-100ns"} else {"unix-ns"});')
    harness = replace(harness,
        '                let paths=(0..doc.structure.len()).find(|i|doc.key_text(*i)==Some("paths")).unwrap();\n                assert_eq!(doc.container(paths),Some((document::ContainerKind::Object,414)));',
        '                let paths=doc.structure.iter().position(|row|doc.text[row.range.clone()].starts_with("\\\"paths\\\":" )).unwrap();\n                assert_eq!(doc.structure.iter().filter(|row|row.parent==Some(paths)).count(),414);')
    harness = replace(harness, 'smoke || open_ms<=100.', 'smoke || census || open_ms<=100.')
    harness = replace(harness,
        '                    let target=code.read(cx).find_target(code.clone(),cx);\n                    target.set_query(if marked {"description"} else {""},w,cx);',
        '                    code.read(cx).find.update(cx,|input,cx|{input.set_value(if marked {"description"} else {""},w,cx);cx.emit(gpui_component::input::InputEvent::Change);});')
    harness = replace(harness,
        '                    if marked {assert_eq!(p.scrollbar_marks.len(),1000,"positive control: capped marks actually present");}\n                    else {assert!(p.scrollbar_marks.is_empty());}',
        '                    if marked {assert_eq!(p.matches.len(),1490,"same complete literal query in both capsules");}\n                    else {assert!(p.matches.is_empty());}')
    harness = replace(harness, '                        let cpu_before=thread_cpu_ms();\n                        let text_probe=platform::text_diagnostics::Probe::start();', '                        let text_probe=platform::text_diagnostics::Probe::start();\n                        let cpu_before=thread_cpu_ms();')
    harness = replace(harness, '                        let frame=Instant::now();',
        '                        let bracket_start=wall_stamp();\n                        let frame=Instant::now();')
    harness = replace(harness, '                        let ms=frame.elapsed().as_secs_f64()*1000.;',
        '                        let ms=frame.elapsed().as_secs_f64()*1000.;\n                        let cpu_after=thread_cpu_ms();\n                        let bracket_end=wall_stamp();')
    harness = replace(harness, 'let cpu_ms=thread_cpu_ms().zip(cpu_before)', 'let cpu_ms=cpu_after.zip(cpu_before)')
    harness = replace(harness, '                        let subtree_work=trace::report();',
        '                        let mut subtree_work=trace::report();\n                        subtree_work["native_memory_and_cycles"]=serde_json::json!({"tid":native_thread_id(),"pid":std::process::id(),"wall_bounds_filetime_100ns":if cfg!(target_os="windows") {Some([bracket_start,bracket_end])} else {None},"wall_bounds_unix_ns":if cfg!(target_os="windows") {None} else {Some([bracket_start,bracket_end])}});')
    harness = replace(harness, 'smoke || ms<=16.', 'smoke || census || ms<=16.')
    harness = replace(harness, 'assert!(scroll_rasterizations<=1,', 'assert!(census || scroll_rasterizations<=1,')
    # Capture all frames before recording the unchanged per-frame verdict in census mode.
    harness = replace(harness, '            cx.update(|cx|cx.quit());',
        '            let misses=frames.iter().filter(|frame|frame["cpu_draw_ms"].as_f64().unwrap()>16.).count();\n            diagnostic_println!("CODE2810_CENSUS frames={} misses={} strict_wall_pass={}",frames.len(),misses,open_ms<=100. && misses==0);\n            cx.update(|cx|cx.quit());')
    # serde_json::json borrows frames; it remains available for the census above.
    write(SRC+'reader_code_file/toolbar_native.rs', harness)
    changes = []
    for name, before in original.items():
        after = (destination/name).read_bytes()
        changes.append({'path':name,'before_sha256':hashlib.sha256(before).hexdigest(),
                        'after_sha256':hashlib.sha256(after).hexdigest()})
    diff = ''.join(''.join(difflib.unified_diff(before.decode().splitlines(True),
            (destination/name).read_text(encoding="utf-8").splitlines(True),fromfile=name,tofile=name))
            for name,before in original.items())
    manifest = {'base_source':base,'controller_source':controller_sha,
                'kind':kind,'changed_files':changes,'fixture_source':TOOLBAR,
                'rules':'observer/harness support only; no viewer styles, cache, shaping, priority, stack or production gate changes'}
    (destination/'capsule-manifest.json').write_text(json.dumps(manifest,indent=2))
    (destination/'capsule.diff').write_text(diff)
    print(json.dumps(manifest,indent=2))


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind',choices=['main','toolbar'])
    parser.add_argument('destination',type=Path)
    parser.add_argument('--in-place',action='store_true',help='only a clean disposable CI checkout')
    args=parser.parse_args()
    prepare(args.destination,args.kind,args.in_place)
