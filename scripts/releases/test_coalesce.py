import importlib.util
from pathlib import Path
import unittest

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
