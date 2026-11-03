"""Ownership controls for the job-scoped fixture reaper (synthetic proc entries)."""
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


helper = runpy.run_path(str(Path(__file__).with_name('sync-fixture-processes.py')))
fixtures = helper['fixtures']


class FixtureOwnershipTests(unittest.TestCase):
    def test_only_pinned_daemons_with_home_beneath_this_job_are_selected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = root / 'proc'
            proc.mkdir()
            job = root / 'job'
            binary = root / 'okilum-sync-fixture/syncthing-linux-amd64-v2.1.6/syncthing'
            cases = [
                (binary, job / 'run-1/peer', True),
                (str(binary) + ' (deleted)', job / 'run-2/peer', True),
                (binary, root / 'job-other/peer', False),
                (binary, root / 'other-job/peer', False),
                (root / 'usr/bin/syncthing', job / 'user-daemon', False),
            ]
            expected = []
            for pid, (exe, home, owned) in enumerate(cases, start=100):
                entry = proc / str(pid)
                entry.mkdir()
                (entry / 'exe').symlink_to(exe)
                (entry / 'cmdline').write_bytes(b'syncthing\0serve\0--home\0' + bytes(home) + b'\0')
                if owned:
                    expected.append((pid, str(home)))
            original = Path.iterdir
            def entries(path):
                return original(proc if path == Path('/proc') else path)
            with patch.object(Path, 'iterdir', entries):
                self.assertEqual(sorted(fixtures(job)), expected)


if __name__ == '__main__':
    unittest.main()
