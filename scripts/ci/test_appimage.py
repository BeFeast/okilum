"""Portable artifact identity and native loader checks must fail closed."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
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
                self.assertEqual(loader.call_count, 3)
                self.assertEqual(loader.call_args.kwargs['env']['LD_LIBRARY_PATH'],
                                 str(extracted / 'usr/lib'))
                self.assertEqual(dict(os.environ), parent_environment)
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
        self.assertIn('needs: [select, linux-checks, appimage]', workflow)
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
        self.assertEqual(caches[0][0]['if'], "github.event.schedule != '5 22 * * 2,6'")
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

    def test_pushed_beta_runs_distribution_and_signing_checks_after_the_handoff_job(self):
        import yaml
        workflow = yaml.safe_load((ROOT / '.github/workflows/forgejo-linux.yml').read_text())
        hourly = "github.event_name == 'push' && startsWith(github.ref, 'refs/heads/forgejo-linux-pr/hourly-')"
        names = ('Verify unchanged AppImage on clean Ubuntu and Fedora',
                 'Verify signed AppImage installation with disposable CI key')
        steps = {s.get('name'): s for s in workflow['jobs']['appimage']['steps']}
        for name in names:
            self.assertTrue(steps[name]['if'].endswith('&& !(' + hourly + ')'), name)
        # The handoff gate stays in the producer job and still precedes completion.
        self.assertNotIn('if', steps['Verify AppImage package'])
        acceptance = workflow['jobs']['appimage-acceptance']
        self.assertEqual(acceptance['needs'], 'appimage')
        self.assertEqual(acceptance['if'], hourly)
        accepted = {s.get('name'): s for s in acceptance['steps']}
        for name in names:
            self.assertNotIn('if', accepted[name])
        self.assertIn('scripts/appimage/smoke.sh', accepted[names[0]]['run'])
        self.assertIn('for image in ubuntu:24.04 fedora:44;', accepted[names[0]]['run'])
        self.assertEqual(accepted[names[1]]['env']['APPIMAGE_ARTIFACT_ID'],
                         '${{ needs.appimage.outputs.artifact-id }}')
        download = [s for s in acceptance['steps'] if str(s.get('uses', '')).startswith('actions/download-artifact@')]
        self.assertEqual(download[0]['with']['name'], 'linux-appimage-${{ github.sha }}')
        # Evidence keeps a distinct name: the handoff matches linux-appimage-<sha> exactly.
        uploads = [s['with']['name'] for s in acceptance['steps'] if 'upload-artifact' in str(s.get('uses', ''))]
        self.assertEqual(uploads, ['linux-appimage-acceptance-evidence-${{ github.sha }}'])
        self.assertEqual(workflow['jobs']['appimage']['outputs']['artifact-id'],
                         '${{ steps.producer.outputs.artifact-id }}')


if __name__ == '__main__':
    unittest.main()
