import importlib.util
from pathlib import Path
import re
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location('coalesce', Path(__file__).with_name('coalesce.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CoalescingTests(unittest.TestCase):
    def test_hourly_tick_builds_only_unpublished_source(self):
        for platform in ['windows', 'macos', 'linux']:
            self.assertTrue(module.needed('schedule', None, 'new', platform))
            self.assertFalse(module.needed('schedule', {'source': 'new', 'platform': platform}, 'new', platform))

    def test_push_cancels_without_compiling_and_manual_build_bypasses_window(self):
        self.assertFalse(module.needed('push', None, 'new', 'macos'))
        for event in ['workflow_dispatch', 'pull_request']:
            self.assertTrue(module.needed(event, {'source': 'new', 'platform': 'macos'}, 'new', 'macos'))

    def test_unknown_event_and_mismatched_descriptor_fail_closed(self):
        with self.assertRaises(ValueError):
            module.needed('unknown', None, 'new', 'macos')
        for item in [{'source': 'old', 'platform': 'macos'}, {'source': 'new', 'platform': 'linux'}]:
            with self.assertRaises(ValueError):
                module.needed('schedule', item, 'new', 'macos')


class ScheduledMainTests(unittest.TestCase):
    @patch.object(module.subprocess, 'run')
    def test_windows_schedule_builds_main_tip_not_its_old_snapshot(self, run):
        # Skipping an outdated snapshot starved beta: merges outpace the tick.
        head = 'a' * 40
        run.return_value = SimpleNamespace(stdout=head + '\trefs/heads/main\n')
        self.assertEqual(module.scheduled_source('schedule', 'b' * 40, 'windows'), head)
        self.assertEqual(module.scheduled_source('schedule', head, 'windows'), head)
        self.assertEqual(run.call_args.kwargs['timeout'], 15)

    @patch.object(module.subprocess, 'run')
    def test_manual_pr_and_other_platforms_never_query_main(self, run):
        for event, platform in [('workflow_dispatch', 'windows'),
                                ('pull_request', 'windows'), ('push', 'windows'),
                                ('schedule', 'macos')]:
            self.assertEqual(module.scheduled_source(event, 'old', platform), 'old')
        run.assert_not_called()

    @patch.object(module.subprocess, 'run')
    def test_unknown_remote_result_is_failure_not_successful_skip(self, run):
        for output in ['', 'garbage', 'a' * 40 + '\trefs/heads/other']:
            run.return_value = SimpleNamespace(stdout=output)
            with self.assertRaises(ValueError):
                module.scheduled_source('schedule', 'old', 'windows')
        run.side_effect = module.subprocess.TimeoutExpired('git', 15)
        with self.assertRaises(module.subprocess.TimeoutExpired):
            module.scheduled_source('schedule', 'old', 'windows')


class LinuxWindowTests(unittest.TestCase):
    @patch.object(module.subprocess, 'run')
    def test_stale_tick_builds_tip_and_manual_bypasses(self, run):
        head = 'a' * 40
        run.return_value = SimpleNamespace(stdout=head + '\trefs/heads/main\n')
        self.assertEqual(module.scheduled_source('schedule', 'b' * 40, 'linux'), head)
        self.assertEqual(module.scheduled_source('schedule', head, 'linux'), head)
        self.assertTrue(module.needed('schedule', None, head, 'linux'))
        self.assertFalse(module.needed('schedule', {'source': head, 'platform': 'linux'}, head, 'linux'))
        run.reset_mock()
        for event in ['workflow_dispatch', 'pull_request']:
            self.assertEqual(module.scheduled_source(event, 'old', 'linux'), 'old')
            self.assertTrue(module.needed(event, {'source': 'old', 'platform': 'linux'}, 'old', 'linux'))
        run.assert_not_called()

    def test_linux_workflow_uses_light_selector_and_preserves_publication(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / '.forgejo/workflows/linux-release.yml').read_text()
        self.assertIn('cron: "*/30 * * * *"', workflow)
        self.assertNotIn('  push:', workflow)
        self.assertIn('  workflow_dispatch:', workflow)
        self.assertIn('runs-on: bridge', workflow)
        # The release checkout builds the commit select chose (main tip on a tick).
        release = workflow.split('  release:\n', 1)[1].split('\n  arch-github:\n', 1)[0]
        header, *steps = re.split(r'\n      - ', release)
        checkouts = [s for s in steps if s.startswith('uses:') and 'checkout@' in s.splitlines()[0]]
        self.assertEqual(len(checkouts), 2)
        trusted, selected = checkouts
        self.assertNotRegex(trusted, r'(?m)^          ref:')
        self.assertRegex(selected, r'(?m)^          ref: \$\{\{ needs.select.outputs.source \}\}$')
        for checkout in checkouts:
            self.assertRegex(checkout, r'(?m)^          fetch-depth: 0$')
            self.assertRegex(checkout, r'(?m)^          persist-credentials: false$')
        guard = next(s for s in steps if s.startswith('name: Reject unsupported selected consumer tooling before checkout\n'))
        self.assertEqual(steps.index(guard), steps.index(trusted) + 1)
        self.assertEqual(steps.index(selected), steps.index(guard) + 1)
        self.assertIn('OKILUM_SELECTED_SOURCE: ${{ needs.select.outputs.source }}', guard)
        self.assertIn('run: python3 scripts/arch/select-source.py "$OKILUM_SELECTED_SOURCE"', guard)
        self.assertNotRegex(guard, r'(?m)^        if:')
        resolve = next(s for s in steps if s.startswith('name: Resolve exact required-CI production bytes\n'))
        self.assertLess(steps.index(selected), steps.index(resolve))
        self.assertIn('OKILUM_SELECTED_SOURCE: ${{ needs.select.outputs.source }}', resolve)
        self.assertIn("if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository", resolve)
        self.assertEqual([line.strip() for line in resolve.split('        run: |\n', 1)[1].splitlines()], [
            'export PR_HEAD_SHA="$(git rev-parse HEAD)"',
            'test "$PR_HEAD_SHA" = "$OKILUM_SELECTED_SOURCE"',
            'python3 scripts/ci/github-linux.py resolve',
        ])
        self.assertIn("if: needs.select.outputs.build == 'true'", header)
        for step in (next(s for s in steps if '          name: arch-publication\n' in s),
                     next(s for s in steps if s.startswith('name: Dispatch protected publication\n'))):
            self.assertRegex(step, r"(?m)^        if: github.ref == 'refs/heads/main' && github.event_name != 'pull_request'$")
        self.assertIn('python3 scripts/releases/coalesce.py linux', workflow)
        self.assertIn("if: needs.select.outputs.build == 'true'", workflow)
        self.assertIn('cancel-in-progress: false', workflow)
        self.assertIn('python3 scripts/releases/publication.py dispatch linux', workflow)


class ExplicitSource(unittest.TestCase):
    """#1040: an older main commit, built on a manual run of main only."""
    SHA = 'b' * 40

    def test_no_request_keeps_the_ordinary_selection(self):
        self.assertIsNone(module.explicit_source('', 'linux', 'refs/heads/main', 'workflow_dispatch'))
        self.assertIsNone(module.explicit_source(None, 'windows', 'refs/heads/main', 'schedule'))

    @patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=0))
    def test_a_main_commit_on_a_manual_main_run_is_accepted(self, run):
        for platform in ['linux', 'windows']:
            self.assertEqual(module.explicit_source(f' {self.SHA} ', platform, 'refs/heads/main',
                                                    'workflow_dispatch'), self.SHA)
        self.assertEqual(run.call_args.args[0][:3], ['git', 'merge-base', '--is-ancestor'])

    @patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1))
    def test_everything_else_is_refused(self, run):
        cases = [
            (self.SHA, 'macos', 'refs/heads/main', 'workflow_dispatch', 'macOS'),
            (self.SHA, 'linux', 'refs/heads/main', 'schedule', 'manual run'),
            (self.SHA, 'linux', 'refs/heads/feature', 'workflow_dispatch', 'manual run'),
            ('abc123', 'linux', 'refs/heads/main', 'workflow_dispatch', '40-character'),
            (self.SHA, 'linux', 'refs/heads/main', 'workflow_dispatch', 'not a commit on main'),
        ]
        for value, platform, ref, event, message in cases:
            with self.assertRaisesRegex(ValueError, message):
                module.explicit_source(value, platform, ref, event)




class ResignTests(unittest.TestCase):
    """#1104: re-sign an existing Windows build of a commit without compiling."""

    def test_only_the_published_build_of_that_commit_qualifies(self):
        sha = 'a' * 40
        published = {'source': sha, 'platform': 'windows', 'build': 11141}
        with patch.object(module, 'descriptor', return_value=published):
            module.check_resign('11141', sha, 'windows')
            with self.assertRaisesRegex(ValueError, 'not the published'):
                module.check_resign('11000', sha, 'windows')
        with patch.object(module, 'descriptor', return_value=None):
            with self.assertRaisesRegex(ValueError, 'not the published'):
                module.check_resign('11141', sha, 'windows')
        for resign, requested, platform in [('11141', None, 'windows'), ('x', sha, 'windows'),
                                            ('11141', sha, 'linux')]:
            with self.assertRaisesRegex(ValueError, 'explicit source'):
                module.check_resign(resign, requested, platform)
