"""Unsigned fork routing with fake ELF, command stubs and local Git fixtures only."""
import argparse
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fork = load('unsigned_fork', ROOT / 'scripts/arch/fork-production.py')
f = load('fork_fixtures', ROOT / 'scripts/ci/test_linux_binary.py')
publication = load('fork_publication', ROOT / 'scripts/releases/publication.py')
publisher = load('fork_arch_publisher', ROOT / 'scripts/arch/publish.py')
selection = load('fork_selection', ROOT / 'scripts/arch/select-source.py')
IDENTITY = dict(repository='BeFeast/okilum', head_repository='contributor/okilum',
    workflow=fork.WORKFLOW, event='pull_request', source_sha=f.SHA, source_kind='head',
    head_sha=f.SHA, base_sha='b' * 40, pr_number=17, run_id=19, run_number=23)
BUILD = 5023


def inspector(binary, manifest):
    fork.p.require(binary.read_bytes() == f.binary_bytes(), 'Stub compiler bytes changed')
    abi = copy.deepcopy(f.manifest()['elf'])
    abi['build_info'].update(build=str(manifest['build']), version=f"0.1.{manifest['build']}")
    abi['build_info']['source'] = manifest['source_sha']
    return abi


def executable(path, source):
    path.write_text('#!' + sys.executable + '\n' + source)
    path.chmod(0o755)


class ForkProduction(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.out = self.root / 'dist/linux-fork'
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.counter = self.root / 'compiler-count'
        executable(self.tools / 'cargo', '''import os,pathlib
root = pathlib.Path(os.environ['FORK_FIXTURE_ROOT'])
with pathlib.Path(os.environ['COMPILER_COUNTER']).open('a') as f: f.write('compile\\n')
binary = root / 'target/release/okilum'
binary.parent.mkdir(parents=True, exist_ok=True)
binary.write_bytes(bytes.fromhex(os.environ['FAKE_ELF']))
''')
        self.environment = patch.dict(os.environ, {
            'PATH': str(self.tools) + os.pathsep + os.environ['PATH'],
            'FORK_FIXTURE_ROOT': str(self.root), 'COMPILER_COUNTER': str(self.counter),
            'FAKE_ELF': f.binary_bytes().hex(), 'GITHUB_RUN_ATTEMPT': '1',
        })
        self.environment.start()
        self.addCleanup(self.environment.stop)
        for key in list(os.environ):
            if key.startswith('CARGO_PROFILE_RELEASE_') or key in ('CARGO_ENCODED_RUSTFLAGS',
                'CARGO_BUILD_TARGET', 'CARGO_BUILD_RUSTFLAGS', 'RUSTC_WRAPPER',
                'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP'):
                os.environ.pop(key)
        for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md'):
            (self.root / name).write_text('fixture notice')
        for obj, name, value in ((fork, 'ROOT', self.root), (fork.p, 'ROOT', self.root),
            (fork, 'provenance', lambda: dict(rustc='fixture', cargo='fixture', toolchain='fixture',
                cargo_lock='d' * 64, vendor='e' * 40, os_release='Arch fixture', rustflags=fork.p.RUSTFLAGS))):
            stub = patch.object(obj, name, value)
            stub.start()
            self.addCleanup(stub.stop)

    def produce(self):
        return fork.produce(self.out, IDENTITY, f.CONFIG, inspector)

    def test_one_production_compilation_reuse_and_no_secret_requirement(self):
        with patch.dict(os.environ, {'MIRROR_TOKEN': '', 'GH_TOKEN': '', 'FORGEJO_TOKEN': ''}):
            self.assertTrue(self.produce())
            self.assertFalse(self.produce())
            fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        self.assertEqual(self.counter.read_text(), 'compile\n')
        receipt = json.loads((self.out / 'receipt.json').read_text())
        self.assertEqual(receipt['schema'], fork.SCHEMA)
        self.assertFalse(receipt['publication_eligible'])
        self.assertNotIn('artifact_api_digest', receipt)
        self.assertNotEqual(fork.artifact_name(f.SHA, f.CONFIG), fork.p.artifact_name(f.SHA, f.CONFIG))
        # Positive detector control: a second direct compiler invocation is seen.
        subprocess.run(['cargo', 'build'], check=True)
        self.assertEqual(self.counter.read_text(), 'compile\ncompile\n')

    def test_failed_attempt_or_missing_payload_cannot_trigger_consumer_compile(self):
        self.out.mkdir(parents=True)
        (self.out / 'attempt.json').write_text('{}')
        with self.assertRaises(FileExistsError):
            self.produce()
        self.assertFalse(self.counter.exists())
        with self.assertRaisesRegex(ValueError, 'receipt'):
            fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        subprocess.run(['cargo', 'build'], check=True)
        self.assertTrue(self.counter.exists())

    def test_rerun_without_bytes_and_profile_overrides_do_not_compile(self):
        with patch.dict(os.environ, {'GITHUB_RUN_ATTEMPT': '2'}):
            with self.assertRaisesRegex(ValueError, 'rerun'):
                self.produce()
        with patch.dict(os.environ, {'CARGO_PROFILE_RELEASE_STRIP': 'true'}):
            with self.assertRaisesRegex(ValueError, 'override'):
                self.produce()
        self.assertFalse(self.counter.exists())

    def test_tampered_payload_receipt_archive_source_and_config_fail_closed(self):
        self.produce()
        saved = (self.out / 'receipt.json').read_bytes()
        # Binary-only tamper, then the same locally editable receipt-hash bypass
        # as transport finding 1: both must fail against the checked archive.
        binary = self.out / 'payload/okilum'
        binary.write_bytes(f.binary_bytes() + b'changed')
        for change_hash in (False, True):
            receipt = json.loads(saved)
            if change_hash:
                receipt['manifest']['binary_sha256'] = fork.p.digest(binary.read_bytes())
            (self.out / 'receipt.json').write_bytes(fork.p.encode(receipt))
            with self.subTest(change_hash=change_hash), self.assertRaises(ValueError):
                fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        binary.write_bytes(f.binary_bytes())
        (self.out / 'receipt.json').write_bytes(saved)
        fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        for identity, config in ((dict(IDENTITY, source_sha='b' * 40), f.CONFIG),
                                 (dict(IDENTITY, run_id=20), f.CONFIG), (IDENTITY, 'd' * 64)):
            with self.assertRaises(ValueError):
                fork.verify(self.out, identity, config, inspector)
        archive = next(self.out.glob('*.tar.zst'))
        original = archive.read_bytes()
        archive.write_bytes(original + b'corruption')
        with self.assertRaisesRegex(ValueError, 'archive changed'):
            fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        archive.write_bytes(original)
        binary.unlink()
        binary.symlink_to(archive)
        with self.assertRaises(ValueError):
            fork.verify(self.out, IDENTITY, f.CONFIG, inspector)
        self.assertEqual(self.counter.read_text(), 'compile\n')

    def test_canonical_validators_reject_every_fork_payload_without_opt_in(self):
        self.produce()
        receipt = json.loads((self.out / 'receipt.json').read_text())
        with self.assertRaises(ValueError):
            fork.p.validate_receipt(receipt, f.SHA, f.CONFIG)
        archive = next(self.out.glob('*.tar.zst'))
        with self.assertRaises(ValueError):
            fork.p.unpack(archive.read_bytes(), archive.with_suffix('.zst.sha256').read_text(),
                self.root / 'trusted', f.SHA, f.CONFIG, inspector=inspector)
        for key, value in [('publication_eligible', True), ('schema', 1), ('artifact_id', 7)]:
            bad = copy.deepcopy(receipt)
            bad[key] = value
            with self.assertRaises(ValueError):
                fork.validate_receipt(bad, f.SHA, f.CONFIG)

    def test_real_build_entrypoint_packages_identical_fork_bytes_and_never_compiles(self):
        self.produce()
        source = self.root / 'scripts/arch'
        source.mkdir(parents=True)
        for name in ('build.sh', 'PKGBUILD', 'okilum.install'):
            shutil.copyfile(ROOT / 'scripts/arch' / name, source / name)
        # The build entrypoint executes the real helper under stubbed inspection
        # and event/config lookup; context itself is covered by real Git below.
        executable(self.tools / 'python3', f'''import importlib.util,json,pathlib
spec = importlib.util.spec_from_file_location('fork_test_helper', {str(ROOT / 'scripts/arch/fork-production.py')!r})
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
spec = importlib.util.spec_from_file_location('fork_test_fixture', {str(Path(__file__).resolve())!r})
t = importlib.util.module_from_spec(spec); spec.loader.exec_module(t)
m.ROOT = pathlib.Path({str(self.root)!r})
m.p.inspect = t.inspector
m.package(pathlib.Path({str(self.out)!r}), {IDENTITY!r}, {f.CONFIG!r})
''')
        executable(self.tools / 'makepkg', '''import io,json,os,pathlib,subprocess,tarfile
root = pathlib.Path(os.environ['OKILUM_PREBUILT_DIR'])
receipt = json.loads((root / 'receipt.json').read_text())
raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode='w') as a:
    binary = (root / 'payload/okilum').read_bytes() + (b'stripped' if os.environ.get('STRIP_CONTROL') else b'')
    for name, value in [('usr/bin/okilum', binary),
        ('usr/share/okilum/production-receipt.json', (root / 'receipt.json').read_bytes())]:
        entry = tarfile.TarInfo(name); entry.size = len(value); a.addfile(entry, io.BytesIO(value))
pathlib.Path('okilum-0.1.%s-1-x86_64.pkg.tar.zst' % receipt['manifest']['build']).write_bytes(
    subprocess.check_output(['zstd', '-q', '-c'], input=raw.getvalue()))
''')
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                        'commit', '--allow-empty', '-qm', 'fixture'], check=True)
        env = dict(os.environ, OKILUM_FORK_VALIDATION='1', OKILUM_SETTINGS_UI_HARNESS='0', OKILUM_PREBUILT_DIR=str(self.out))
        subprocess.run(['bash', str(source / 'build.sh')], cwd=self.root, env=env, check=True, capture_output=True)
        path = next((self.root / 'dist/arch').glob('*.pkg.tar.zst'))
        receipt, binary = fork.package_receipt(path, f.SHA, BUILD)
        self.assertEqual(binary, f.binary_bytes())
        self.assertEqual(receipt, json.loads((self.out / 'receipt.json').read_text()))
        self.assertTrue((self.root / 'dist/arch/SHA256SUMS').is_file())
        self.assertEqual(self.counter.read_text(), 'compile\n')
        # The same package cannot enter the default trusted validator/publisher.
        with self.assertRaises(ValueError):
            fork.p.package_receipt(path, f.SHA, BUILD)
        store, key = Mock(), Mock()
        args = argparse.Namespace(command='publish', package=str(path), source=f.SHA, build=BUILD)
        with self.assertRaises(ValueError):
            publisher.execute(args, store, key, self.root)
        store.call.assert_not_called()
        store.put.assert_not_called()
        key.sign.assert_not_called()
        # A package transformation must fail before upload/install. The compiler
        # count remains unchanged even on this failed packaging invocation.
        stripped = subprocess.run(['bash', str(source / 'build.sh')], cwd=self.root,
            env=dict(env, STRIP_CONTROL='1'), capture_output=True)
        self.assertNotEqual(stripped.returncode, 0)
        self.assertIn(b'Package/source/build/binary drift', stripped.stderr)
        self.assertEqual(self.counter.read_text(), 'compile\n')


class ForkContextAndRouting(unittest.TestCase):
    def test_exact_head_and_synthetic_merge_are_distinct_and_mismatches_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            def git(*args, **kwargs):
                return subprocess.check_output(['git', '-C', str(root), '-c', 'user.name=Fixture',
                    '-c', 'user.email=fixture@example.invalid', *args], text=True, **kwargs).strip()
            tree = git('mktree', input='')
            base = git('commit-tree', tree, input='base\n')
            head = git('commit-tree', tree, '-p', base, input='head\n')
            merge = git('commit-tree', tree, '-p', base, '-p', head, input='merge\n')
            event = dict(number=17, pull_request=dict(base=dict(sha=base, repo=dict(full_name='BeFeast/okilum')),
                head=dict(sha=head, repo=dict(full_name='contributor/okilum'))))
            event_path = root / 'event.json'
            event_path.write_text(json.dumps(event))
            env = dict(GITHUB_EVENT_NAME='pull_request', GITHUB_EVENT_PATH=str(event_path),
                GITHUB_REPOSITORY='BeFeast/okilum', GITHUB_RUN_ID='19', GITHUB_RUN_NUMBER='23')
            real_output = fork.output
            def command(*args):
                return git(*args[1:]) if args[0] == 'git' else real_output(*args)
            with patch.dict(os.environ, env), patch.object(fork, 'output', side_effect=command):
                for source, kind in ((head, 'head'), (merge, 'merge')):
                    git('update-ref', 'HEAD', source)
                    os.environ['GITHUB_SHA'] = source
                    identity = fork.context()
                    self.assertEqual(identity['source_sha'], source)
                    self.assertEqual(identity['source_kind'], kind)
                    self.assertEqual(identity['head_sha'], head)
                os.environ['GITHUB_SHA'] = head
                with self.assertRaisesRegex(ValueError, 'event source'):
                    fork.context()
                os.environ['GITHUB_SHA'] = merge
                event['pull_request']['base']['sha'] = 'b' * 40
                event_path.write_text(json.dumps(event))
                with self.assertRaisesRegex(ValueError, 'declared base/head merge'):
                    fork.context()
                event['pull_request']['head']['repo']['full_name'] = 'BeFeast/okilum'
                event_path.write_text(json.dumps(event))
                with self.assertRaisesRegex(ValueError, 'trusted sources'):
                    fork.context()

    def test_protected_run_metadata_rejects_forks_even_with_forged_valid_hosted_receipt(self):
        # Positive control: the supplied forged local fields are fully accepted
        # by the structural hosted validator; eligibility must use API run facts.
        forged = f.receipt()
        # Isolate external authority in this structural positive control on both
        # base and transport-fixed interfaces. No trusted_envelope bypass.
        with patch.object(fork.p, 'authenticate_receipt', create=True):
            fork.p.validate_receipt(forged, f.SHA, f.CONFIG)
        trusted = dict(workflow_id='linux-release.yml', prettyref='main', is_fork_pull_request=False,
                       trigger_event='schedule', status='success', commit_sha=f.SHA)
        self.assertTrue(publication.eligible(trusted, 'linux', f.SHA))
        for changes in ({'is_fork_pull_request': True}, {'trigger_event': 'pull_request'}, {'prettyref': 'fork'}):
            api = Mock()
            api.call.side_effect = [dict(trusted, **changes), {'commit': {'id': f.SHA}}]
            with self.subTest(changes=changes), patch.object(publication.subprocess, 'run') as launch, \
                    patch.object(publication, 'production_build', return_value=forged['manifest']['build']) as read_receipt:
                with self.assertRaisesRegex(ValueError, 'trusted main'):
                    publication.publish(api, 'linux', 19)
                launch.assert_not_called()
                read_receipt.assert_not_called()
                self.assertEqual(api.call.call_count, 2)

    def test_existing_workflow_routes_forks_without_weakening_gates_or_guard(self):
        # Keep the ordinary Arch gate stdlib-only; CI images do not install PyYAML.
        workflow = (ROOT / '.forgejo/workflows/linux-release.yml').read_text()
        release = workflow.split('  release:\n', 1)[1].split('  arch-github:\n', 1)[0]
        steps = re.split(r'\n      - ', release)
        fork_step = next(s for s in steps if s.startswith('name: Produce isolated unsigned fork bytes'))
        resolve = next(s for s in steps if s.startswith('name: Resolve exact required-CI production bytes'))
        self.assertIn('!= github.repository', fork_step)
        self.assertIn('== github.repository', resolve)
        self.assertNotIn('secrets.', fork_step)
        self.assertLess(release.index('name: Reject unsupported'), release.index('name: Produce isolated'))
        checkouts = [s for s in steps if s.startswith('uses:') and 'checkout@' in s]
        self.assertIn('ref: ${{ needs.select.outputs.source }}', checkouts[1])
        gate = next(s for s in steps if s.startswith('name: Validate package and install'))
        for command in ('pacman -U', 'okilum --help', 'ldd /usr/bin/okilum', 'python -m unittest', 'smoke-x11.sh'):
            self.assertIn(command, gate)
        for step in steps:
            if 'name: arch-publication' in step or step.startswith('name: Dispatch protected publication'):
                self.assertIn("github.event_name != 'pull_request'", step)
        ci = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        local = ci.split('  linux-local:\n', 1)[1].split('  linux-github:\n', 1)[0]
        self.assertIn('!= github.repository', local)
        self.assertIn('run: bash scripts/ci/check-linux.sh', local)
        self.assertNotIn('--release', (ROOT / 'scripts/ci/check-linux.sh').read_text())
        self.assertIn('scripts/arch/', selection.PATHS)
        self.assertIn('scripts/ci/package-linux-binary.py', selection.PATHS)


if __name__ == '__main__':
    unittest.main()
