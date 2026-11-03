"""Check actual nested watchdog ceilings, including setup and cleanup margins."""
import copy
from pathlib import Path
import re
import shlex
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
GROUP = re.compile(r'^\s*bash scripts/ci/test-group\.sh (\S+) (\d+) (.+)$', re.M)
PHASE = re.compile(r'^\s*phase (\S+) (\d+) (.+)$', re.M)
MARGIN = 60  # Includes the supervisor's 30 s TERM/KILL/reap grace.
JOB_MARGIN = 90  # Checkout/toolchain/fixture setup plus final cleanup.


def validate(workflow, scripts):
    """Read ceilings from production sources rather than duplicating a budget table."""
    visited = set()

    def children(command):
        args = shlex.split(command)
        if args[:2] == ['bash', 'scripts/ci/fast-local.sh']:
            source = scripts['fast-local.sh']
            mode = 'shell-full' if args[2:] == ['shell-full'] else 'fast'
            gate = re.search(r'\b' + mode + r'\) gate_script=(\S+); gate_limit=(\d+)', source)
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

    for name, job in workflow['jobs'].items():
        total = 0
        bounded = False
        for step in job['steps']:
            groups = [(g, int(limit), cmd) for g, limit, cmd in GROUP.findall(step.get('run', ''))]
            if not groups:
                continue
            bounded = True
            required = sum(child[1] for child in groups) + 30
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
    assert visited == {'fast.sh', 'shell-full.sh', 'live-preview-tests.sh', 'sync-source-sharing-fast.sh',
                       'sync-source-sharing.sh', 'sync-network-policy.sh'}, visited


class FastBudgets(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load((ROOT / '.forgejo/workflows/fast.yml').read_text())
        self.scripts = {path.name: path.read_text() for path in (ROOT / 'scripts/ci').glob('*.sh')}

    def test_every_nested_group_and_job_has_additive_headroom(self):
        validate(self.workflow, self.scripts)

    def test_hourly_beta_five_minute_python_steps_preserve_headroom(self):
        workflow = copy.deepcopy(self.workflow)
        for step in workflow['jobs']['check']['steps']:
            if '-python ' in step.get('run', ''):
                step['timeout-minutes'] = 5
        validate(workflow, self.scripts)

    def test_original_aggregate_limits_are_rejected(self):
        for gate, old in [('fast', 660), ('shell-full', 1110)]:
            with self.subTest(gate=gate):
                scripts = dict(self.scripts)
                scripts['fast-local.sh'] = re.sub(
                    r'(' + gate + r'\) gate_script=\S+; gate_limit=)\d+',
                    lambda match: match[1] + str(old), scripts['fast-local.sh'])
                with self.assertRaisesRegex(AssertionError, 'children/setup/margin'):
                    validate(self.workflow, scripts)
        workflow = copy.deepcopy(self.workflow)
        step = workflow['jobs']['sync-source-sharing']['steps'][-2]
        step['run'] = re.sub(r'sync-local \d+', 'sync-local 840', step['run'])
        with self.assertRaisesRegex(AssertionError, 'sync-local:'):
            validate(workflow, self.scripts)

    def test_increasing_any_inner_phase_or_lock_requires_resizing_ancestors(self):
        for name in ['fast.sh', 'shell-full.sh', 'sync-source-sharing-fast.sh']:
            source = self.scripts[name]
            pattern = r'(?m)(^\s*phase \S+ |^bash scripts/ci/test-group\.sh \S+ |^flock -w )(\d+)'
            for match in re.finditer(pattern, source):
                with self.subTest(script=name, phase=match[0]):
                    scripts = dict(self.scripts)
                    scripts[name] = source[:match.start(2)] + '9999' + source[match.end(2):]
                    with self.assertRaisesRegex(AssertionError, 'children/setup/margin'):
                        validate(self.workflow, scripts)

    def test_step_and_job_cleanup_margins_are_enforced(self):
        for job in ['check', 'shell-full', 'sync-source-sharing']:
            with self.subTest(job=job):
                workflow = copy.deepcopy(self.workflow)
                workflow['jobs'][job]['timeout-minutes'] = 1
                with self.assertRaisesRegex(AssertionError, job + ' job:'):
                    validate(workflow, self.scripts)
        workflow = copy.deepcopy(self.workflow)
        step = next(s for s in workflow['jobs']['check']['steps'] if 'ci-python' in s.get('run', ''))
        step['timeout-minutes'] = 3
        with self.assertRaisesRegex(AssertionError, 'check step:'):
            validate(workflow, self.scripts)
