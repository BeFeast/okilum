import importlib.util
import os
import json
import plistlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('macos_shipping_policy', Path(__file__).with_name('macos-hosted.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
SOURCE = 'a' * 40
WORKFLOW = 'b' * 40


class ShippingPolicy(unittest.TestCase):
    def env(self):
        return {'GITHUB_SHA': WORKFLOW, 'PRODUCT_SOURCE': SOURCE, 'GITHUB_REF': 'refs/heads/main',
                'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_RUN_ATTEMPT': '1',
                'GITHUB_RUN_ID': '700', 'SHIPPING_BUILD': '5600', 'PRODUCER': '500',
                'PRODUCER_NUMBER': '600', 'PRODUCER_ATTEMPT': '1',
                'OKILUM_SIGNING_IDENTITY': 'Developer ID Application: Test (ABCDEFGHIJ)',
                'OKILUM_NOTARY_PROFILE': 'already-approved-profile', 'MACOS_LANE': 'hosted'}

    @patch.object(m.subprocess, 'run')
    def test_route_pins_older_product_source_not_workflow_sha(self, run):
        with tempfile.TemporaryDirectory() as tmp:
            env = self.env() | {'GITHUB_OUTPUT': str(Path(tmp) / 'out')}
            m.route(env)
            self.assertEqual(Path(env['GITHUB_OUTPUT']).read_text(), f'lane=hosted\nsource={SOURCE}\n')
            self.assertEqual(run.call_args.args[0], ['git', 'merge-base', '--is-ancestor', SOURCE, WORKFLOW])

    @patch.object(m.subprocess, 'run')
    def test_invalid_source_and_nonancestor_never_route(self, run):
        for source in ['main', '../main', 'A' * 40, '', 'a' * 39]:
            if source == '':
                continue  # Empty explicit source intentionally selects workflow SHA.
            with self.assertRaises(ValueError):
                m.source_identity(source, WORKFLOW)
        run.assert_not_called()
        run.side_effect = subprocess.CalledProcessError(1, 'git')
        with self.assertRaises(subprocess.CalledProcessError):
            m.source_identity(SOURCE, WORKFLOW)

    @patch.object(m.subprocess, 'run')
    def test_no_workday_local_fallback_or_old_source_local_route(self, run):
        for changes in [{'GITHUB_EVENT_NAME': 'schedule'}, {'MACOS_LANE': 'local'},
                        {'MACOS_LANE': 'typo'}]:
            with self.assertRaises(ValueError):
                m.route(self.env() | changes)

    @patch.object(m.subprocess, 'run')
    @patch.object(m.subprocess, 'check_output')
    def test_approved_existing_keychain_positive_and_missing_signer_negative(self, output, run):
        listing = '  1) ABCD "Developer ID Application: Test (ABCDEFGHIJ)"\n'
        output.side_effect = [WORKFLOW + '\n', 'Darwin arm64\n', listing]
        self.assertEqual(m.preflight(self.env()), (SOURCE, WORKFLOW))
        self.assertEqual(run.call_args.args[0], ['xcrun', 'notarytool', 'history', '--keychain-profile',
                                               'already-approved-profile', '--output-format', 'json'])
        self.assertEqual(run.call_args.kwargs['stdout'], subprocess.DEVNULL)
        output.side_effect = [WORKFLOW + '\n', 'Darwin arm64\n', '  0 valid identities found\n']
        run.reset_mock()
        with self.assertRaisesRegex(ValueError, 'no approved Developer ID'):
            m.preflight(self.env())
        self.assertEqual(len(run.call_args_list), 1)  # ancestry only; no Cargo/notary mutation

    @patch.object(m.subprocess, 'run')
    @patch.object(m.subprocess, 'check_output')
    def test_notary_failure_remains_failure_no_cargo(self, output, run):
        output.side_effect = [WORKFLOW, 'Darwin arm64', '1) X "Developer ID Application: Test (ABCDEFGHIJ)"']
        run.side_effect = [None, subprocess.CalledProcessError(1, 'notarytool')]
        with self.assertRaises(subprocess.CalledProcessError):
            m.preflight(self.env())
        self.assertFalse(any('cargo' in str(c) for c in run.call_args_list))

    @patch.object(m.subprocess, 'check_output', return_value=WORKFLOW)
    @patch.object(m.subprocess, 'run')
    def test_ref_attempt_build_platform_mismatch_rejected(self, run, output):
        for delta in [{'GITHUB_REF': 'refs/pull/1/head'}, {'GITHUB_RUN_ATTEMPT': '2'},
                      {'SHIPPING_BUILD': '9999'}, {'PRODUCER_ATTEMPT': '2'}, {'PRODUCER': '0'}]:
            with self.assertRaises(ValueError):
                m.preflight(self.env() | delta)
        output.side_effect = [WORKFLOW, 'Darwin x86_64']
        with self.assertRaisesRegex(ValueError, 'Darwin arm64'):
            m.preflight(self.env())

    def test_build_records_executed_source_not_workflow_and_rejects_bad_outputs(self):
        repo = Path(__file__).resolve().parents[2]
        names = ['scripts/build-macos-ci.sh', 'scripts/ci/supervisor-bundle.sh',
                 'scripts/updater/sign-bundle.sh', 'scripts/updater/make-dmg.sh']
        scripts = {name: (repo / name).read_bytes() for name in names}
        for wrong in [None, 'source', 'notary', 'plist']:
            with self.subTest(wrong=wrong), tempfile.TemporaryDirectory() as tmp:
                old = os.getcwd()
                os.chdir(tmp)
                for name, data in scripts.items():
                    dest = Path(name); dest.parent.mkdir(parents=True, exist_ok=True); dest.write_bytes(data)
                try:
                    def execute(argv, **kwargs):
                        if argv[0] == 'git':
                            self.assertEqual(argv, ['git', 'checkout', '--detach', SOURCE])
                            return
                        self.assertEqual(argv[0], 'bash')
                        self.assertTrue(argv[1].endswith('/scripts/build-macos-ci.sh'))
                        self.assertEqual(Path(argv[1]).read_bytes(), scripts['scripts/build-macos-ci.sh'])
                        self.assertEqual(kwargs['env']['OKILUM_DELIVERY_TOOLS'], str(Path(argv[1]).parents[1]))
                        self.assertEqual(kwargs['env']['OKILUM_BUILD_VERSION'], '5600')
                        out = Path(argv[2]);app = out / 'Okilum.app/Contents'
                        app.mkdir(parents=True)
                        (app / 'Info.plist').write_bytes(plistlib.dumps({
                            'OkilumSourceCommit': WORKFLOW if wrong == 'plist' else SOURCE,
                            'OkilumSourceTree': 'c' * 40, 'CFBundleVersion': '5600'}))
                        (out / 'app.zip').write_bytes(b'signed zip fixture')
                        (out / 'app.dmg').write_bytes(b'signed dmg fixture')
                        (out / 'release.env').write_text(
                            'SOURCE_SHA=' + (WORKFLOW if wrong == 'source' else SOURCE)
                            + '\nSOURCE_TREE=' + 'c' * 40 + '\nBUILD=5600\n'
                            + 'ARCHIVE=' + str(out / 'app.zip') + '\nDMG=' + str(out / 'app.dmg') + '\n')
                        for n in ['notarization.json', 'notarization-dmg.json']:
                            (out / n).write_text(json.dumps({'status': 'Invalid' if wrong == 'notary' else 'Accepted'}))
                    with patch.object(m, 'preflight', return_value=(SOURCE, WORKFLOW)), \
                            patch.object(m.subprocess, 'run', side_effect=execute), \
                            patch.object(m.subprocess, 'check_output', side_effect=[SOURCE, 'c' * 40]):
                        if wrong:
                            with self.assertRaises(ValueError):
                                m.build(self.env())
                            self.assertFalse(Path('dist/shipping-download').exists())
                        else:
                            m.build(self.env())
                            receipt = json.loads(Path('dist/shipping-download/provenance.json').read_text())
                            self.assertEqual(receipt['source'], SOURCE)
                            self.assertEqual(receipt['workflow_sha'], WORKFLOW)
                            self.assertEqual(receipt['build'], 5600)
                            self.assertEqual({x.name for x in Path('dist/shipping-download').iterdir()},
                                             {'app.zip', 'app.dmg', 'provenance.json'})
                finally:
                    os.chdir(old)

    def test_workflow_retains_local_signing_boundary_and_hosted_has_no_secret_bindings(self):
        root = Path(__file__).resolve().parents[2]
        fj = (root / '.forgejo/workflows/macos-release.yml').read_text()
        gh = (root / '.github/workflows/forgejo-macos-shipping.yml').read_text()
        self.assertIn("needs.select.outputs.lane == 'local'", fj)
        self.assertIn("needs.select.outputs.lane == 'hosted' && github.event_name == 'workflow_dispatch'", fj)
        self.assertIn('python3 scripts/releases/macos-hosted.py route', fj)
        self.assertIn('persist-credentials: false', gh)
        self.assertNotIn('secrets.MIRROR_TOKEN', gh)
        self.assertNotIn('secrets.SPARKLE_ED_PRIVATE_KEY', gh)
        self.assertIn('secrets.OKILUM_MAC_DEVELOPER_ID_P12', gh)
        self.assertIn('if: always()', gh)
        self.assertNotIn('security import', gh)
        self.assertNotIn('publication.py', gh)
        self.assertNotIn('release-publish', gh)
        self.assertIn('path: dist/shipping-download/*', gh)
        self.assertIn("github.run_attempt == 1", gh)


if __name__ == '__main__':
    unittest.main()
