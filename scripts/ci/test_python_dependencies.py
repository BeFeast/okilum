"""Python suites require an explicit, pinned environment on every runner."""
import importlib.util
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
GUARD = Path(__file__).with_name('python_dependencies.py')
spec = importlib.util.spec_from_file_location('python_dependencies', GUARD)
dependencies = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dependencies)


def runs_discovery(run, root=ROOT, seen=None):
    """Follow shell wrappers so supervision cannot hide a discovery job."""
    if any('unittest discover -s scripts/' + suite in run
           for suite in ['ci', 'releases', 'updater']):
        return True
    seen = set() if seen is None else seen
    for entrypoint in re.findall(r'\bscripts/ci/[\w.-]+\.sh\b', run):
        if entrypoint in seen:
            continue
        seen.add(entrypoint)
        script = root / entrypoint
        if script.is_file() and runs_discovery(script.read_text(), root, seen):
            return True
    return False


class PythonDependencies(unittest.TestCase):
    def test_discovery_follows_nested_wrappers_and_stops_at_cycles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / 'scripts/ci'
            scripts.mkdir(parents=True)
            (scripts / 'suite.sh').write_text(
                "python3 -I -m unittest discover -s scripts/ci -p 'test_*.py'\n")
            (scripts / 'wrapped.sh').write_text(
                'bash scripts/ci/wrapped.sh\nbash scripts/ci/suite.sh\n')
            (scripts / 'unrelated.sh').write_text('bash scripts/ci/unrelated.sh\n')
            self.assertTrue(runs_discovery('bash scripts/ci/wrapped.sh', root))
            self.assertFalse(runs_discovery('bash scripts/ci/unrelated.sh', root))

    def test_required_dependencies_are_available_at_the_pinned_versions(self):
        result = subprocess.run([sys.executable, '-I', str(GUARD)],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Python test dependency: PyYAML==', result.stdout)

    def test_missing_dependency_fails_with_an_actionable_message(self):
        # -S removes site-packages even when the invoking venv has PyYAML.
        result = subprocess.run([sys.executable, '-I', '-S', str(GUARD)],
                                capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Missing required Python test dependency PyYAML==', result.stderr)
        self.assertIn('(import yaml). Run:', result.stderr)
        self.assertIn('-m pip install -r', result.stderr)

    def test_wrong_version_is_rejected(self):
        with patch.object(dependencies.importlib, 'import_module') as load:
            load.return_value.__version__ = '0.0.0'
            with self.assertRaisesRegex(RuntimeError, "found '0.0.0'.*pip install -r"):
                dependencies.check()

    def test_every_full_discovery_job_prepares_the_environment(self):
        # Follow both direct discovery and the shell entrypoints used by local
        # and hosted full gates. Do not let a new runner inherit host PyYAML.
        import yaml
        jobs_checked = []
        for directory in ['.forgejo/workflows', '.github/workflows']:
            for path in sorted((ROOT / directory).glob('*.yml')):
                workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
                for name, job in workflow.get('jobs', {}).items():
                    steps = job.get('steps', [])
                    runs = [step.get('run', '') for step in steps]
                    if not any(runs_discovery(run) for run in runs):
                        continue
                    with self.subTest(workflow=path.name, job=name):
                        setup = next((i for i, run in enumerate(runs)
                                      if run == 'bash scripts/ci/setup-python-tests.sh'), None)
                        self.assertIsNotNone(setup, 'Missing pinned Python test setup')
                        self.assertNotIn('if', steps[setup])
                        self.assertNotIn('continue-on-error', steps[setup])
                        for i, run in enumerate(runs):
                            if runs_discovery(run):
                                self.assertLess(setup, i)
                            if 'scripts/releases/hourly_beta.py certify-source' in run:
                                # This switches to an older exact product tree;
                                # dependency tooling must come from the workflow.
                                self.assertLess(setup, i)
                        jobs_checked.append((directory, path.name, name))
        # #2589: certification check's Python suites run in its hosted run.
        self.assertIn(('.github/workflows', 'forgejo-check.yml', 'python'), jobs_checked)
        self.assertNotIn(('.forgejo/workflows', 'fast.yml', 'check'), jobs_checked)
        # Okilum left baldr (14.11): the daily Linux gate and the nightly source
        # sharing acceptance run their Python discovery in hosted jobs.
        self.assertIn(('.github/workflows', 'forgejo-linux.yml', 'linux-checks'), jobs_checked)
        self.assertIn(('.github/workflows', 'agent-checks.yml', 'linux'), jobs_checked)
        self.assertIn(('.github/workflows', 'forgejo-sync-source-sharing.yml', 'sync-source-sharing'), jobs_checked)


if __name__ == '__main__':
    unittest.main()
