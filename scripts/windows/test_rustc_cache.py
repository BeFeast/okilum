"""The cache wrapper must preserve the packaged GPUI path in compiled output."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

WRAPPER = Path(__file__).resolve().parents[1] / 'windows-rustc.py'


class RustcCacheTests(unittest.TestCase):
    def test_direct_and_cached_compiler_see_the_same_manifest_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            compiler = root / 'compiler'
            compiler.write_text('#!/usr/bin/env python3\nimport os,json,sys\n'
                                'print(json.dumps([os.environ["CARGO_MANIFEST_DIR"], sys.argv[1:]]))\n')
            compiler.chmod(0o755)
            cache = root / 'cache'
            cache.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                             'os.execv(sys.argv[1],sys.argv[1:])\n')
            cache.chmod(0o755)
            for cached in ['', str(cache)]:
                for target, expected in [('x86_64-pc-windows-msvc', 'gpui-shaders'),
                                         ('x86_64-unknown-linux-gnu', 'original')]:
                    args = ['--crate-name', 'gpui_windows', '--target', target]
                    result = subprocess.run(['python3', str(WRAPPER), str(compiler), *args],
                        env={**os.environ, 'CARGO_MANIFEST_DIR': 'original', 'OKILUM_SCCACHE': cached},
                        check=True, capture_output=True, text=True)
                    self.assertEqual(json.loads(result.stdout), [expected, args])
