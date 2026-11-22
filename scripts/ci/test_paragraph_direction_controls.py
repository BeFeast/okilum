"""Exercise control supervision/restoration with fake Cargo, never Rust builds."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "paragraph-direction-controls.py"
SUPERVISOR = HERE / "test-group.py"
STATE = Path("vendor/gpui-component/crates/base/src/input/base/state.rs")
TEST = ("reader_editor::code_operations::row_layout_tests::"
        "edit2803_paragraph_direction_does_not_copy_reused_long_note_rows")
CANDIDATE = '''fn paragraph_rtl_at() {
        let mut source_bytes = 0;
        let rtl = borrowed_characters();
        rtl
}
'''


@unittest.skipUnless(sys.platform == "linux", "uses the real Linux subreaper")
class ParagraphDirectionControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.evidence = self.root / "evidence"
        self.state = self.root / STATE
        self.state.parent.mkdir(parents=True)
        self.state.write_text(CANDIDATE)
        (self.root / "crates/okilum-shell").mkdir(parents=True)
        bindir = self.root / "bin"
        bindir.mkdir()
        cargo = bindir / "cargo"
        cargo.write_text(f'''#!{sys.executable}
import json,os,pathlib,signal,sys,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
with (root/'calls.jsonl').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')
print('FAKE_COMPILER_PROGRESS',flush=True)
if os.environ.get('FIXTURE_MODE')=='hang-compile':
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    time.sleep(60)
print(json.dumps({{'reason':'compiler-artifact','executable':str(root/'test-binary'),
                  'profile':{{'test':True}},'target':{{'name':'okilum'}}}}),flush=True)
''')
        cargo.chmod(0o755)
        binary = self.root / "test-binary"
        binary.write_text(f'''#!{sys.executable}
import os,pathlib,sys
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
assert pathlib.Path.cwd()==root/'crates/okilum-shell'
text=(root/{str(STATE)!r}).read_text()
if 'let text = self.text.slice_line' in text:
    if os.environ.get('FIXTURE_MODE')=='zero-tests':
        print('running 0 tests\\ntest result: ok. 0 passed; 0 failed;'); sys.exit(0)
    print('running 1 test\\ntest {TEST} ... FAILED')
    if os.environ.get('FIXTURE_MODE')!='wrong-failure':
        print('paragraph direction copied long rows: 614400 source bytes for 2 queries')
    print('test result: FAILED. 0 passed; 1 failed'); sys.exit(101)
print('running 3 tests')
for name in [{TEST!r},
    'vendor_bidi_geometry::tests::paragraph_direction_iterator_preserves_first_strong_semantics',
    'vendor_bidi_geometry::tests::paragraph_direction_follows_the_first_strong_character']:
    print('test '+name+' ... ok')
print('test result: ok. 3 passed; 0 failed;')
''')
        binary.chmod(0o755)
        self.env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"],
                        FIXTURE_ROOT=str(self.root))
        self.command = [sys.executable, str(SCRIPT), "--evidence", str(self.evidence)]

    def run_control(self, mode="normal"):
        return subprocess.run(self.command, cwd=self.root, env=dict(self.env, FIXTURE_MODE=mode),
                              capture_output=True, text=True, timeout=10)

    def restore(self):
        return subprocess.run(self.command + ["--restore-only"], cwd=self.root, env=self.env,
                              capture_output=True, text=True, timeout=5)

    def test_compilation_and_runtime_are_separate_and_named_controls_fire(self):
        result = self.run_control()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PARAGRAPH_DIRECTION_OLD_CONTROL_FAILED_AS_EXPECTED", result.stdout)
        self.assertIn("PARAGRAPH_DIRECTION_CANDIDATE_PASS", result.stdout)
        self.assertEqual(self.state.read_text(), CANDIDATE)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        self.assertTrue(all("--no-run" in args for args in calls))
        for phase in ("old-compile", "old-test", "candidate-compile", "candidate-test"):
            self.assertIn(f"PARAGRAPH_DIRECTION_PHASE_END phase={phase}", result.stdout)
            self.assertTrue((self.evidence / f"{phase}.log").is_file())

    def test_supervisor_timeout_keeps_streamed_progress_and_independent_restore(self):
        result = subprocess.run(
            [sys.executable, str(SUPERVISOR), "--kill-after", ".3", "paragraph-control-timeout", "1",
             *self.command], cwd=self.root, env=dict(self.env, FIXTURE_MODE="hang-compile"),
            capture_output=True, text=True, timeout=6)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertIn("FAKE_COMPILER_PROGRESS", result.stdout)
        self.assertIn("FAKE_COMPILER_PROGRESS", (self.evidence / "old-compile.log").read_text())
        self.assertNotIn("PARAGRAPH_DIRECTION_CANDIDATE_PASS", result.stdout)
        # TERM/KILL bypassed the interrupted Python finally, just as in the
        # failed hosted run. The independent workflow step must recover it.
        self.assertNotEqual(self.state.read_text(), CANDIDATE)
        restored = self.restore()
        self.assertEqual(restored.returncode, 0, restored.stdout + restored.stderr)
        self.assertIn("PARAGRAPH_DIRECTION_RESTORED", restored.stdout)
        self.assertEqual(self.state.read_text(), CANDIDATE)

    def test_zero_tests_and_unrelated_failure_cannot_pass_as_old_copy_evidence(self):
        for mode in ("zero-tests", "wrong-failure"):
            with self.subTest(mode=mode):
                result = self.run_control(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("PARAGRAPH_DIRECTION_CANDIDATE_PASS", result.stdout)
                self.assertEqual(self.state.read_text(), CANDIDATE)

    def test_corrupt_backup_is_rejected_before_overwriting_source(self):
        result = self.run_control()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        (self.evidence / "candidate-state.rs").write_text("corrupted")
        self.state.write_text("keep the current source")
        restored = self.restore()
        self.assertNotEqual(restored.returncode, 0)
        self.assertIn("candidate backup hash mismatch", restored.stderr)
        self.assertEqual(self.state.read_text(), "keep the current source")
