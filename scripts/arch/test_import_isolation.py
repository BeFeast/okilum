"""Cross-directory production fixtures must not redirect Arch test discovery."""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ImportIsolation(unittest.TestCase):
    def test_fixture_imports_preserve_path_and_arch_module_origins(self):
        for fixture in ('test_fork_production', 'test_prebuilt'):
            with self.subTest(fixture=fixture):
                code = '''import importlib, json, sys
from pathlib import Path
root = Path.cwd()
sys.path.insert(0, str(root / 'scripts/arch'))
before = sys.path.copy()
importlib.import_module(sys.argv[1])
after = sys.path.copy()
import test_publish
publisher = getattr(test_publish, 'publish', None) or test_publish.p
print(json.dumps(dict(before=before, after=after,
    test=test_publish.__file__, publisher=publisher.__file__,
    catalog=test_publish.catalog.__file__)))
'''
                result = subprocess.run([sys.executable, '-c', code, fixture], cwd=ROOT,
                    env={'PATH': os.environ['PATH'], 'PYTHONDONTWRITEBYTECODE': '1'},
                    capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                origins = json.loads(result.stdout)
                self.assertEqual(origins['after'], origins['before'])
                for key, relative in (('test', 'arch/test_publish.py'),
                                      ('publisher', 'arch/publish.py'),
                                      ('catalog', 'releases/catalog.py')):
                    self.assertEqual(Path(origins[key]).resolve(), ROOT / 'scripts' / relative)


if __name__ == '__main__':
    unittest.main()
