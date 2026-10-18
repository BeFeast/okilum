"""Production payload controls use fake ELF/build-info, never native app builds."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('linux_package', Path(__file__).with_name('package-linux-binary.py'))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)
SHA = 'a' * 40
CONFIG = 'c' * 64


def binary_bytes():
    data = bytearray(64)
    data[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<H', data, 18, 62)
    return bytes(data) + b'compiler-positive-control'


def manifest(run_id=1):
    p = dict(repository='BeFeast/okilum', workflow=package.WORKFLOW, event='push', source_sha=SHA,
             run_id=run_id, attempt=1, forgejo_run_id=91, forgejo_run_attempt=1, forgejo_run_number=9108)
    m = dict(schema=1, source_sha=SHA, config=CONFIG, target=package.TARGET, profile='release',
             features=['brain'], harness=False, gpui_test_support=False, build=14108, producer=p,
             binary_sha256=package.digest(binary_bytes()), provenance=dict(rustc='fixture rustc', cargo='fixture cargo', linker='fixture mold',
             toolchain='pinned fixture', cargo_lock='d' * 64, vendor='e' * 40,
             patches={'patch.diff': 'f' * 64}, cargo_config={}, environment={'os_release': 'fixture',
             'rustflags': package.RUSTFLAGS, 'cargo_incremental': '0', 'cargo_build_jobs': '4', 'profile_overrides': 'repository defaults'}),
             timing=dict(hosted_created='2026-10-18T01:00:00Z', hosted_job_started='2026-10-18T01:00:10Z',
                         product_build_started='2026-10-18T01:01:00Z', product_build_completed='2026-10-18T01:02:00Z'))
    m['elf'] = dict(machine='x86_64', interpreter='/lib64/ld-linux-x86-64.so.2', needed=['libc.so.6'],
        symbol_versions=['GLIBC_2.39'], runtime_libraries='producer runtime fixture',
        build_info=dict(source=SHA, build='14108', version='0.1.14108', profile='release',
            target=package.TARGET, brain='yes', **{'gpui-test-support': 'no', 'settings-ui-harness': 'no', 'updater-ui-harness': 'no'}))
    return m


def receipt():
    m = manifest()
    return dict(schema=1, manifest=m, producer=m['producer'], artifact_id=7,
        artifact_api_digest='sha256:' + 'd' * 64, artifact_zip_sha256='d' * 64, archive_sha256='f' * 64,
        expires_at='2099-01-01T00:00:00Z', hosted_created='2026-10-18T01:00:00Z',
        hosted_job_started='2026-10-18T01:00:10Z', verified_at='2026-10-18T01:03:00Z')


def inspector(binary, m):
    package.require(binary.read_bytes() == binary_bytes(), 'Fake compiler control marker missing')
    return m.get('elf', manifest()['elf'])


def tar_data(entries):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w') as archive:
        for name, contents, kind in entries:
            entry = tarfile.TarInfo(name)
            entry.type = kind
            entry.size = len(contents) if kind == tarfile.REGTYPE else 0
            entry.linkname = '/outside' if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE) else ''
            archive.addfile(entry, io.BytesIO(contents) if entry.isfile() else None)
    return subprocess.check_output(['zstd', '-q', '-c'], input=raw.getvalue())


class LinuxBinaryTests(unittest.TestCase):
    def fixture(self, root):
        binary = root / 'fake-compiled'
        binary.write_bytes(binary_bytes())
        archive = package.package(binary, root / 'out', manifest(), inspector)
        return archive, archive.read_bytes(), archive.with_suffix('.zst.sha256').read_text()

    def test_roundtrip_and_corruption_positive_control(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive, data, checksum = self.fixture(root)
            m = package.unpack(data, checksum, root / 'extracted', SHA, CONFIG, manifest()['producer'], inspector)
            self.assertEqual((root / 'extracted/okilum').read_bytes(), binary_bytes())
            self.assertEqual(m['build'], 14108)
            self.assertEqual(package.verify_binary(root / 'extracted/okilum', m), m['elf'])
            with self.assertRaisesRegex(ValueError, 'Outer'):
                package.unpack(data + b'corruption', checksum, root / 'bad', SHA, CONFIG, inspector=inspector)
            with self.assertRaises(ValueError):
                package.unpack(data, checksum, root / 'bad', 'b' * 40, CONFIG, inspector=inspector)

    def test_missing_fields_wrong_profile_features_source_and_producer_fail_closed(self):
        good = manifest()
        package.validate_manifest(good, SHA, CONFIG, good['producer'])
        for field in good:
            bad = copy.deepcopy(good)
            del bad[field]
            with self.subTest(missing=field), self.assertRaises((ValueError, KeyError)):
                package.validate_manifest(bad, SHA, CONFIG, good['producer'])
        for key, value in [('source_sha', 'b' * 40), ('profile', 'debug'), ('features', []),
                           ('harness', True), ('gpui_test_support', True), ('build', 5042), ('target', 'aarch64')]:
            bad = copy.deepcopy(good)
            bad[key] = value
            with self.subTest(field=key), self.assertRaises(ValueError):
                package.validate_manifest(bad, SHA, CONFIG, good['producer'])
        for field in good['producer']:
            bad = copy.deepcopy(good)
            del bad['producer'][field]
            with self.subTest(producer=field), self.assertRaises(ValueError):
                package.validate_manifest(bad, SHA, CONFIG, good['producer'])
        for key in good['elf']['build_info']:
            bad = copy.deepcopy(good)
            bad['elf']['build_info'][key] = 'wrong'
            with self.subTest(build_info=key), self.assertRaises(ValueError):
                package.validate_manifest(bad, SHA, CONFIG)

    def test_archive_omission_traversal_links_duplicates_and_checksum_omission(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive, data, _ = self.fixture(root)
            raw = subprocess.check_output(['zstd', '-q', '-d', '-c'], input=data)
            with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
                entries = [(m.name, tar.extractfile(m).read() if m.isfile() else b'', m.type) for m in tar.getmembers()]
            corruptions = [entries[1:], entries + [('../escape', b'x', tarfile.REGTYPE)], entries + [entries[-1]]]
            for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE):
                bad = list(entries)
                name, contents, _ = bad[-1]
                bad[-1] = (name, contents, kind)
                corruptions.append(bad)
            bad = [(n, b'' if n.endswith('/SHA256SUMS') else b, k) for n, b, k in entries]
            corruptions.append(bad)
            for i, entries in enumerate(corruptions):
                corrupted = tar_data(entries)
                checksum = package.digest(corrupted) + '  ' + archive.name + '\n'
                with self.subTest(control=i), self.assertRaises(ValueError):
                    package.unpack(corrupted, checksum, root / f'bad{i}', SHA, CONFIG, inspector=inspector)
                self.assertFalse((root / f'bad{i}').exists())

    def test_package_preserves_original_binary_and_refuses_stripping_receipt_omission(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'package.pkg.tar.zst'
            entries = [('usr/bin/okilum', binary_bytes(), tarfile.REGTYPE),
                       ('usr/share/okilum/production-receipt.json', package.encode(receipt()), tarfile.REGTYPE)]
            path.write_bytes(tar_data(entries))
            r, binary = package.package_receipt(path, SHA, 14108, trusted_envelope=True)
            self.assertEqual(binary, binary_bytes())
            self.assertEqual(r['manifest']['producer']['forgejo_run_number'], 9108)
            for wrong in (entries[:1], entries + [entries[0]],
                          [(entries[0][0], b'stripped', tarfile.REGTYPE), entries[1]]):
                path.write_bytes(tar_data(wrong))
                with self.assertRaises(ValueError):
                    package.package_receipt(path, SHA, 14108, trusted_envelope=True)
            path.write_bytes(tar_data(entries))
            with self.assertRaises(ValueError):
                package.package_receipt(path, SHA, 14109, trusted_envelope=True)

    def test_executed_inspector_detects_elf_runtime_and_harness_controls(self):
        from unittest.mock import patch
        m = manifest()
        facts = '\n'.join(f'{k}: {v}' for k, v in m['elf']['build_info'].items())
        outputs = {
            '-lW': '[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]',
            '-dW': '(NEEDED) Shared library: [libc.so.6]', '-VW': 'GLIBC_2.39',
            '--build-info': facts, 'ldd': 'producer runtime fixture'}
        def command(args, **kwargs):
            return outputs['ldd'] if args[0] == 'ldd' else outputs[args[1]]
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / 'app'
            binary.write_bytes(binary_bytes())
            with patch.object(package.subprocess, 'check_output', side_effect=command), \
                    patch.object(package.subprocess, 'run') as execute:
                self.assertEqual(package.inspect(binary, m), m['elf'])
                execute.assert_called_once()
                for field, value in [('-lW', 'wrong interpreter'), ('ldd', 'libfoo => not found'),
                                     ('--build-info', facts.replace('gpui-test-support: no', 'gpui-test-support: yes'))]:
                    saved = outputs[field]
                    outputs[field] = value
                    with self.subTest(control=field), self.assertRaises(ValueError):
                        package.inspect(binary, m)
                    outputs[field] = saved

    def test_non_elf_or_test_support_build_info_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'wrong'
            path.write_bytes(b'#!/bin/sh\n')
            with self.assertRaises(ValueError):
                package.verify_binary(path, manifest())
        with self.assertRaises(ValueError):
            package.build_info('source: unknown\ngpui-test-support: yes\n', manifest())
