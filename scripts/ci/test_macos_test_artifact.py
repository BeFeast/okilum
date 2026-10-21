"""Exercise the standalone app's actual generated launcher and metadata off Mac."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/forgejo-macos.yml'


def packaging():
    step = WORKFLOW.read_text().split('      - name: Package compiled arm64 test app\n', 1)[1]
    return textwrap.dedent(step.split('        run: |\n', 1)[1].split('      - name:', 1)[0])


def embedded(marker):
    return re.search("python3 - <<'" + marker + "'\n(.*?)\n" + marker, packaging(), re.S)[1]


class MacTestArtifactTests(unittest.TestCase):
    def test_launcher_replaces_inherited_stable_state_and_preserves_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            app = work / 'Okilum Test.app'
            binary_dir = app / 'Contents/MacOS'
            binary_dir.mkdir(parents=True)
            env = dict(os.environ, APP=str(app), GITHUB_SHA='a' * 40, GITHUB_RUN_NUMBER='42')
            subprocess.run([sys.executable, '-c', embedded('PYAPP')], env=env, check=True)
            info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
            self.assertNotIn('SUFeedURL', info)
            self.assertNotIn('SUPublicEDKey', info)
            self.assertNotIn('CFBundleURLTypes', info)
            self.assertFalse(info['SUEnableAutomaticChecks'])
            # Only filesystem creation is stubbed; execute the actual launcher and child.
            stub = work / 'bin'
            stub.mkdir()
            (stub / 'mkdir').write_text('#!/bin/sh\nexit 0\n')
            (stub / 'mkdir').chmod(0o755)
            (binary_dir / 'okilum').write_text('#!' + sys.executable + '\nimport json,os,sys\n'
                'keys=["HOME","OKILUM_STATE_DIR","XDG_CONFIG_HOME","XDG_CACHE_HOME","XDG_STATE_HOME","OKILUM_INDEX_DIR","OKILUM_VAULT"]\n'
                'print(json.dumps(dict(env={key:os.environ[key] for key in keys if key in os.environ},args=sys.argv[1:])))\n')
            (binary_dir / 'okilum').chmod(0o755)
            env.update(PATH=str(stub) + os.pathsep + env['PATH'], OKILUM_STATE_DIR='/installed/state',
                       XDG_CONFIG_HOME='/installed/config', XDG_CACHE_HOME='/installed/cache',
                       XDG_STATE_HOME='/installed/xdg-state', OKILUM_INDEX_DIR='/installed/index',
                       OKILUM_VAULT='/installed/vault')
            result = subprocess.run([str(binary_dir / info['CFBundleExecutable']), 'a vault with spaces'],
                                    env=env, check=True, capture_output=True, text=True)
            observed = json.loads(result.stdout)
            state = str(Path(os.environ['HOME']) / 'Library/Application Support/com.befeast.okilum.arm64-test' / ('a' * 40))
            self.assertEqual(observed['env']['OKILUM_STATE_DIR'], state)
            self.assertEqual(observed['env']['XDG_CONFIG_HOME'], state + '/config')
            self.assertEqual(observed['env']['XDG_CACHE_HOME'], state + '/cache')
            self.assertNotIn('OKILUM_INDEX_DIR', observed['env'])
            self.assertNotIn('OKILUM_VAULT', observed['env'])
            self.assertEqual(observed['env']['HOME'], os.environ['HOME'])
            self.assertEqual(observed['args'], ['a vault with spaces'])

    def test_manifest_records_actual_bytes_without_claiming_release_signing(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            app = work / 'Test.app'
            (app / 'Contents/MacOS').mkdir(parents=True)
            for name in ['okilum', 'okilum-sync-supervisor']:
                (app / 'Contents/MacOS' / name).write_bytes(b'fixture ' + name.encode())
            archive = work / 'fixture.zip'
            archive.write_bytes(b'fixture archive')
            source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
            env = dict(os.environ, APP=str(app), QA_ARCHIVE=str(archive), GITHUB_SHA=source,
                       GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='1', GITHUB_REF='refs/heads/fixture')
            subprocess.run([sys.executable, '-c', embedded('PYRECEIPT')], cwd=ROOT, env=env, check=True)
            manifest = json.loads((work / 'manifest.json').read_text())
            self.assertEqual(manifest['sha256'], hashlib.sha256(archive.read_bytes()).hexdigest())
            self.assertEqual(manifest['size'], archive.stat().st_size)
            self.assertEqual(manifest['source'], source)
            self.assertEqual(manifest['signing'], 'ad-hoc')
            self.assertFalse(manifest['notarized'])
            self.assertIn('development', manifest['provenance'])
            self.assertIn('NOT VERIFIED', manifest['managed_sync'])

    def test_no_compile_or_publish_fallback_and_no_cache_only_artifact(self):
        script = packaging()
        commands = '\n'.join(line for line in script.splitlines() if not line.strip().startswith('#'))
        self.assertIsNone(re.search(r'\b(cargo|notarytool|security|curl|wget)\b', commands))
        workflow = WORKFLOW.read_text()
        for name in ['Package compiled arm64 test app', 'Retain standalone arm64 test app']:
            step = workflow.split('      - name: ' + name + '\n', 1)[1].split('      - name:', 1)[0]
            self.assertIn("if: success() && steps.native.outcome == 'success'", step)
        subprocess.run(['bash', '-n'], input=script, text=True, check=True)


if __name__ == '__main__':
    unittest.main()
