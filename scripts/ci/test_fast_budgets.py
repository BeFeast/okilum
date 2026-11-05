"""Check actual nested watchdog ceilings, including setup and cleanup margins."""
import copy
import importlib.util
from pathlib import Path
import re
import shlex
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
WRAPPER = r'(?:scripts/ci/test-group\.sh|"\$OKILUM_TEST_GROUP"|"\$\{OKILUM_TEST_GROUP:-scripts/ci/test-group\.sh\}")'
GROUP = re.compile(r'^\s*bash ' + WRAPPER + r' (\S+) (\d+) (.+)$', re.M)
PHASE = re.compile(r'^\s*phase (\S+) (\d+) (.+)$', re.M)
MARGIN = 60  # Includes the supervisor's 30 s TERM/KILL/reap grace.
JOB_MARGIN = 90  # Checkout/toolchain/fixture setup plus final cleanup.
# #2589: certification check and shell-full wait on the `bridge` runner. Its
# primary service (capacity 8) stops every job at `runner.timeout: 1h`.
BRIDGE_RUNNER_CEILING = 60 * 60
# Worst-case poll lag (60 s), cancellation and temporary-ref cleanup requests.
BRIDGE_CLEANUP = 120
# Forgejo fast.yml waiter job -> the GitHub workflow that executes it.
HOSTED = {
    'check': '.github/workflows/forgejo-check.yml',
    'shell-full': '.github/workflows/forgejo-shell-full.yml',
}
# #2589 hosted shell-full exercises, GitHub runs 42049574081 and 42052787792
# (cold, 4 vCPU): the slower phase seconds and bridge wait.
HOSTED_COMPILE, HOSTED_EXECUTION, HOSTED_BRIDGE = 530.258, 422.195, 1050
# #2589 hosted check exercises, GitHub runs 42070990602, 42073735620 and
# 42073756719 (cold, 4 vCPU, docs/fast-runner.md): the slowest group seconds of
# each gate phase and the slowest bridge wait.
CHECK_MEASURED = {
    'ci-python': 63.806, 'releases-python': 1.954, 'updater-python': 0.178,
    'appimage-channel-tests': 25.401, 'build': 467.954, 'workspace-test-check': 280.471,
    'list-tests': 10.783, 'paste-tests': 0.350,
}
CHECK_BRIDGE = 872
# Minimum limit/measurement ratio; 1.5 unless listed. The cold hosted desktop
# build is the thin phase: its unchanged 540 s deadline over the measured cold
# compile (no target cache on exact-source certification). See docs/fast-runner.md.
CHECK_RATIO = {'build': 1.1}
# Slowest observed hosted push-to-start (agent-checks 41999797379, 5 November).
HOSTED_QUEUE_MAX = 980
WAITER = 'python3 "$OKILUM_HOSTED_BRIDGE/github-pr.py"'
_spec = importlib.util.spec_from_file_location('budget_bridge', Path(__file__).with_name('github-pr.py'))
BRIDGE = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(BRIDGE)


def validate(workflow, scripts, hosted, bridge=BRIDGE):
    """Read ceilings from production sources rather than duplicating a budget table.

    Returns each bridged lane's shared queue/run budget.
    """
    visited = set()

    def children(command):
        args = shlex.split(command)
        if args[:1] != ['bash'] or not args[1].startswith('scripts/ci/'):
            return [], 0
        name = Path(args[1]).name
        assert name in scripts, f'Unmodelled nested script: {name}'
        source = scripts[name].replace('\\\n', ' ')
        visited.add(name)
        groups = [(g, int(limit), cmd) for g, limit, cmd in GROUP.findall(source)]
        groups += [(g, int(limit), cmd) for g, limit, cmd in PHASE.findall(source)]
        lock = re.search(r'^flock -w (\d+) ', source, re.M)
        return groups, int(lock[1]) if lock else 0

    def group(name, limit, command):
        nested, setup = children(command) if command else ([], 0)
        if nested:
            required = sum(child[1] for child in nested) + setup + MARGIN
            assert limit >= required, f'{name}: {limit}s < children/setup/margin {required}s'
            for child in nested:
                group(*child)

    def job_ceiling(name, job):
        """Bounded steps (groups or their own ceilings) plus setup/cleanup margin."""
        total = 0
        bounded = False
        for step in job['steps']:
            groups = [(g, int(limit), cmd) for g, limit, cmd in GROUP.findall(step.get('run', ''))]
            if not groups and 'timeout-minutes' not in step:
                continue
            bounded = True
            required = sum(child[1] for child in groups) + 30 if groups else 0
            if 'timeout-minutes' in step:
                ceiling = int(step['timeout-minutes']) * 60
                assert ceiling >= required, f'{name} step: {ceiling}s < {required}s'
                total += ceiling
            else:
                total += required
            for child in groups:
                group(*child)
        if bounded:
            ceiling = int(job['timeout-minutes']) * 60
            assert ceiling >= total + JOB_MARGIN, f'{name} job: {ceiling}s < {total + JOB_MARGIN}s'
            return ceiling
        return None

    for name, job in workflow['jobs'].items():
        job_ceiling(name, job)
    budgets = {}
    for lane, run in hosted.items():
        hosted_jobs = run['jobs']
        # Every hosted job of the run is bounded; GitHub stops each at its own ceiling.
        hosted_ceiling = max(job_ceiling('hosted ' + name, job) for name, job in hosted_jobs.items())
        # The bridge's evidence names real, unconditional gate steps of this run.
        gate, step_name, run_timeout = bridge.LANES[lane]
        for job_name, required in ((gate, step_name), *getattr(bridge, 'ALSO_REQUIRED', {}).get(lane, ())):
            assert job_name in hosted_jobs, f'{lane}: hosted job {job_name} missing'
            named = [s for s in hosted_jobs[job_name]['steps'] if s.get('name') == required]
            assert len(named) == 1, f'{lane}: hosted step {required!r} missing or ambiguous'
            assert 'if' not in named[0] and 'continue-on-error' not in named[0], f'{lane}: hosted step {required!r} is conditional'
            assert 'if' not in hosted_jobs[job_name], f'{lane}: hosted job {job_name} is conditional'
        # The Forgejo waiter: validation + bridge step inside the bridge runner.
        waiter = workflow['jobs'][lane]
        assert waiter['runs-on'] == 'bridge', waiter['runs-on']
        ceiling = int(waiter['timeout-minutes']) * 60
        assert ceiling <= BRIDGE_RUNNER_CEILING, f'{lane} job: {ceiling}s > runner ceiling {BRIDGE_RUNNER_CEILING}s'
        step = next(s for s in waiter['steps'] if WAITER in s.get('run', ''))
        assert step['env']['CI_LANE'] == lane, step['env']
        history = sum(int(t) for t in re.findall(r'^\s*timeout (\d+) git fetch', step['run'], re.M))
        budget = bridge.BRIDGE_BUDGETS[lane]
        required = history + budget + BRIDGE_CLEANUP
        assert int(step['timeout-minutes']) * 60 >= required, f'{lane} bridge step: < {required}s'
        # GitHub's own job deadline must fire, and be observed, before the waiter cancels.
        assert run_timeout >= hosted_ceiling + BRIDGE_CLEANUP, f'{lane} bridge run timeout: {run_timeout}s < hosted job'
        budgets[lane] = budget
    assert visited == {'fast.sh', 'shell-full.sh', 'live-preview-tests.sh', 'sync-source-sharing-fast.sh',
                       'sync-source-sharing.sh', 'sync-network-policy.sh'}, visited
    return budgets


class FastBudgets(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load((ROOT / '.forgejo/workflows/fast.yml').read_text())
        self.hosted = {lane: yaml.safe_load((ROOT / path).read_text()) for lane, path in HOSTED.items()}
        self.scripts = {path.name: path.read_text() for path in (ROOT / 'scripts/ci').glob('*.sh')}

    def validate(self, workflow=None, scripts=None, hosted=None, bridge=BRIDGE):
        return validate(self.workflow if workflow is None else workflow,
                        self.scripts if scripts is None else scripts,
                        self.hosted if hosted is None else hosted, bridge)

    def hosted_step(self, hosted, lane, job, marker):
        return next(s for s in hosted[lane]['jobs'][job]['steps'] if marker in s.get('run', ''))

    def test_every_nested_group_and_job_has_additive_headroom(self):
        self.validate()

    def test_hourly_beta_five_minute_python_steps_preserve_headroom(self):
        # #2536's 5 min Python step ceilings, now in the hosted check run.
        hosted = copy.deepcopy(self.hosted)
        steps = [s for s in hosted['check']['jobs']['python']['steps'] if '-python ' in s.get('run', '')]
        self.assertEqual(len(steps), 3)
        for step in steps:
            self.assertEqual(step['timeout-minutes'], 5)
        self.validate(hosted=hosted)

    def test_original_aggregate_limits_are_rejected(self):
        # The hosted gates keep the former locked fast.sh and shell-full.sh envelopes.
        for lane, job, command, smaller in [('check', 'check', 'fast.sh 2040', 'fast.sh 660'),
                                            ('shell-full', 'shell-full', 'shell-full.sh 1920', 'shell-full.sh 1110')]:
            with self.subTest(lane=lane):
                hosted = copy.deepcopy(self.hosted)
                step = self.hosted_step(hosted, lane, job, command)
                step['run'] = step['run'].replace(command, smaller)
                with self.assertRaisesRegex(AssertionError, 'children/setup/margin'):
                    self.validate(hosted=hosted)
        workflow = copy.deepcopy(self.workflow)
        step = workflow['jobs']['sync-source-sharing']['steps'][-2]
        step['run'] = re.sub(r'sync-local \d+', 'sync-local 840', step['run'])
        with self.assertRaisesRegex(AssertionError, 'sync-local:'):
            self.validate(workflow)

    def test_increasing_any_inner_phase_or_lock_requires_resizing_ancestors(self):
        for name in ['fast.sh', 'shell-full.sh', 'sync-source-sharing-fast.sh']:
            source = self.scripts[name]
            pattern = r'(?m)(^\s*phase \S+ |^bash scripts/ci/test-group\.sh \S+ |^flock -w )(\d+)'
            for match in re.finditer(pattern, source):
                with self.subTest(script=name, phase=match[0]):
                    scripts = dict(self.scripts)
                    scripts[name] = source[:match.start(2)] + '9999' + source[match.end(2):]
                    with self.assertRaisesRegex(AssertionError, 'children/setup/margin'):
                        self.validate(scripts=scripts)

    def test_step_and_job_cleanup_margins_are_enforced(self):
        for job in ['check', 'shell-full', 'sync-source-sharing']:
            with self.subTest(job=job):
                workflow = copy.deepcopy(self.workflow)
                workflow['jobs'][job]['timeout-minutes'] = 1
                with self.assertRaisesRegex(AssertionError, job + ' job:'):
                    self.validate(workflow)
        # Hosted: each step covers its group, each job covers apt + its steps.
        for lane, job, marker, step_value, job_value in [
                ('check', 'python', 'ci-python 180', 3, 26),
                ('check', 'check', 'fast.sh 2040', 34, 46),
                ('shell-full', 'shell-full', 'shell-full.sh 1920', 32, 44)]:
            for field, value in [('step', step_value), ('job', job_value)]:
                with self.subTest(lane=lane, job=job, field=field):
                    hosted = copy.deepcopy(self.hosted)
                    target = hosted[lane]['jobs'][job]
                    if field == 'step':
                        target = self.hosted_step(hosted, lane, job, marker)
                    target['timeout-minutes'] = value
                    with self.assertRaisesRegex(AssertionError, f'hosted {job} {field}:'):
                        self.validate(hosted=hosted)

    def test_certification_steps_reserve_additive_time_in_both_certification_jobs(self):
        for name in ['check', 'shell-full']:
            with self.subTest(job=name):
                workflow = copy.deepcopy(self.workflow)
                step = next(s for s in workflow['jobs'][name]['steps']
                            if 'certification-source ' in s.get('run', ''))
                self.assertEqual(GROUP.findall(step['run'])[0][:2], ('certification-source', '90'))
                step['run'] = step['run'].replace('certification-source 90', 'certification-source 900')
                step['timeout-minutes'] = 16
                with self.assertRaisesRegex(AssertionError, name + ' job:'):
                    self.validate(workflow)
                workflow = copy.deepcopy(self.workflow)
                workflow['jobs'][name]['timeout-minutes'] = 37
                with self.assertRaisesRegex(AssertionError, name + ' job:'):
                    self.validate(workflow)

    def test_bridge_waiters_fit_the_bridge_runner_ceiling(self):
        for name in ['check', 'shell-full']:
            with self.subTest(job=name):
                workflow = copy.deepcopy(self.workflow)
                workflow['jobs'][name]['timeout-minutes'] = 61
                with self.assertRaisesRegex(AssertionError, 'runner ceiling'):
                    self.validate(workflow)
                workflow = copy.deepcopy(self.workflow)
                workflow['jobs'][name]['runs-on'] = 'fast-linux'
                with self.assertRaises(AssertionError):
                    self.validate(workflow)

    def test_bridge_steps_cover_history_shared_budget_and_cleanup(self):
        for name in ['check', 'shell-full']:
            with self.subTest(job=name):
                workflow = copy.deepcopy(self.workflow)
                step = next(s for s in workflow['jobs'][name]['steps'] if WAITER in s.get('run', ''))
                step['timeout-minutes'] = 55
                with self.assertRaisesRegex(AssertionError, name + ' bridge step:'):
                    self.validate(workflow)
                step['timeout-minutes'] = 56
                step['run'] = step['run'].replace('timeout 60 git fetch', 'timeout 120 git fetch')
                with self.assertRaisesRegex(AssertionError, name + ' bridge step:'):
                    self.validate(workflow)
                workflow = copy.deepcopy(self.workflow)
                step = next(s for s in workflow['jobs'][name]['steps'] if WAITER in s.get('run', ''))
                step['env'] = dict(step['env'], CI_LANE='fast')
                with self.assertRaises(AssertionError):
                    self.validate(workflow)

    def test_hosted_job_deadline_fires_before_the_waiter_cancels(self):
        # Either hosted check job, or the shell-full job, outgrowing the run timeout.
        for lane, job, value in [('check', 'check', 48), ('check', 'python', 48), ('shell-full', 'shell-full', 46)]:
            with self.subTest(lane=lane, job=job):
                hosted = copy.deepcopy(self.hosted)
                hosted[lane]['jobs'][job]['timeout-minutes'] = value
                with self.assertRaisesRegex(AssertionError, lane + ' bridge run timeout'):
                    self.validate(hosted=hosted)

    def test_bridge_evidence_names_unconditional_hosted_gate_steps(self):
        expected = [('check', 'Build Linux desktop and run fast unit tests'),
                    ('python', 'Run the full CI tooling Python suite'),
                    ('python', 'Run the full releases Python suite'),
                    ('python', 'Run the full updater Python suite')]
        self.assertEqual([BRIDGE.LANES['check'][:2], *BRIDGE.ALSO_REQUIRED['check']], expected)
        for job, step_name in expected:
            with self.subTest(job=job, step=step_name):
                for mutation in ['rename', 'if', 'job-if']:
                    hosted = copy.deepcopy(self.hosted)
                    target = hosted['check']['jobs'][job]
                    step = next(s for s in target['steps'] if s.get('name') == step_name)
                    if mutation == 'rename':
                        step['name'] += ' (optional)'
                    elif mutation == 'if':
                        step['if'] = 'false'
                    else:
                        target['if'] = 'false'
                    with self.assertRaisesRegex(AssertionError, 'check: hosted'):
                        self.validate(hosted=hosted)

    def test_hosted_phases_and_queue_keep_margin_over_measurements(self):
        limits = {group: int(limit) for group, limit, _ in PHASE.findall(self.scripts['shell-full.sh'])}
        self.assertGreaterEqual(limits['shell-binary-compile'], 1.5 * HOSTED_COMPILE)
        self.assertGreaterEqual(limits['shell-binary-tests'], 2 * HOSTED_EXECUTION)
        budgets = self.validate()
        self.assertGreaterEqual(budgets['shell-full'] - HOSTED_BRIDGE, 2 * HOSTED_QUEUE_MAX)

    def test_hosted_check_phases_and_queue_keep_margin_over_measurements(self):
        limits = {group: int(limit) for group, limit, _ in PHASE.findall(self.scripts['fast.sh'])}
        for job in self.hosted['check']['jobs'].values():
            for step in job['steps']:
                limits.update({g: int(limit) for g, limit, _ in GROUP.findall(step.get('run', ''))})
        self.assertEqual(set(CHECK_MEASURED), set(limits) - {'fast.sh', 'live-preview-regressions'})
        for phase, seconds in CHECK_MEASURED.items():
            with self.subTest(phase=phase):
                self.assertGreater(seconds, 0)
                self.assertGreaterEqual(limits[phase], CHECK_RATIO.get(phase, 1.5) * seconds)
        budgets = self.validate()
        self.assertGreaterEqual(budgets['check'] - CHECK_BRIDGE, 2 * HOSTED_QUEUE_MAX)
