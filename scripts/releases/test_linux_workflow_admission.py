"""Arch compilation cannot occupy AppImage admission; feed writes stay serialized."""
from pathlib import Path
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]


class LinuxAdmission(unittest.TestCase):
    def test_arch_cannot_block_next_main_appimage_path(self):
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/linux-release.yml').read_text())
        # Hourly coordinators cancel superseded ticks. Explicit archive/reuse
        # runs have independent groups, so Arch cannot occupy their admission.
        self.assertIn('github.run_id', workflow['concurrency']['group'])
        self.assertIn("inputs.hourly_source", workflow['concurrency']['group'])
        self.assertIs(workflow['concurrency']['cancel-in-progress'], True)
        jobs = workflow['jobs']
        self.assertNotIn('needs', jobs['select'])
        self.assertEqual(jobs['appimage-reuse']['needs'], 'select')
        self.assertEqual(set(jobs['appimage-publication']['needs']), {'select', 'appimage-reuse'})
        self.assertEqual(jobs['release']['needs'], 'select')
        self.assertEqual(set(jobs['publication']['needs']), {'select', 'release'})

    def test_publication_and_stable_still_share_the_existing_lock(self):
        workflows = ROOT / '.forgejo/workflows'
        publication = yaml.safe_load((workflows / 'release-publish.yml').read_text())
        stable = yaml.safe_load((workflows / 'releases.yml').read_text())
        self.assertTrue(stable['concurrency']['group'].startswith(
            "${{ (inputs.build != '' || inputs.sync_stable) && 'okilum-macos-release' ||"))
        self.assertEqual(publication['concurrency']['group'], 'okilum-macos-release')
        self.assertIs(publication['concurrency']['cancel-in-progress'], False)
        self.assertIs(stable['concurrency']['cancel-in-progress'], False)


if __name__ == '__main__':
    unittest.main()
