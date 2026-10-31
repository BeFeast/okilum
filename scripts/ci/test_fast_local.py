"""Real parallel flock controls, with no compiler or runner-service side effects."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

SCRIPT=Path(__file__).with_name('fast-local.sh').resolve()


class ParallelTargets(unittest.TestCase):
    def fixture(self, root):
        scripts=root/'scripts/ci';scripts.mkdir(parents=True)
        shutil.copyfile(SCRIPT,scripts/'fast-local.sh')
        (scripts/'release-cache.sh').write_text('release_cache_stats() { :; }\n')
        (scripts/'fast.sh').write_text('''#!/usr/bin/env bash
set -eu
# A daemon/compiler must not be able to retain the outer shell's slot lock.
test ! -e /proc/$$/fd/9
printf '%s|%s|%s\\n' "$OKILUM_FAST_SLOT" "$CARGO_TARGET_DIR" "$CARGO_BUILD_JOBS" >> "$POOL_LOG"
while [ ! -f "$POOL_GATE" ]; do sleep 0.02; done
exit "${COMPILER_STATUS:-0}"
''')
        return dict(os.environ,OKILUM_FAST_SLOT_ROOT=str(root/'pool'),OKILUM_FAST_SLOTS='4',
                    OKILUM_FAST_SLOT_WAIT_SECONDS='0',POOL_LOG=str(root/'log'),POOL_GATE=str(root/'gate'))

    def test_four_jobs_hold_distinct_targets_and_fifth_waits_outside_them(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);env=self.fixture(root);processes=[]
            try:
                for number in range(4):
                    processes.append(subprocess.Popen(['bash','scripts/ci/fast-local.sh'],cwd=root,
                        env=dict(env,GITHUB_RUN_NUMBER=str(number)),stdout=subprocess.PIPE,stderr=subprocess.PIPE))
                deadline=time.monotonic()+5
                while time.monotonic()<deadline:
                    rows=(root/'log').read_text().splitlines() if (root/'log').exists() else []
                    if len(rows)==4:break
                    time.sleep(.02)
                self.assertEqual(len(rows),4)
                records=[row.split('|') for row in rows]
                self.assertEqual(len({row[0] for row in records}),4)
                self.assertEqual(len({row[1] for row in records}),4)
                self.assertEqual({row[2] for row in records},{'3'})
                self.assertTrue(all(p.poll() is None for p in processes))
                fifth=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=2)
                self.assertEqual(fifth.returncode,75)
                self.assertIn('no compile/test verdict',fifth.stderr)
                self.assertEqual(len((root/'log').read_text().splitlines()),4)
                (root/'gate').touch()
                for process in processes:
                    stdout,stderr=process.communicate(timeout=3)
                    self.assertEqual(process.returncode,0,(stdout,stderr))
                # Released locks admit the next job; the rejected fifth was not a fake success.
                next_job=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=2)
                self.assertEqual(next_job.returncode,0,next_job.stderr)
            finally:
                (root/'gate').touch()
                for process in processes:
                    if process.poll() is None:process.terminate()
                    process.communicate(timeout=3)

    def test_real_compiler_failure_survives_cache_stats_and_releases_slot(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);env=self.fixture(root);(root/'gate').touch()
            env['COMPILER_STATUS']='23'
            result=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=3)
            self.assertEqual(result.returncode,23,result.stderr)
            env['COMPILER_STATUS']='0'
            result=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=3)
            self.assertEqual(result.returncode,0,result.stderr)


if __name__=='__main__':unittest.main()
