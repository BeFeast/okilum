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
        for name in ['test-group.sh', 'test-group.py']:
            shutil.copyfile(SCRIPT.with_name(name), scripts/name)
        shutil.copyfile(SCRIPT.with_name('fast-source.sh'),scripts/'fast-source.sh')
        (scripts/'release-cache.sh').write_text('release_cache_stats() { :; }\n')
        (scripts/'fast.sh').write_text('''#!/usr/bin/env bash
set -eu
# A daemon/compiler must not be able to retain the outer shell's slot lock.
test ! -e /proc/$$/fd/9
printf '%s|%s|%s\\n' "$OKILUM_FAST_SLOT" "$CARGO_TARGET_DIR" "$CARGO_BUILD_JOBS" >> "$POOL_LOG"
while [ ! -f "$POOL_GATE" ]; do sleep 0.02; done
exit "${COMPILER_STATUS:-0}"
''')
        subprocess.run(['git','init','-q',str(root)],check=True)
        subprocess.run(['git','-C',str(root),'add','scripts'],check=True)
        subprocess.run(['git','-C',str(root),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid',
                        'commit','-qm','fixture'],check=True)
        return dict(os.environ,GITHUB_SHA=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip(),
                    OKILUM_FAST_SLOT_ROOT=str(root/'pool'),OKILUM_FAST_SLOTS='4',
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
            self.assertIn('TEST_GROUP_START group=fast.sh limit=480s', result.stdout)
            env['COMPILER_STATUS']='0'
            result=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=3)
            self.assertEqual(result.returncode,0,result.stderr)

    def test_shell_gate_uses_shared_target_and_preserves_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);env=self.fixture(root);(root/'gate').touch()
            env.update(OKILUM_FAST_SLOTS='1',GITHUB_RUN_NUMBER='0')
            shell=root/'scripts/ci/shell-full.sh'
            shell.write_text('''#!/usr/bin/env bash
set -eu
test ! -e /proc/$$/fd/9
printf '%s|%s|%s\\n' "$OKILUM_FAST_SLOT" "$CARGO_TARGET_DIR" "$CARGO_BUILD_JOBS" >> "$POOL_LOG"
echo SHELL_GATE_EXECUTED
exit 41
''')
            subprocess.run(['git','-C',str(root),'add','scripts'],check=True)
            subprocess.run(['git','-C',str(root),'-c','user.name=Fixture',
                            '-c','user.email=fixture@example.invalid','commit','-qm','shell probe'],check=True)
            env['GITHUB_SHA']=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip()
            full=subprocess.run(['bash','scripts/ci/fast-local.sh','shell-full'],cwd=root,env=env,capture_output=True,text=True,timeout=3)
            self.assertEqual(full.returncode,41,full.stderr)
            self.assertIn('TEST_GROUP_START group=shell-full.sh limit=1020s', full.stdout)
            self.assertIn('SHELL_GATE_EXECUTED',full.stdout)
            fast=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,env=env,capture_output=True,text=True,timeout=3)
            self.assertEqual(fast.returncode,0,fast.stderr)
            self.assertNotIn('SHELL_GATE_EXECUTED',fast.stdout)
            rows=(root/'log').read_text().splitlines()
            self.assertEqual(len(rows),2)
            self.assertEqual(rows[0],rows[1])

    def test_stable_source_avoids_legacy_metadata_and_reuses_its_own_target(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);env=self.fixture(root);(root/'gate').touch()
            env.update(OKILUM_FAST_SLOTS='1',GITHUB_RUN_NUMBER='0')
            legacy=root/'pool/0/target'
            legacy.mkdir(parents=True)
            metadata=legacy/'icons-dir'
            stale='/removed/hostexecutor/vendor/gpui-kit/assets/icons'
            metadata.write_text(stale)
            # Model Cargo reusing build-script metadata rather than rerunning
            # assets build.rs. The component forwards it into macro expansion.
            scripts=root/'scripts/ci'
            (scripts/'fast.sh').write_text('''#!/usr/bin/env bash
set -eu
test ! -e /proc/$$/fd/9
metadata="$CARGO_TARGET_DIR/icons-dir"
if [ ! -f "$metadata" ]; then
    printf '%s' "$PWD/vendor/gpui-kit/assets/icons" > "$metadata"
    printf 'assets-build\\n' >> "$POOL_LOG"
fi
export DEP_GPUI_KIT_DEFAULT_ICONS_ICONS_DIR="$(cat "$metadata")"
component_rustc_env="$DEP_GPUI_KIT_DEFAULT_ICONS_ICONS_DIR"
test "$component_rustc_env" = "$PWD/vendor/gpui-kit/assets/icons"
printf '%s\\n' "$CARGO_TARGET_DIR" >> "$POOL_LOG"
''')
            subprocess.run(['git','-C',str(root),'add','scripts'],check=True)
            subprocess.run(['git','-C',str(root),'-c','user.name=Fixture',
                            '-c','user.email=fixture@example.invalid','commit','-qm','metadata probe'],check=True)
            env['GITHUB_SHA']=subprocess.check_output(
                ['git','-C',str(root),'rev-parse','HEAD'],text=True).strip()
            # Positive control: the probe detects the legacy stale path.
            control=subprocess.run(['bash','scripts/ci/fast.sh'],cwd=root,
                env=dict(env,CARGO_TARGET_DIR=str(legacy)),capture_output=True,text=True,timeout=3)
            self.assertNotEqual(control.returncode,0)
            for _ in range(2):
                result=subprocess.run(['bash','scripts/ci/fast-local.sh'],cwd=root,
                    env=env,capture_output=True,text=True,timeout=3)
                self.assertEqual(result.returncode,0,result.stderr)
            rows=(root/'log').read_text().splitlines()
            self.assertEqual(rows[0],'assets-build')
            self.assertEqual(len(rows),3)  # One build-script run, two warm consumers.
            self.assertEqual(rows[1],rows[2])
            target=Path(rows[1])
            self.assertEqual(target.parent,legacy.parent)
            self.assertNotEqual(target,legacy)
            self.assertEqual((target/'icons-dir').read_text(),
                             str(root/'pool/0/source/vendor/gpui-kit/assets/icons'))
            self.assertEqual(metadata.read_text(),stale)


if __name__=='__main__':unittest.main()
