"""Historical selection and exact-package recovery; fake transport/signing/tools only."""
import argparse
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/ci'))
from test_linux_artifact_reuse import accepted_store, linux
from test_linux_binary import SHA, CONFIG, binary_bytes, receipt, tar_data, package as payload
from test_prebuilt import Store, publish


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Exact pre-schema control source, frozen from 6da9ee0acbb7b0aa55594509b8fabac2a21564c7.
LEGACY_HELPER = r'''#!/usr/bin/env python3
"""Unsigned permanent Linux PR lane using the proven exact-head macOS transport."""
import importlib.util
import os
from pathlib import Path
import uuid

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
WORKFLOW = '.github/workflows/forgejo-linux.yml'
BUILD_STEP = 'Run full Linux gate'


def main():
    result, branch, api = 'failure', None, None
    try:
        token = os.environ.get('MIRROR_TOKEN')
        if not token:
            raise transport.Unavailable('Mirror token unavailable')
        sha = os.environ['PR_HEAD_SHA']
        branch = transport.ref_name(os.environ['PR_NUMBER'], sha, os.environ['GITHUB_RUN_ID'],
                                    os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
        branch = branch.replace('forgejo-pr/', 'forgejo-linux-pr/', 1)
        api = transport.GitHub(token)
        transport.push_head(branch, sha, token)
        result, message = transport.wait_for_run(api, branch, sha, workflow=WORKFLOW,
            job_name='linux', build_step=BUILD_STEP, queue_timeout=900, run_timeout=5400)
        print(message)
    except transport.Unavailable as error:
        print(f'Hosted Linux unavailable; no fallback success: {error}')
    finally:
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'result={result}\n')
        if branch and api:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable:
                print('::warning::Temporary Linux ref cleanup failed')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
'''


selection = load('arch_selection', ROOT / 'scripts/arch/select-source.py')
recovery = load('arch_recovery', ROOT / 'scripts/arch/reuse-package.py')


def package_bytes(builddate=1, r=None):
    return tar_data([('usr/bin/okilum', binary_bytes(), tarfile.REGTYPE),
        ('usr/share/okilum/production-receipt.json', payload.encode(r or receipt()), tarfile.REGTYPE),
        ('.PKGINFO', f'pkgname = okilum\npkgver = 0.1.14108-1\narch = x86_64\nbuilddate = {builddate}\n'.encode(), tarfile.REGTYPE)])


def fixture(root):
    prebuilt = root / 'prebuilt'
    (prebuilt / 'payload').mkdir(parents=True)
    (prebuilt / 'payload/okilum').write_bytes(binary_bytes())
    (prebuilt / 'receipt.json').write_bytes(payload.encode(receipt()))
    data = package_bytes()
    name = 'okilum-0.1.14108-1-x86_64.pkg.tar.zst'
    prefix = 'okilum/arch/builds/14108'
    manifest = dict(build=14108, source=SHA, filename=name, sha256=payload.digest(data),
                    production=receipt(), binary_sha256=payload.digest(binary_bytes()))
    store = Store()
    store.objects = {f'{prefix}/manifest.json': payload.encode(manifest),
                     f'{prefix}/{name}': data, f'{prefix}/{name}.sig': b'fixture signature'}
    return prebuilt, store, name, prefix


class SourceSelection(unittest.TestCase):
    def test_actual_pre_schema_helper_control_and_rejection_before_execution(self):
        # Byte-for-byte fixture from 6da9ee0a; works even in shallow CI checkouts.
        old = LEGACY_HELPER
        namespace = {'__file__': str(ROOT / 'scripts/ci/github-linux.py'), '__name__': 'legacy_control'}
        exec(compile(old, 'actual-pre-schema-helper.py', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            def git(*args):
                return subprocess.check_output(['git', '-c', 'core.hooksPath=/dev/null',
                    '-c', 'user.name=Arch control', '-c', 'user.email=arch@example.invalid', *args],
                    cwd=root, text=True, stderr=subprocess.PIPE).strip()
            git('init', '-q')
            (root / 'scripts/ci').mkdir(parents=True)
            (root / 'scripts/ci/github-linux.py').write_text(old)
            git('add', '.')
            git('commit', '-qm', 'Legacy control fixture')
            legacy = git('rev-parse', 'HEAD')
            for relative in selection.PATHS:
                source = ROOT / relative
                paths = source.glob('*') if source.is_dir() else [source]
                for source_path in paths:
                    if source_path.is_file():
                        target = root / source_path.relative_to(ROOT)
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(source_path.read_bytes())
            current_helper = (root / 'scripts/ci/github-linux.py').read_bytes()
            (root / 'scripts/ci/github-linux.py').write_text(old)
            git('add', '.')
            git('commit', '-qm', 'Legacy helper with otherwise current packaging fixture')
            hybrid = git('rev-parse', 'HEAD')
            (root / 'scripts/ci/github-linux.py').write_bytes(current_helper)
            git('add', '.')
            git('commit', '-qm', 'Canonical consumer fixture')
            current = git('rev-parse', 'HEAD')
            self.assertEqual(selection.validate(current, root=root), current)
            env = dict(MIRROR_TOKEN='fixture', PR_HEAD_SHA=SHA, PR_NUMBER='1', GITHUB_RUN_ID='1',
                       GITHUB_OUTPUT=str(root / 'result'))
            with patch.dict(os.environ, env), patch.object(sys, 'argv', ['github-linux.py', 'resolve']), \
                    patch.object(namespace['transport'], 'GitHub'), \
                    patch.object(namespace['transport'], 'push_head') as push, \
                    patch.object(namespace['transport'], 'wait_for_run', return_value=('success', 'stub gate')):
                self.assertEqual(namespace['main'](), 0)
                push.assert_called_once()  # Positive dispatch-detection control.
                push.reset_mock()
                # Guard rejection prevents reaching even this known-dispatching helper.
                for selected in (legacy, hybrid):
                    with patch.object(subprocess, 'run', wraps=subprocess.run) as commands, \
                            self.assertRaisesRegex(ValueError, 'unsupported Arch consumer'):
                        selection.validate(selected, root=root)
                        namespace['main']()
                    self.assertFalse(any(c.args[0][0] in ('cargo', 'rustc', 'makepkg') for c in commands.call_args_list))
                push.assert_not_called()
            # Product/configuration changes do not substitute the selected source
            # and do not need to match the workflow's product configuration.
            (root / 'Cargo.lock').write_text('different product configuration fixture\n')
            git('add', '.')
            git('commit', '-qm', 'Product-only change fixture')
            self.assertEqual(selection.validate(current, root=root), current)

    def test_matching_tooling_allows_real_canonical_consumer_no_dispatch_or_compiler(self):
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        self.assertEqual(selection.validate(head), head)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            store = accepted_store(root)
            with patch.dict(os.environ, dict(PR_HEAD_SHA=SHA, MIRROR_TOKEN='fixture',
                    OKILUM_ARTIFACT_DIR=str(root / 'resolved'), GITHUB_OUTPUT=str(root / 'result'))), \
                    patch.object(linux.payload, 'config', return_value=CONFIG), \
                    patch.object(linux, 'archived', return_value=None), \
                    patch.object(linux.transport, 'GitHub', return_value=store), \
                    patch.object(linux.transport, 'push_head') as push, \
                    patch.object(linux.subprocess, 'run', wraps=subprocess.run) as commands:
                self.assertEqual(linux.main('resolve'), 0)
                push.assert_not_called()
                self.assertFalse(any(c.args[0][0] in ('cargo', 'rustc', 'makepkg') for c in commands.call_args_list))
                self.assertEqual((root / 'resolved/payload/okilum').read_bytes(), binary_bytes())
        for invalid in ('main', '6da9ee0a', '-' + 'a' * 39):
            with self.assertRaises(ValueError):
                selection.validate(invalid)

    def test_workflow_checks_trusted_tooling_before_selected_checkout(self):
        workflow = (ROOT / '.forgejo/workflows/linux-release.yml').read_text()
        guard = workflow.index('run: python3 scripts/arch/select-source.py')
        selected = workflow.index("ref: ${{ (inputs.source")
        resolve = workflow.index('python3 scripts/ci/github-linux.py resolve')
        self.assertLess(guard, selected)
        self.assertLess(selected, resolve)
        self.assertIn('fetch-depth: 0', workflow[:guard])
        self.assertIn('test "$PR_HEAD_SHA" = "$OKILUM_SELECTED_SOURCE"', workflow[selected:resolve])


class PackageRecovery(unittest.TestCase):
    def reuse(self, prebuilt, store, output, **kwargs):
        with patch.object(recovery.payload, 'config', return_value=CONFIG):
            return recovery.reuse(SHA, prebuilt, output, fetch=store.objects.get,
                                  verify=kwargs.get('verify', Mock()))

    def test_manual_current_source_catalog_and_archive_without_catalog_reuse_exact_package(self):
        for catalog in (True, False):
            with self.subTest(catalog=catalog), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                prebuilt, store, name, prefix = fixture(root)
                if catalog:
                    store.objects[f'okilum/releases/{SHA}/linux.json'] = b'catalog is not the recovery authority'
                # Fresh download can update verified_at; retain original embedded receipt.
                fresh = receipt()
                fresh['verified_at'] = '2026-10-18T02:00:00Z'
                (prebuilt / 'receipt.json').write_bytes(payload.encode(fresh))
                verify = Mock()
                self.assertTrue(self.reuse(prebuilt, store, root / 'out', verify=verify))
                verify.assert_called_once()
                self.assertEqual((root / 'out' / name).read_bytes(), store.objects[f'{prefix}/{name}'])
                self.assertEqual((root / 'out' / (name + '.sig')).read_bytes(), b'fixture signature')
                self.assertEqual(json.loads((root / 'out/production-receipt.json').read_text()), receipt())
                self.assertEqual(publish.native_payload.package_receipt(root / 'out' / name, SHA, 14108)[0], receipt())

    def test_metadata_drift_is_rejected_without_replacing_original_or_signing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prebuilt, store, name, prefix = fixture(root)
            output = root / 'out'
            output.mkdir()
            path = output / name
            path.write_bytes(package_bytes(2))
            self.assertNotEqual(path.read_bytes(), store.objects[f'{prefix}/{name}'])
            publish.native_payload.package_receipt(path, SHA, 14108)  # Same valid binary/receipt.
            before = copy.deepcopy(store.objects)
            with self.assertRaisesRegex(ValueError, 'differs from immutable'):
                self.reuse(prebuilt, store, output)
            self.assertEqual(path.read_bytes(), package_bytes(2))
            key = Mock()
            args = argparse.Namespace(command='publish', build=14108, source=SHA, package=str(path), archive_only=False)
            with self.assertRaisesRegex(ValueError, 'different contents'):
                publish.execute(args, store, key, root)
            key.sign.assert_not_called()
            self.assertEqual(store.objects, before)

    def test_exact_retry_recovers_feed_then_catalog_without_resigning_or_archive_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prebuilt, store, name, prefix = fixture(root)
            original = copy.deepcopy(store.objects)
            self.assertTrue(self.reuse(prebuilt, store, root / 'out'))
            key = Mock()
            key.public.return_value = b'fixture key'
            args = argparse.Namespace(command='publish', build=14108, source=SHA,
                package=str(root / 'out' / name), archive_only=False)
            def channel(r2, _key, _tmp, _channel, manifest, package, signature):
                self.assertEqual(package, original[f'{prefix}/{name}'])
                self.assertEqual(signature, b'fixture signature')
                r2.put('okilum/arch/beta/x86_64/latest.json', publish.encode(manifest))
            with patch.object(publish, 'validate_package'), patch.object(publish, 'publish_channel', side_effect=channel):
                publish.execute(args, store, key, root)
                publish.execute(args, store, key, root)
            key.sign.assert_not_called()
            self.assertEqual(key.verify.call_count, 2)
            for k, v in original.items():
                self.assertEqual(store.objects[k], v)
            self.assertIn(f'okilum/releases/{SHA}/linux.json', store.objects)
            self.assertEqual(json.loads(store.objects['okilum/arch/beta/x86_64/latest.json'])['build'], 14108)

    def test_corrupt_wrong_source_config_receipt_signature_and_incomplete_archives_fail_closed(self):
        for bad in ('checksum', 'source', 'config', 'receipt', 'binary', 'signature', 'missing_package', 'missing_signature', 'partial'):
            with self.subTest(bad=bad), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                prebuilt, store, name, prefix = fixture(root)
                manifest = json.loads(store.objects[f'{prefix}/manifest.json'])
                verify = Mock()
                if bad == 'checksum':
                    store.objects[f'{prefix}/{name}'] += b'corruption'
                elif bad == 'source':
                    manifest['source'] = 'b' * 40
                elif bad in ('config', 'receipt'):
                    r = receipt()
                    if bad == 'config':
                        r['manifest']['config'] = 'b' * 64
                    else:
                        r['artifact_id'] += 1
                    data = package_bytes(r=r)
                    store.objects[f'{prefix}/{name}'] = data
                    manifest.update(sha256=payload.digest(data), production=r)
                elif bad == 'binary':
                    (prebuilt / 'payload/okilum').write_bytes(b'changed')
                elif bad == 'signature':
                    verify.side_effect = ValueError('fixture bad signature')
                elif bad == 'missing_package':
                    del store.objects[f'{prefix}/{name}']
                elif bad == 'missing_signature':
                    del store.objects[f'{prefix}/{name}.sig']
                store.objects[f'{prefix}/manifest.json'] = payload.encode(manifest)
                if bad == 'partial':
                    del store.objects[f'{prefix}/manifest.json']
                with self.assertRaises(ValueError):
                    self.reuse(prebuilt, store, root / 'out', verify=verify)
                self.assertFalse((root / 'out').exists())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prebuilt, store, _, _ = fixture(root)
            store.objects = {}
            self.assertFalse(self.reuse(prebuilt, store, root / 'out'))

    def test_whole_build_script_skips_makepkg_on_reuse_and_detects_packager_compiler_stubs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prebuilt, store, name, prefix = fixture(root)
            # Build.sh runs real validation/reuse code. Only binary inspection,
            # signature verification and public reads are substituted by controls.
            r = receipt()
            r['manifest']['config'] = recovery.payload.config()
            data = package_bytes(r=r)
            store.objects[f'{prefix}/{name}'] = data
            manifest = json.loads(store.objects[f'{prefix}/manifest.json'])
            manifest.update(production=r, sha256=payload.digest(data))
            store.objects[f'{prefix}/manifest.json'] = payload.encode(manifest)
            (prebuilt / 'receipt.json').write_bytes(payload.encode(r))
            fake_store = root / 'store'
            for key, value in store.objects.items():
                path = fake_store / key
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(value)
            (root / 'scripts').symlink_to(ROOT / 'scripts', target_is_directory=True)
            tools = root / 'tools'
            tools.mkdir()
            for tool in ('cargo', 'rustc', 'makepkg'):
                path = tools / tool
                path.write_text('#!/bin/sh\necho ' + tool + ' >> "$TOOL_COUNTER"\nexit 97\n')
                path.chmod(0o755)
            (tools / 'git').write_text('#!/bin/sh\n[ "$*" = "rev-parse HEAD" ] || exit 98\necho ' + SHA + '\n')
            (tools / 'git').chmod(0o755)
            bootstrap = '''import importlib.util, os, pathlib, runpy, sys
original = importlib.util.spec_from_file_location
def spec(name, path, *a, **kw):
    result = original(name, path, *a, **kw)
    execute = result.loader.exec_module
    def load(module):
        execute(module)
        if str(path).endswith('package-linux-binary.py'):
            module.inspect = lambda binary, m: m['elf']
        if str(path).endswith('github-linux.py'):
            def fetch(key):
                p = pathlib.Path(os.environ['FAKE_STORE']) / key
                return p.read_bytes() if p.exists() else None
            def verify(package, signature):
                assert signature.read_bytes() == b'fixture signature'
                with open(os.environ['VERIFY_COUNTER'], 'a') as stream: stream.write('verified\\n')
            module.public_read, module.verify_signature = fetch, verify
    result.loader.exec_module = load
    return result
importlib.util.spec_from_file_location = spec
if sys.argv[1] == '-':
    exec(compile(sys.stdin.read(), '<build.sh inline>', 'exec'))
else:
    sys.argv = sys.argv[1:]
    runpy.run_path(sys.argv[0], run_name='__main__')
'''
            (tools / 'python3').write_text('#!' + sys.executable + '\n' + bootstrap)
            (tools / 'python3').chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                OKILUM_PREBUILT_DIR=str(prebuilt), FAKE_STORE=str(fake_store),
                VERIFY_COUNTER=str(root / 'verified'), TOOL_COUNTER=str(root / 'tools-called'))
            result = subprocess.run(['bash', str(ROOT / 'scripts/arch/build.sh')], cwd=root,
                                    env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((root / 'dist/arch' / name).read_bytes(), data)
            self.assertEqual((root / 'verified').read_text(), 'verified\n')
            self.assertFalse((root / 'tools-called').exists())
            for tool in ('makepkg', 'cargo', 'rustc'):
                self.assertEqual(subprocess.run([tool, 'build'], env=env, capture_output=True).returncode, 97)
            self.assertEqual((root / 'tools-called').read_text(), 'makepkg\ncargo\nrustc\n')


if __name__ == '__main__':
    unittest.main()
