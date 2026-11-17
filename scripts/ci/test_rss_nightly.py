"""#2037 (Oleg 17.11): nightly arch-live RSS probe on GitHub-hosted, report-only."""
import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/rss-nightly.yml'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


report = load('rss_probe_report', 'scripts/ci/rss-probe-report.py')
probe = load('edit_memory', 'scripts/check-edit-memory-native.py')
samples = load('edit_memory_samples', 'scripts/ci/test_edit_memory_native.py')


def probe_log(readings, restored=None, empty=None, opened=None, label='arch-live'):
    """The probe's own stdout for these readings, as its main() prints it."""
    restored = [True] * len(readings) if restored is None else restored
    lines = [f'{label} cycle {n} rss_kib {rss} restored {ok}'
             for n, (rss, ok) in enumerate(zip(readings, restored))]
    try:
        result, checks = probe.judge(readings, restored, empty or samples.MEASURED_EMPTY,
                                     opened or samples.MEASURED_OPEN)
    except probe.Invalid as error:
        return '\n'.join(lines + [f'{label} INVALID: {error}']) + '\n'
    print_ = io.StringIO()
    with contextlib.redirect_stdout(print_):
        print(label, result, flush=True)
        print(label, checks, flush=True)
    return '\n'.join(lines) + '\n' + print_.getvalue()


def run(text, label='arch-live'):
    with tempfile.TemporaryDirectory() as tmp:
        log, summary = Path(tmp) / 'probe.log', Path(tmp) / 'summary.md'
        if text is not None:
            log.write_text(text)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = report.main(['rss-probe-report.py', str(log), label, str(summary)])
        return code, out.getvalue(), summary.read_text()


class Readout(unittest.TestCase):
    def test_measured_flat_run_passes_without_annotation(self):
        code, out, summary = run(probe_log(samples.MEASURED_40))
        self.assertEqual(code, 0)
        self.assertIn('**PASS**', summary)
        self.assertIn('40 cycles', summary)
        self.assertIn('`slope_kib_per_cycle`', summary)
        self.assertNotIn('::', out)

    def test_growth_is_an_error_annotation_and_still_exits_zero(self):
        # Control: steady growth at twice the limit fails the probe's own check.
        growing = [samples.MEASURED_40[0] + 128 * n for n in range(40)]
        code, out, summary = run(probe_log(growing))
        self.assertEqual(code, 0)
        self.assertIn('**FAIL**', summary)
        self.assertIn('failed: no_growth', summary)
        self.assertIn('| `no_growth` | **no** |', summary)
        self.assertIn('::error title=arch-live RSS no_growth probe (report-only)::FAIL: failed: no_growth', out)

    def test_unrestored_note_fails(self):
        restored = [True] * 10 + [False] * 30
        code, out, summary = run(probe_log(samples.MEASURED_40, restored))
        self.assertIn('failed: note_restored_every_cycle', summary)
        self.assertIn('::error ', out)

    def test_inconclusive_run_is_a_warning(self):
        code, out, summary = run(probe_log(samples.MEASURED_40, opened=1, empty=2))
        self.assertEqual(code, 0)
        self.assertIn('**INVALID**', summary)
        self.assertIn('RSS did not rise when the editor opened', summary)
        self.assertIn('::warning ', out)

    def test_missing_or_truncated_output_is_never_a_pass(self):
        for text in (None, '', probe_log(samples.MEASURED_40).rsplit('arch-live {', 2)[0],
                     probe_log(samples.MEASURED_40, label='source-live')):
            with self.subTest(text=(text or '')[-60:]):
                code, out, summary = run(text)
                self.assertEqual(code, 0)
                self.assertIn('**NO_RESULT**', summary)
                self.assertIn('::warning ', out)


class Workflow(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load(WORKFLOW.read_text())
        self.steps = self.workflow['jobs']['arch-live-rss']['steps']

    def step(self, name):
        return next(step for step in self.steps if step.get('name') == name)

    def test_nightly_on_main_only(self):
        self.assertEqual(set(self.workflow['on']), {'schedule', 'workflow_dispatch'})
        self.assertEqual(len(self.workflow['on']['schedule']), 1)
        self.assertEqual(self.workflow['permissions'], {'contents': 'read'})
        self.assertNotIn('secrets.', WORKFLOW.read_text())

    def test_probe_is_the_fixed_40_cycle_live_check_and_report_only(self):
        probe_step = self.step('Probe RSS over 40 Live Preview edit cycles')
        self.assertIn('python scripts/check-edit-memory-native.py /usr/bin/okilum', probe_step['run'])
        self.assertIn('arch-live 40 live', probe_step['run'])
        self.assertIn('set -o pipefail', probe_step['run'])
        self.assertIs(probe_step['continue-on-error'], True)
        verdict = self.step('Report the probe verdict')
        self.assertIn('scripts/ci/rss-probe-report.py rss-probe.log arch-live', verdict['run'])
        self.assertNotIn('continue-on-error', self.workflow['jobs']['arch-live-rss'])

    def test_same_arch_image_as_the_calibrated_runs(self):
        arch = yaml.safe_load((ROOT / '.github/workflows/forgejo-arch.yml').read_text())['jobs']['arch']
        job = self.workflow['jobs']['arch-live-rss']
        self.assertEqual(job['container'], arch['container'])
        build = self.step('Build package as an unprivileged user')['run']
        self.assertIn('OKILUM_SETTINGS_UI_HARNESS=0', build)
        self.assertIn('bash scripts/arch/build.sh', build)
        # Only the probe: no package validation, smoke or package artifact.
        text = WORKFLOW.read_text()
        for absent in ('smoke-x11.sh', 'desktop-file-validate', 'unsigned-arch', 'pkg.tar.zst\n        retention'):
            self.assertNotIn(absent, text)

    def test_nothing_depends_on_it(self):
        for directory in ('.github/workflows', '.forgejo/workflows'):
            for path in (ROOT / directory).glob('*.yml'):
                if path != WORKFLOW:
                    self.assertNotIn('rss-nightly', path.read_text(), path.name)


if __name__ == '__main__':
    unittest.main()
