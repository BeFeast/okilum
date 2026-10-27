"""Linux-only publication semantics using the real Arch planner and an isolated store."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import catalog
import linux

SOURCE = 'a' * 40
BUILD = 42


class Store:
    def __init__(self):
        self.data = {'okilum/appcast.xml': b'mac', 'okilum/windows/stable/releases.stable.json': b'signed windows',
                     'okilum/releases/stable.json': b'all-platform marker'}
        self.writes = []
        self.fail_at = None
        self.fail_after = False

    def call(self, method, key):
        if method == 'GET':
            return self.data.get(key)
        assert method == 'DELETE'
        self.mutate(key, None)

    def mutate(self, key, data):
        assert key.startswith('okilum/arch/'), key
        self.writes.append(key)
        fail = len(self.writes) == self.fail_at
        if fail and not self.fail_after:
            raise OSError('injected interruption')
        if data is None:
            self.data.pop(key, None)
        else:
            self.data[key] = data
        if fail:
            raise OSError('injected lost response after write')

    def put(self, key, data, *_):
        self.mutate(key, data)


class Signing:
    fingerprint = 'F' * 40
    home = '/fake'

    def __init__(self):
        self.verified = []

    def verify(self, path):
        self.verified.append(path.name)
        if Path(str(path) + '.sig').read_bytes() != b'signed:' + path.read_bytes():
            raise ValueError('Bad signature')


def repo_add(*args, **kwargs):
    assert args[0] == 'repo-add', args
    db = Path(args[-2])
    db.write_bytes(b'database:' + Path(args[-1]).read_bytes())
    Path(str(db) + '.sig').write_bytes(b'signed:' + db.read_bytes())
    return b''


def validate_package(path, build):
    if path.read_bytes() != f'package:{build}'.encode():
        raise ValueError('Unexpected package identity')


def fixture(old=True):
    store = Store()
    name = linux.arch.package_name(BUILD)
    archive = f'okilum/arch/builds/{BUILD}'
    package = f'package:{BUILD}'.encode()
    signature = b'signed:' + package
    manifest = {'build': BUILD, 'filename': name, 'source': SOURCE, 'sha256': linux.digest(package)}
    store.data[f'{archive}/manifest.json'] = catalog.encode(manifest)
    assets = []
    for filename, data in [(name, package), (name + '.sig', signature)]:
        key = f'{archive}/{filename}'
        store.data[key] = data
        assets.append(catalog.asset(key, filename, data))
    store.data[f'{catalog.PREFIX}/{SOURCE}/linux.json'] = catalog.encode(
        {'platform': 'linux', 'source': SOURCE, 'build': BUILD, 'assets': assets})
    if old:
        for name, data in [('okilum-stable.db', b'old database'), ('okilum-stable.db.sig', b'signed:old database'),
                           ('latest.json', catalog.encode({'build': 40, 'source': 'b' * 40}))]:
            store.data[f'{linux.PREFIX}/{name}'] = data
    return store


class LinuxPromotion(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.signing = Signing()
        for target, replacement in [('run', repo_add), ('validate_package', validate_package)]:
            patcher = patch.object(linux.arch, target, side_effect=replacement)
            patcher.start()
            self.addCleanup(patcher.stop)

    def execute(self, store, rollback=False, source=SOURCE, build=BUILD):
        return linux.execute(store, self.signing, Path(self.tmp.name), source, build, rollback)

    def test_real_planner_publishes_original_bytes_and_other_platforms_are_untouched(self):
        store = fixture()
        before = {key: value for key, value in store.data.items() if not key.startswith('okilum/arch/')}
        receipt = self.execute(store)
        self.assertEqual(receipt['state'], 'complete')
        self.assertTrue(any(name.endswith('.pkg.tar.zst') for name in self.signing.verified))
        self.assertIn('okilum-stable.db.tar.gz', self.signing.verified)
        self.assertEqual(before, {key: value for key, value in store.data.items() if not key.startswith('okilum/arch/')})
        name = linux.arch.package_name(BUILD)
        for suffix in ['', '.sig']:
            self.assertEqual(store.data[f'{linux.PREFIX}/{name}{suffix}'],
                             store.data[f'okilum/arch/builds/{BUILD}/{name}{suffix}'])
        self.assertTrue(any(k.endswith('/latest.json') for k in store.writes))  # Positive control.
        writes = store.writes.copy()
        self.execute(store)
        self.assertEqual(store.writes, writes)

    def test_invalid_inputs_catalog_archive_checksums_signatures_and_metadata_fail_before_writes(self):
        desc = f'{catalog.PREFIX}/{SOURCE}/linux.json'
        manifest = f'okilum/arch/builds/{BUILD}/manifest.json'
        package = f'okilum/arch/builds/{BUILD}/{linux.arch.package_name(BUILD)}'
        mutations = [(desc, None, None), (manifest, None, None), (desc, 'source', 'b' * 40),
                     (desc, 'build', 43), (desc, 'platform', 'windows'), (manifest, 'source', 'b' * 40),
                     (manifest, 'build', 43), (manifest, 'filename', '../escape'), (manifest, 'sha256', '0' * 64),
                     (package, None, b'corrupt'), (package + '.sig', None, b'bad signature'),
                     (package + '.sig', None, None)]
        for key, field, value in mutations:
            with self.subTest(key=key, field=field):
                store = fixture()
                if field:
                    obj = json.loads(store.data[key]); obj[field] = value; store.data[key] = catalog.encode(obj)
                elif value is None:
                    del store.data[key]
                else:
                    store.data[key] = value
                    if key.endswith('.sig'):  # Correct digest, invalid signature must still be detected.
                        obj = json.loads(store.data[desc]); obj['assets'][1] = catalog.asset(key, key.split('/')[-1], value)
                        store.data[desc] = catalog.encode(obj)
                with self.assertRaises(ValueError):
                    self.execute(store)
                self.assertEqual(store.writes, [])
        for source, build in [('short', BUILD), (SOURCE, 0), (SOURCE, -1)]:
            store = fixture()
            with self.assertRaises(ValueError):
                self.execute(store, source=source, build=build)
            self.assertEqual(store.writes, [])
        store = fixture()
        with patch.object(linux.arch, 'validate_package', side_effect=ValueError('bad PKGINFO')):
            with self.assertRaisesRegex(ValueError, 'PKGINFO'):
                self.execute(store)
        self.assertEqual(store.writes, [])

    def test_wrong_asset_key_size_or_asset_set_are_refused(self):
        for change in ['key', 'size', 'set']:
            store = fixture()
            key = f'{catalog.PREFIX}/{SOURCE}/linux.json'
            obj = json.loads(store.data[key])
            if change == 'set':
                obj['assets'] *= 2
            else:
                obj['assets'][0][change] = 'other' if change == 'key' else 1
            store.data[key] = catalog.encode(obj)
            with self.assertRaises(ValueError):
                self.execute(store)
            self.assertEqual(store.writes, [])

    def test_every_write_interruption_and_lost_response_can_retry_same_inputs(self):
        complete = fixture()
        self.execute(complete)
        for fail_after in [False, True]:
            for index in range(1, len(complete.writes) + 1):
                with self.subTest(index=index, fail_after=fail_after):
                    store = fixture(); store.fail_at = index; store.fail_after = fail_after
                    with self.assertRaises(OSError):
                        self.execute(store)
                    store.fail_at = None
                    self.assertEqual(self.execute(store)['state'], 'complete')
                    for name in linux.FEEDS:
                        self.assertEqual(store.data[f'{linux.PREFIX}/{name}'], complete.data[f'{linux.PREFIX}/{name}'])

    def test_rollback_restores_exact_previous_feeds_and_retains_payloads(self):
        for old in [True, False]:
            store = fixture(old)
            before = {name: store.data.get(f'{linux.PREFIX}/{name}') for name in linux.FEEDS}
            self.execute(store)
            result = self.execute(store, rollback=True)
            self.assertEqual(result['state'], 'rolled_back')
            self.assertEqual(before, {name: store.data.get(f'{linux.PREFIX}/{name}') for name in linux.FEEDS})
            self.assertIn(f'{linux.PREFIX}/{linux.arch.package_name(BUILD)}', store.data)
            self.execute(store, rollback=True)  # Idempotent rollback.
            with self.assertRaisesRegex(ValueError, 'rolled back'):
                self.execute(store)

    def test_interrupted_rollback_can_only_resume_rollback(self):
        for fail_after in [False, True]:
            for step in range(1, 7):
                store = fixture()
                self.execute(store)
                store.fail_at = len(store.writes) + step; store.fail_after = fail_after
                with self.assertRaises(OSError):
                    self.execute(store, rollback=True)
                store.fail_at = None
                self.assertEqual(self.execute(store, rollback=True)['state'], 'rolled_back')

    def test_rollback_after_interrupted_signature_write(self):
        store = fixture()
        # Receipt, active pointer, applying receipt, 2 payloads, DB signature then DB.
        store.fail_at = 7
        with self.assertRaises(OSError):
            self.execute(store)
        store.fail_at = None
        self.assertEqual(self.execute(store, rollback=True)['state'], 'rolled_back')
        self.assertEqual(store.data[f'{linux.PREFIX}/okilum-stable.db'], b'old database')

    def test_monotonic_guard_and_external_feed_changes(self):
        store = fixture()
        store.data[f'{linux.PREFIX}/latest.json'] = catalog.encode({'build': 99})
        with self.assertRaisesRegex(ValueError, 'rollback'):
            self.execute(store)
        self.assertEqual(store.writes, [])
        store = fixture(); self.execute(store)
        store.data[f'{linux.PREFIX}/latest.json'] = catalog.encode({'build': 99})
        writes = store.writes.copy()
        for rollback in [True, False]:
            with self.assertRaisesRegex(ValueError, 'outside this receipt'):
                self.execute(store, rollback=rollback)
        self.assertEqual(store.writes, writes)

    def test_bad_generated_db_signature_never_writes(self):
        store = fixture()
        def corrupt_repo(*args, **kwargs):
            repo_add(*args, **kwargs)
            Path(str(args[-2]) + '.sig').write_bytes(b'bad database signature')
        with patch.object(linux.arch, 'run', side_effect=corrupt_repo):
            with self.assertRaisesRegex(ValueError, 'Bad signature'):
                self.execute(store)
        self.assertEqual(store.writes, [])

    def test_silent_payload_or_feed_write_loss_is_not_completed(self):
        for target in [linux.arch.package_name(BUILD), 'okilum-stable.db']:
            store = fixture()
            original = store.put
            def drop(key, data, *args):
                if key != f'{linux.PREFIX}/{target}':
                    original(key, data, *args)
            with patch.object(store, 'put', side_effect=drop):
                with self.assertRaisesRegex(ValueError, 'readback mismatch'):
                    self.execute(store)
            self.assertEqual(self.execute(store)['state'], 'complete')

    def test_other_selection_cannot_supersede_interrupted_transaction(self):
        store = fixture(); store.fail_at = 7
        with self.assertRaises(OSError):
            self.execute(store)
        store.fail_at = None
        writes = store.writes.copy()
        with self.assertRaisesRegex(ValueError, 'interrupted Linux'):
            self.execute(store, source='b' * 40, build=43)
        self.assertEqual(store.writes, writes)

    def test_incomplete_or_bad_previous_feed_and_immutable_conflict_are_refused(self):
        for mode in ['missing', 'signature', 'immutable']:
            store = fixture()
            if mode == 'missing':
                del store.data[f'{linux.PREFIX}/okilum-stable.db.sig']
            elif mode == 'signature':
                store.data[f'{linux.PREFIX}/okilum-stable.db.sig'] = b'bad'
            else:
                store.data[f'{linux.PREFIX}/{linux.arch.package_name(BUILD)}'] = b'conflict'
            with self.assertRaises(ValueError):
                self.execute(store)
            self.assertEqual(store.writes, [])


class Workflow(unittest.TestCase):
    def test_cli_rejects_source_outside_main_before_signing_or_store_access(self):
        import sys
        with patch.object(sys, 'argv', ['linux.py', '--source', SOURCE, '--build', str(BUILD)]), \
                patch.object(linux.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'git')) as ancestry, \
                patch.object(linux.arch, 'SigningKey') as signing, patch.object(linux.arch, 'R2') as store:
            with self.assertRaises(subprocess.CalledProcessError):
                linux.main()
            ancestry.assert_called_once_with(['git', 'merge-base', '--is-ancestor', SOURCE, 'origin/main'], check=True)
            signing.assert_not_called()
            store.assert_not_called()

    def test_linux_dispatch_exits_before_any_legacy_build_or_cross_platform_path(self):
        workflow = Path(__file__).resolve().parents[2] / '.forgejo/workflows/releases.yml'
        text = workflow.read_text()
        self.assertIn("inputs.build != '' && 'okilum-macos-release'", text)
        self.assertIn('cancel-in-progress: false', text)
        script = text.split('        run: |\n', 1)[1]
        script = '\n'.join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stub = root / 'python'
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            stub.chmod(0o755)
            env = {**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH'], 'LINUX_ONLY': 'true',
                   'PROMOTE_BUILD': str(BUILD), 'PROMOTE_SOURCE': SOURCE, 'WINDOWS_ONLY': 'false',
                   'SKIP_WINDOWS': 'false', 'SUPERSEDE_PENDING': 'false', 'LINUX_ROLLBACK': 'false'}
            result = subprocess.run(['bash', '-e', '-c', script], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['scripts/releases/appimage_stable.py', '--source', SOURCE, '--build', str(BUILD)])
            for change in [{'PROMOTE_BUILD': ''}, {'PROMOTE_SOURCE': ''}, {'WINDOWS_ONLY': 'true'},
                           {'SKIP_WINDOWS': 'true'}, {'SUPERSEDE_PENDING': 'true'},
                           {'LINUX_ONLY': 'false', 'LINUX_ROLLBACK': 'true'}]:
                result = subprocess.run(['bash', '-e', '-c', script], env={**env, **change}, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, '')
            result = subprocess.run(['bash', '-e', '-c', script], env={**env, 'LINUX_ROLLBACK': 'true'}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
