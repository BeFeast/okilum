"""Hosted-only source mutation controls for #2519; never shipped in its PR."""
import os
from pathlib import Path
import subprocess

assert os.uname().sysname == 'Darwin'
assert os.environ.get('GITHUB_ACTIONS') == 'true'
source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
assert source == os.environ['PAIRING_SOURCE_SHA'], source
common = ['cargo', 'test', '--release', '--locked', '--target', 'aarch64-apple-darwin']
tests = [
    (['-p', 'okilum-t3', '--lib'], 'stream::tests::pairing_reports_build_target_os'),
    (['--workspace', '--bins'], 'project::credentials::tests::selected_native_manager_restores_stored_token'),
    (['--workspace', '--bins'], 'project::credentials::tests::selected_native_manager_restores_native_token_on_connection'),
    (['--workspace', '--bins'], 'project::native::credential_tests::pairing_persists_before_connecting_and_restores_without_other_sessions'),
]

def execute(label, expected_failure):
    for selection, name in tests:
        result = subprocess.run(common + selection + [name, '--', '--exact', '--test-threads=1'],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = result.stdout
        print(log, flush=True)
        Path(os.environ['RUNNER_TEMP'], label + '-' + name.replace('::', '-') + '.log').write_text(log)
        status = 'FAILED' if expected_failure else 'ok'
        assert f'test {name} ... {status}' in log, (name, 'missing final verdict', result.returncode)
        assert result.returncode == (101 if expected_failure else 0), (name, result.returncode)
        assert ('1 failed;' if expected_failure else '1 passed;') in log, (name, 'missing count')
        print(f'CONTROL {label}: {name}: {status}', flush=True)

execute('head', False)
mutations = {
    'crates/okilum-t3/src/stream/auth.rs': [('("client_os", std::env::consts::OS)', '("client_os", "linux")')],
    'crates/okilum-shell/src/project/credentials.rs': [('cfg!(target_os = "macos") && !native_panel', 'cfg!(target_os = "macos")')],
    'crates/okilum-shell/src/project.rs': [('if self.threads.enabled {\n                    self.native_restore(false, window, cx);', 'if cfg!(target_os = "linux") {\n                    self.native_restore(false, window, cx);')],
    'crates/okilum-shell/src/project/native.rs': [('''                        if this.threads.enabled {
                            this.native.open = true;
                            this.connection_details = true;
                        }
''', '')],
}
originals = {}
try:
    for name, replacements in mutations.items():
        path = Path(name)
        originals[path] = path.read_text()
        text = originals[path]
        for old, new in replacements:
            assert text.count(old) == 1, (name, old, text.count(old))
            text = text.replace(old, new)
        path.write_text(text)
    execute('without-fix', True)
finally:
    for path, text in originals.items():
        path.write_text(text)
execute('restored-head', False)
assert subprocess.run(['git', 'diff', '--exit-code']).returncode == 0
print('All four regressions fail without the corresponding fix and pass after restoration.', flush=True)
