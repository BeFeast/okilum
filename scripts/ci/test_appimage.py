"""Portable artifact identity and native loader checks must fail closed."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('appimage_verify', ROOT / 'scripts/appimage/verify.py')
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class AppImageTests(unittest.TestCase):
    def test_loader_accepts_resolved_libraries_and_rejects_absence_or_loader_failure(self):
        verify.require_loaded('libc.so.6 => /lib/libc.so.6 (0x123)', 0)
        for output, code in [('libfoo.so => not found', 0),
                             ("libc.so.6: version `GLIBC_2.39' not found", 1),
                             ('not a dynamic executable', 1)]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                verify.require_loaded(output, code)

    def test_only_matching_payload_and_source_reach_native_loader(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact, extracted = root / 'artifact', root / 'extracted'
            artifact.mkdir()
            extracted.mkdir()
            image = artifact / 'Okilum-x86_64.AppImage'
            image.write_bytes(b'exact artifact')
            provenance = {'artifact': image.name, 'sha256': hashlib.sha256(image.read_bytes()).hexdigest(),
                          'source_sha': 'a' * 40}
            (artifact / 'provenance.json').write_text(json.dumps(provenance))
            source = extracted / 'SOURCE_SHA'
            source.write_text('a' * 40 + '\n')
            for name in ('okilum.desktop', 'okilum.svg', 'AppRun'):
                (extracted / name).touch()
            with patch.object(verify.subprocess, 'run', return_value=SimpleNamespace(
                    stdout='gpgv 2.4; libc.so.6 => /lib/libc.so.6', stderr='', returncode=0)) as loader:
                parent_environment = dict(os.environ)
                verify.verify(artifact, extracted)
                self.assertEqual(loader.call_count, 6)
                self.assertEqual(loader.call_args.kwargs['env']['LD_LIBRARY_PATH'],
                                 str(extracted / 'usr/lib'))
                self.assertEqual(dict(os.environ), parent_environment)
                for broken in ('heif-convert', 'prlimit', 'heic-control.py'):
                    def result_for(command, **kwargs):
                        failed = any(str(arg).endswith('/' + broken) for arg in command)
                        return SimpleNamespace(stdout='gpgv 2.4',
                                               stderr='libmissing.so => not found' if failed else '',
                                               returncode=1 if failed else 0)
                    loader.side_effect = result_for
                    with self.subTest(broken=broken), self.assertRaises(ValueError):
                        verify.verify(artifact, extracted)
                loader.side_effect = None
                loader.reset_mock()
                source.write_text('b' * 40)
                with self.assertRaisesRegex(ValueError, 'source identity'):
                    verify.verify(artifact, extracted)
                loader.assert_not_called()
                source.write_text('a' * 40)
                image.write_bytes(b'replaced artifact')
                with self.assertRaisesRegex(ValueError, 'checksum'):
                    verify.verify(artifact, extracted)
                loader.assert_not_called()

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux child resource limits')
    def test_decoder_limits_have_allocation_and_file_size_positive_controls(self):
        allocation = 'bytearray(128 * 1024 * 1024)'
        positive = subprocess.run([sys.executable, '-c', allocation], capture_output=True, timeout=10)
        self.assertEqual(positive.returncode, 0, positive.stderr)
        limited = subprocess.run(['/usr/bin/prlimit', '--as=67108864', '--',
                                  sys.executable, '-c', allocation], capture_output=True, timeout=10)
        self.assertNotEqual(limited.returncode, 0)
        self.assertIn(b'MemoryError', limited.stderr)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'output'
            writer = "import pathlib,sys; pathlib.Path(sys.argv[1]).write_bytes(b'x' * (2 * 1024 * 1024))"
            positive = subprocess.run([sys.executable, '-c', writer, str(path)],
                                      capture_output=True, timeout=10)
            self.assertEqual(positive.returncode, 0, positive.stderr)
            self.assertEqual(path.stat().st_size, 2 * 1024 * 1024)
            path.unlink()
            limited = subprocess.run(['/usr/bin/prlimit', '--fsize=1048576', '--',
                                      sys.executable, '-c', writer, str(path)],
                                     capture_output=True, timeout=10)
            self.assertNotEqual(limited.returncode, 0)
            self.assertLessEqual(path.stat().st_size, 1024 * 1024)

    def test_shipped_launch_clears_library_override_and_preserves_arguments_and_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / 'fake shipped artifact'
            image.write_text('#!/bin/sh\n'
                             'test "${LD_LIBRARY_PATH+set}" != set || exit 91\n'
                             'test "$APPIMAGE_EXTRACT_AND_RUN" = 1 || exit 92\n'
                             'printf "%s\\n" "$@"\n'
                             'exit 23\n')
            image.chmod(0o755)
            environment = dict(os.environ, LD_LIBRARY_PATH='/smoke-only/library/path')
            result = subprocess.run(['bash', str(ROOT / 'scripts/appimage/launch.sh'),
                                     str(image), '--vault', 'vault with spaces'],
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 23)
            self.assertEqual(result.stdout, '--vault\nvault with spaces\n')
            self.assertEqual(environment['LD_LIBRARY_PATH'], '/smoke-only/library/path')

    def test_smoke_help_and_window_use_the_documented_runtime_option(self):
        smoke = (ROOT / 'scripts/appimage/smoke.sh').read_text()
        launch = '/artifact/Okilum-x86_64.AppImage --appimage-extract-and-run'
        self.assertIn(launch + ' --help', smoke)
        self.assertIn(launch + ' "$@"', smoke)
        self.assertIn('unset APPIMAGE_EXTRACT_AND_RUN LD_LIBRARY_PATH', smoke)
        self.assertNotIn('/source/scripts/appimage/launch.sh', smoke)
        self.assertNotIn('/work/squashfs-root/AppRun', smoke)

    def test_required_workflow_retains_artifact_before_testing_and_aggregates_both_gates(self):
        workflow = (ROOT / '.github/workflows/forgejo-linux.yml').read_text()
        self.assertIn('needs: [linux-checks, appimage]', workflow)
        # The actual aggregate command and fail-closed evidence are exercised
        # across main/branch and manual/automatic runs in test_gate_log_evidence.
        self.assertIn('CHECKS: ${{ needs.linux-checks.result }}', workflow)
        self.assertIn('APPIMAGE: ${{ needs.appimage.result }}', workflow)
        self.assertLess(workflow.index('name: linux-appimage-${{ github.sha }}'),
                        workflow.index('name: Verify unchanged AppImage'))
        self.assertIn('runs-on: ubuntu-22.04', workflow)
        self.assertIn('for image in ubuntu:24.04 fedora:44;', workflow)
        self.assertEqual(workflow.count('bash scripts/appimage/build.sh'), 1)

    def test_beta_producer_restores_main_seeded_dependency_cache_without_saving(self):
        import yaml
        producer = yaml.safe_load((ROOT / '.github/workflows/forgejo-linux.yml').read_text())
        seeder = yaml.safe_load((ROOT / '.github/workflows/beta-appimage-cache.yml').read_text())
        appimage, seed = producer['jobs']['appimage'], seeder['jobs']['seed']
        # rust-cache hashes the job env into its key; drift means a silent cold build.
        self.assertEqual(appimage['env'], seed['env'])
        self.assertEqual(appimage['runs-on'], seed['runs-on'])
        caches = [[s for s in job['steps'] if str(s.get('uses', '')).startswith('Swatinem/rust-cache@')]
                  for job in (appimage, seed)]
        self.assertEqual([len(c) for c in caches], [1, 1])
        for (step,) in [caches[0], caches[1]]:
            self.assertEqual(step['with']['shared-key'], 'beta-appimage-v1')
            # Only main saves (#2881); write-once beta refs restore main's entry.
            self.assertEqual(step['with']['save-if'], "${{ github.ref == 'refs/heads/main' }}")
            # Registry dependencies only: path crates must compile from the checkout.
            self.assertNotIn('cache-workspace-crates', step['with'])
        self.assertEqual(caches[0][0]['if'],
                         "steps.select.outputs.build == 'true' && github.event.schedule != '5 22 * * 2,6'")
        triggers = seeder.get('on', seeder.get(True))  # PyYAML reads a bare on: as True
        self.assertEqual(triggers['push']['branches'], ['main'])
        build = [s for s in seed['steps'] if s.get('run') == 'bash scripts/appimage/build.sh']
        self.assertEqual(len(build), 1)
        self.assertEqual(build[0]['if'], "steps.cargo-cache.outputs.cache-hit != 'true'")
        self.assertFalse(any('upload-artifact' in str(s.get('uses', '')) for s in seed['steps']))
        self.assertNotIn('${{ secrets.', (ROOT / '.github/workflows/beta-appimage-cache.yml').read_text())
        # Exactly one beta entry stays in the 10 GiB quota: prune runs after the save
        # (job boundary), only when the seed built, only on main, with no secrets.
        prune = seeder['jobs']['prune']
        self.assertEqual(prune['needs'], 'seed')
        self.assertEqual(prune['if'], "needs.seed.outputs.built == 'true' && github.ref == 'refs/heads/main'")
        self.assertEqual(seed['outputs']['built'], "${{ steps.cargo-cache.outputs.cache-hit != 'true' }}")
        self.assertEqual(prune['permissions'], {'actions': 'write'})
        script = prune['steps'][0]['run']
        self.assertEqual(prune['steps'][0]['env']['PREFIX'], 'v0-rust-beta-appimage-v1-Linux-')
        self.assertIn('ref=refs/heads/main&sort=created_at&direction=desc&per_page=1', script)
        self.assertIn('select(.id != $keep)', script)
        self.assertIn('exit 0', script.split('select(')[0])  # no main entry: delete nothing
        # The old main-only actions/cache step never ran on hourly refs.
        self.assertFalse(any(str(s.get('uses', '')).startswith('actions/cache') for s in appimage['steps']))

    def test_beta_cache_key_ignores_the_runner_images_preinstalled_toolchains(self):
        # #3043: rust-cache hashes every installed toolchain, and ubuntu-22.04
        # images 20261103/20261111 preinstall different stable Rust, so producers
        # on the other image missed the single pruned entry and compiled cold.
        import yaml
        producer = yaml.safe_load((ROOT / '.github/workflows/forgejo-linux.yml').read_text())
        seeder = yaml.safe_load((ROOT / '.github/workflows/beta-appimage-cache.yml').read_text())
        for job in (producer['jobs']['appimage'], seeder['jobs']['seed']):
            steps = job['steps']
            index = next(i for i, s in enumerate(steps) if str(s.get('uses', '')).startswith('Swatinem/rust-cache@'))
            before = steps[index - 1]
            self.assertEqual(before.get('run'), 'bash scripts/ci/rust-cache-toolchains.sh')
            self.assertEqual(before.get('if'), steps[index].get('if'))

    def _run_toolchain_pin(self, installed, uninstall_works=True):
        pinned = '1.100.0-x86_64-unknown-linux-gnu'
        with tempfile.TemporaryDirectory() as temporary:
            temporary = Path(temporary)
            state = temporary / 'toolchains.json'
            state.write_text(json.dumps(installed))
            log = temporary / 'calls.log'
            bin_dir = temporary / 'bin'
            bin_dir.mkdir()
            rustup = bin_dir / 'rustup'
            rustup.write_text(f"""#!{sys.executable}
import json, sys
state, log, pinned = {str(state)!r}, {str(log)!r}, {pinned!r}
args = sys.argv[1:]
with open(log, 'a') as out:
    out.write(' '.join(args) + '\\n')
names = json.load(open(state))
if args == ['toolchain', 'install']:
    names = names if pinned in names else names + [pinned]
elif args == ['show', 'active-toolchain']:
    print(pinned + " (overridden by '/repo/rust-toolchain.toml')")
elif args == ['toolchain', 'list', '--quiet']:
    print('\\n'.join(names))
elif args[:2] == ['toolchain', 'uninstall'] and {uninstall_works!r}:
    names = [n for n in names if n != args[2]]
json.dump(names, open(state, 'w'))
""")
            rustup.chmod(0o755)
            rustc = bin_dir / 'rustc'
            rustc.write_text('#!/bin/sh\necho rustc 1.100.0\n')
            rustc.chmod(0o755)
            env = dict(os.environ, PATH=f'{bin_dir}:{os.environ["PATH"]}')
            result = subprocess.run(['bash', str(ROOT / 'scripts/ci/rust-cache-toolchains.sh')],
                                    env=env, capture_output=True, text=True)
            return result, json.loads(state.read_text()), log.read_text().splitlines()

    def test_toolchain_pin_leaves_only_the_pinned_toolchain_on_either_image(self):
        pinned = '1.100.0-x86_64-unknown-linux-gnu'
        # Image 20261103: stable 1.99.1 preinstalled. Image 20261111: stable is
        # 1.100.0 under another name. Fresh runner: pinned not installed yet.
        for installed in (['stable-x86_64-unknown-linux-gnu', pinned],
                          ['stable-x86_64-unknown-linux-gnu'],
                          [pinned]):
            with self.subTest(installed=installed):
                result, left, calls = self._run_toolchain_pin(installed)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(left, [pinned])
                self.assertEqual(calls[0], 'toolchain install')
                self.assertNotIn(f'toolchain uninstall {pinned}', calls)

    def test_toolchain_pin_fails_when_another_toolchain_survives(self):
        # Control: an uninstall that leaves the toolchain must not pass silently.
        result, left, calls = self._run_toolchain_pin(
            ['stable-x86_64-unknown-linux-gnu', '1.100.0-x86_64-unknown-linux-gnu'], uninstall_works=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('toolchain uninstall stable-x86_64-unknown-linux-gnu', calls)
        self.assertIn('Expected only', result.stderr)

    def test_beta_is_one_hosted_job_with_inline_admission_and_blocking_checks(self):
        import yaml
        workflow = yaml.safe_load((ROOT / '.github/workflows/forgejo-linux.yml').read_text())
        jobs = workflow['jobs']
        # Every extra job on the critical path is another wait for a free hosted
        # runner (14.11: a separate select job queued 11 min), so admission and
        # both acceptance checks run inside the single producer job.
        self.assertEqual(set(jobs), {'linux-checks', 'appimage', 'linux'})
        appimage = jobs['appimage']
        self.assertNotIn('needs', appimage)
        self.assertNotIn('if', appimage)
        steps = appimage['steps']
        self.assertEqual(steps[1]['id'], 'select')
        self.assertIn('python3 scripts/releases/hourly_beta.py linux', steps[1]['run'])
        self.assertEqual(appimage['outputs']['build'], '${{ steps.select.outputs.build }}')
        gate = "steps.select.outputs.build == 'true'"
        for step in steps[2:]:
            self.assertTrue(str(step.get('if', '')).startswith(gate) or
                            step.get('if') == 'always() && ' + gate, step.get('name') or step.get('uses'))
        names = ('Verify unchanged AppImage on clean Ubuntu and Fedora',
                 'Verify signed AppImage installation with disposable CI key')
        by_name = {step.get('name'): step for step in steps}
        for name in names:
            # Runs for pushed beta refs (ref != main) as well as PR proxies,
            # dispatches and nightly; publication waits for it (beta gate).
            self.assertEqual(by_name[name]['if'], gate + " && (github.ref != 'refs/heads/main' || "
                             "github.event_name == 'workflow_dispatch' || github.event.schedule == '5 22 * * 2,6')")
        order = [step.get('name') for step in steps]
        self.assertLess(order.index('Verify AppImage package'), order.index(names[0]))
        aggregate = jobs['linux']
        self.assertEqual(aggregate['needs'], ['linux-checks', 'appimage'])
        self.assertEqual(aggregate['if'], "always() && (needs.appimage.outputs.build == 'true' "
                                          "|| needs.appimage.result != 'success')")

if __name__ == '__main__':
    unittest.main()
