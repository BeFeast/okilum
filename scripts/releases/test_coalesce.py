import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location('coalesce', Path(__file__).with_name('coalesce.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CoalescingTests(unittest.TestCase):
    def test_hourly_tick_builds_only_unpublished_source(self):
        for platform in ['windows', 'macos']:
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
    def test_windows_schedule_skips_old_main_and_keeps_current(self, run):
        head = 'a' * 40
        run.return_value = SimpleNamespace(stdout=head + '\trefs/heads/main\n')
        self.assertFalse(module.current_schedule('schedule', 'b' * 40, 'windows'))
        self.assertTrue(module.current_schedule('schedule', head, 'windows'))
        self.assertEqual(run.call_args.kwargs['timeout'], 15)

    @patch.object(module.subprocess, 'run')
    def test_manual_pr_and_other_platforms_never_query_main(self, run):
        for event, platform in [('workflow_dispatch', 'windows'),
                                ('pull_request', 'windows'), ('push', 'windows'),
                                ('schedule', 'macos')]:
            self.assertTrue(module.current_schedule(event, 'old', platform))
        run.assert_not_called()

    @patch.object(module.subprocess, 'run')
    def test_unknown_remote_result_is_failure_not_successful_skip(self, run):
        for output in ['', 'garbage', 'a' * 40 + '\trefs/heads/other']:
            run.return_value = SimpleNamespace(stdout=output)
            with self.assertRaises(ValueError):
                module.current_schedule('schedule', 'old', 'windows')
        run.side_effect = module.subprocess.TimeoutExpired('git', 15)
        with self.assertRaises(module.subprocess.TimeoutExpired):
            module.current_schedule('schedule', 'old', 'windows')
