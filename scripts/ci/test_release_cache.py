"""Optional cache installation must not turn network failures into build failures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('release-cache.sh').resolve()


class CacheFallbackTests(unittest.TestCase):
    def test_download_and_corrupt_archive_fall_back_under_errexit(self):
        for behavior in ['exit 22', 'while [ "$1" != -o ]; do shift; done; shift; printf bad > "$1"']:
            with self.subTest(behavior=behavior), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                curl = root / 'curl'
                curl.write_text('#!/bin/bash\nprintf called > "$CACHE_TEST_CALLED"\n' + behavior + '\n')
                curl.chmod(0o755)
                env = {**os.environ, 'PATH': str(root) + ':' + os.environ['PATH'],
                       'AWS_ACCESS_KEY_ID': 'test', 'SCCACHE_ENDPOINT': 'test',
                       'TESSERA_SCCACHE_HOME': str(root / 'cache'),
                       'CACHE_TEST_CALLED': str(root / 'called')}
                env.pop('RUSTC_WRAPPER', None)
                result = subprocess.run(['bash', '-eu', '-c',
                    'source "$1"; test -z "${RUSTC_WRAPPER:-}"; echo compiler-continues',
                    'test', str(SCRIPT)], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('compiler-continues', result.stdout)
                self.assertEqual((root / 'called').read_text(), 'called')
                self.assertFalse(list((root / 'cache').rglob('sccache')))
