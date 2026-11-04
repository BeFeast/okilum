"""Python suites require an explicit, pinned environment on every runner."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
GUARD = Path(__file__).with_name('python_dependencies.py')
spec = importlib.util.spec_from_file_location('python_dependencies', GUARD)
dependencies = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dependencies)


class PythonDependencies(unittest.TestCase):
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
        def runs_discovery(run):
            return any('unittest discover -s scripts/' + suite in run
                       for suite in ['ci', 'releases', 'updater']) or any(
                           entrypoint in run for entrypoint in [
                               'scripts/ci/check-linux.sh', 'scripts/ci/check-shared.sh',
                               'scripts/ci/sync-source-sharing.sh'])

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
                        jobs_checked.append((directory, path.name, name))
        self.assertIn(('.forgejo/workflows', 'fast.yml', 'check'), jobs_checked)
        self.assertIn(('.forgejo/workflows', 'ci.yml', 'linux-local'), jobs_checked)
        self.assertIn(('.github/workflows', 'agent-checks.yml', 'linux'), jobs_checked)
        self.assertIn(('.forgejo/workflows', 'fast.yml', 'sync-source-sharing'), jobs_checked)


if __name__ == '__main__':
    unittest.main()
