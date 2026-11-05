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
# #2589: certification shell-full waits on the `bridge` runner. Its primary
# service (capacity 8) stops every job at `runner.timeout: 1h`.
BRIDGE_RUNNER_CEILING = 60 * 60
# Worst-case poll lag (60 s), cancellation and temporary-ref cleanup requests.
BRIDGE_CLEANUP = 120
HOSTED = '.github/workflows/forgejo-shell-full.yml'
# #2589 hosted exercise, GitHub run 42049574081 (cold, 4 vCPU): phase seconds and
# the whole bridge wait. Slowest observed hosted push-to-start: 980 s.
HOSTED_COMPILE, HOSTED_EXECUTION, HOSTED_BRIDGE = 490.372, 390.094, 981
HOSTED_QUEUE_MAX = 980
WAITER = 'python3 "$OKILUM_HOSTED_BRIDGE/github-pr.py"'
_spec = importlib.util.spec_from_file_location('budget_bridge', Path(__file__).with_name('github-pr.py'))
BRIDGE = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(BRIDGE)


def validate(workflow, scripts, hosted, bridge=BRIDGE):
    """Read ceilings from production sources rather than duplicating a budget table."""
    visited = set()

    def children(command):
        args = shlex.split(command)
        if args[:2] == ['bash', 'scripts/ci/fast-local.sh']:
            source = scripts['fast-local.sh']
            assert args[2:] in ([], ['fast']), command
            gate = re.search(r'\bfast\) gate_script=(\S+); gate_limit=(\d+)', source)
            assert gate, command
            wait = int(re.search(r'OKILUM_FAST_SLOT_WAIT_SECONDS:-(\d+)', source)[1])
            assert MARGIN >= wait + 30, 'slot wait must leave cleanup headroom'
            return [(gate[1], int(gate[2]), 'bash scripts/ci/' + gate[1])], 0
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
    hosted_ceiling = job_ceiling('hosted shell-full', hosted['jobs']['shell-full'])
    assert visited == {'fast.sh', 'shell-full.sh', 'live-preview-tests.sh', 'sync-source-sharing-fast.sh',
                       'sync-source-sharing.sh', 'sync-network-policy.sh'}, visited

    # The Forgejo waiter: validation + bridge step inside the bridge runner.
    waiter = workflow['jobs']['shell-full']
    assert waiter['runs-on'] == 'bridge', waiter['runs-on']
    ceiling = int(waiter['timeout-minutes']) * 60
    assert ceiling <= BRIDGE_RUNNER_CEILING, f'shell-full job: {ceiling}s > runner ceiling {BRIDGE_RUNNER_CEILING}s'
    step = next(s for s in waiter['steps'] if WAITER in s.get('run', ''))
    assert step['env']['CI_LANE'] == 'shell-full', step['env']
    history = sum(int(t) for t in re.findall(r'^\s*timeout (\d+) git fetch', step['run'], re.M))
    budget = bridge.BRIDGE_BUDGETS['shell-full']
    required = history + budget + BRIDGE_CLEANUP
    assert int(step['timeout-minutes']) * 60 >= required, f'bridge step: < {required}s'
    # GitHub's own job deadline must fire, and be observed, before the waiter cancels.
    run_timeout = bridge.LANES['shell-full'][2]
    assert run_timeout >= hosted_ceiling + BRIDGE_CLEANUP, f'bridge run timeout: {run_timeout}s < hosted job'
    return budget


class FastBudgets(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load((ROOT / '.forgejo/workflows/fast.yml').read_text())
        self.hosted = yaml.safe_load((ROOT / HOSTED).read_text())
        self.scripts = {path.name: path.read_text() for path in (ROOT / 'scripts/ci').glob('*.sh')}

    def validate(self, workflow=None, scripts=None, hosted=None, bridge=BRIDGE):
        return validate(self.workflow if workflow is None else workflow,
                        self.scripts if scripts is None else scripts,
                        self.hosted if hosted is None else hosted, bridge)

    def test_every_nested_group_and_job_has_additive_headroom(self):
        self.validate()

    def test_hourly_beta_five_minute_python_steps_preserve_headroom(self):
        workflow = copy.deepcopy(self.workflow)
        for step in workflow['jobs']['check']['steps']:
            if '-python ' in step.get('run', ''):
                step['timeout-minutes'] = 5
        self.validate(workflow)

    def test_original_aggregate_limits_are_rejected(self):
        scripts = dict(self.scripts)
        scripts['fast-local.sh'] = re.sub(r'(fast\) gate_script=\S+; gate_limit=)\d+',
                                          lambda match: match[1] + '660', scripts['fast-local.sh'])
        with self.assertRaisesRegex(AssertionError, 'children/setup/margin'):
            self.validate(scripts=scripts)
        # The hosted shell suite keeps the former locked shell-full.sh envelope.
        hosted = copy.deepcopy(self.hosted)
        step = next(s for s in hosted['jobs']['shell-full']['steps'] if 'shell-full.sh 1920' in s.get('run', ''))
        step['run'] = step['run'].replace('shell-full.sh 1920', 'shell-full.sh 1110')
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
        workflow = copy.deepcopy(self.workflow)
        step = next(s for s in workflow['jobs']['check']['steps'] if 'ci-python' in s.get('run', ''))
        step['timeout-minutes'] = 3
        with self.assertRaisesRegex(AssertionError, 'check step:'):
            self.validate(workflow)
        # Hosted: the suite step covers its group, the job covers apt + suite.
        for field, value, message in [('step', 32, 'hosted shell-full step:'), ('job', 44, 'hosted shell-full job:')]:
            with self.subTest(hosted=field):
                hosted = copy.deepcopy(self.hosted)
                target = hosted['jobs']['shell-full']
                if field == 'step':
                    target = next(s for s in target['steps'] if 'shell-full.sh 1920' in s.get('run', ''))
                target['timeout-minutes'] = value
                with self.assertRaisesRegex(AssertionError, message):
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
        workflow['jobs']['shell-full']['timeout-minutes'] = 37
        with self.assertRaisesRegex(AssertionError, 'shell-full job:'):
            self.validate(workflow)

    def test_bridge_waiter_fits_the_bridge_runner_ceiling(self):
        workflow = copy.deepcopy(self.workflow)
        workflow['jobs']['shell-full']['timeout-minutes'] = 61
        with self.assertRaisesRegex(AssertionError, 'runner ceiling'):
            self.validate(workflow)
        workflow = copy.deepcopy(self.workflow)
        workflow['jobs']['shell-full']['runs-on'] = 'fast-linux'
        with self.assertRaises(AssertionError):
            self.validate(workflow)

    def test_bridge_step_covers_history_shared_budget_and_cleanup(self):
        workflow = copy.deepcopy(self.workflow)
        step = next(s for s in workflow['jobs']['shell-full']['steps'] if WAITER in s.get('run', ''))
        step['timeout-minutes'] = 55
        with self.assertRaisesRegex(AssertionError, 'bridge step:'):
            self.validate(workflow)
        step['timeout-minutes'] = 56
        step['run'] = step['run'].replace('timeout 60 git fetch', 'timeout 120 git fetch')
        with self.assertRaisesRegex(AssertionError, 'bridge step:'):
            self.validate(workflow)

    def test_hosted_job_deadline_fires_before_the_waiter_cancels(self):
        hosted = copy.deepcopy(self.hosted)
        hosted['jobs']['shell-full']['timeout-minutes'] = 47
        with self.assertRaisesRegex(AssertionError, 'bridge run timeout'):
            self.validate(hosted=hosted)

    def test_hosted_phases_and_queue_keep_margin_over_measurements(self):
        limits = {group: int(limit) for group, limit, _ in PHASE.findall(self.scripts['shell-full.sh'])}
        self.assertGreaterEqual(limits['shell-binary-compile'], 1.5 * HOSTED_COMPILE)
        self.assertGreaterEqual(limits['shell-binary-tests'], 2.5 * HOSTED_EXECUTION)
        budget = self.validate()
        self.assertGreaterEqual(budget - HOSTED_BRIDGE, 2 * HOSTED_QUEUE_MAX)
