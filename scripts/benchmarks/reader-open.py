#!/usr/bin/env python3
"""Builder-only same-job comparison; never consumes a user's vault."""
import os
from pathlib import Path
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[2]
if not os.environ.get('CI'):
    raise SystemExit('This benchmark is builder-only (CI required).')
head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()
pins = [('shipped', '500077c632593dba7dfbbb95659015ed95145f7b'),
        ('frozen338', '701ae2256e0c785d3157059561a5a73b8761c83b'), ('candidate', head)]
template = (repo / 'scripts/benchmarks/reader-open.rs').read_text()
with tempfile.TemporaryDirectory(prefix='reader-profile-', dir=os.environ.get('RUNNER_TEMP')) as work:
    for kind, pin in pins:
        source = Path(work) / kind
        source.mkdir()
        archive = subprocess.Popen(['git', 'archive', pin], cwd=repo, stdout=subprocess.PIPE)
        subprocess.run(['tar', '-x', '-C', str(source)], stdin=archive.stdout, check=True)
        archive.stdout.close()
        if archive.wait():
            raise RuntimeError('git archive failed')
        (source / 'vendor').mkdir(exist_ok=True)
        (source / 'vendor/gpui-component').symlink_to(repo / 'vendor/gpui-component', target_is_directory=True)
        text = template.replace('// API_SCAN',
            'let (vault, _sources) = Vault::scan_snapshot_with(&root, &mut |_, _| Ok(())).unwrap();' if kind == 'candidate'
            else 'let vault = Vault::scan(&root).unwrap();')
        text = text.replace('// API_SNAPSHOT',
            'let captured = _sources; assert_eq!(captured.len(), 5001);' if kind == 'candidate'
            else 'let captured: Vec<_> = vault.notes.iter().map(|note| std::fs::read(root.join(&note.path)).unwrap()).collect(); assert_eq!(captured.len(), 5001);')
        first = 'prepare(&vault); let first_ms = total.elapsed().as_secs_f64() * 1000.;'
        if kind == 'shipped':
            text = text.replace('// API_LATE_FIRST', first)
        else:
            text = text.replace('// API_FIRST: production prepares first document after the full scan/index.',
                'let mut first = Vault::from_note_paths(["note0000.md".to_owned()]); first.root = root.clone(); prepare(&first); let first_ms = total.elapsed().as_secs_f64() * 1000.;')
        test = source / 'crates/tessera-core/tests/reader_open_profile.rs'
        test.write_text(text)
        env = dict(os.environ, READER_PROFILE_PIN=pin, CARGO_TARGET_DIR=str(repo / 'target'))
        subprocess.run(['cargo', 'test', '--locked', '-p', 'tessera-core', '--test', 'reader_open_profile', '--', '--nocapture'], cwd=source, env=env, check=True)
