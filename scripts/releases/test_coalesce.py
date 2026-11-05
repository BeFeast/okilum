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
        for platform in ['windows', 'macos', 'linux']:
            self.assertTrue(module.needed('schedule', None, 'new', platform))
            self.assertFalse(module.needed('schedule', {'source': 'new', 'platform': platform}, 'new', platform))

    def test_push_builds_unpublished_source_and_manual_build_bypasses_window(self):
        self.assertTrue(module.needed('push', None, 'new', 'macos'))
        self.assertFalse(module.needed('push', {'source': 'new', 'platform': 'macos'}, 'new', 'macos'))
        for event in ['workflow_dispatch', 'pull_request']:
            self.assertTrue(module.needed(event, {'source': 'new', 'platform': 'macos'}, 'new', 'macos'))

    def test_unknown_event_and_mismatched_descriptor_fail_closed(self):
        with self.assertRaises(ValueError):
            module.needed('unknown', None, 'new', 'macos')
        for item in [{'source': 'old', 'platform': 'macos'}, {'source': 'new', 'platform': 'linux'}]:
            with self.assertRaises(ValueError):
                module.needed('schedule', item, 'new', 'macos')


class LinuxWorkflow(unittest.TestCase):
    def test_linux_workflow_uses_light_selector_and_preserves_publication(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / '.forgejo/workflows/linux-release.yml').read_text()
        self.assertNotIn('schedule:', workflow)
        self.assertIn('  push:\n    branches: [main]\n', workflow)
        self.assertIn('  workflow_dispatch:', workflow)
        self.assertIn('runs-on: bridge', workflow)
        # The release checkout builds the commit select chose; a push builds its own commit.
        self.assertIn("(inputs.source != '' || inputs.hourly_source != '') && needs.select.outputs.source", workflow)
        self.assertIn('python3 scripts/releases/coalesce.py linux', workflow)
        # Arch must honor freeze/coalescing, skip AppImage-only dispatches, and retain
        # PR-lane routing. Match the release job's entire condition so a guard
        # elsewhere in the workflow cannot hide a regression here.
        release = workflow.split('\n  release:\n', 1)[1].split('\n  appimage-reuse:\n', 1)[0]
        self.assertIn('    needs: select', release.splitlines())
        self.assertEqual(
            [line.strip() for line in release.splitlines() if line.startswith('    if:')],
            ["if: needs.select.outputs.arch != 'false' && inputs.appimage_only != 'true' && "
             "needs.select.outputs.build == 'true' && "
             "(github.event_name != 'pull_request' || "
             "github.event.pull_request.head.repo.full_name != github.repository || "
             "vars.OKILUM_PR_LANE == 'local')"],
        )
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'push' }}", workflow)
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


class MacPublicationTests(unittest.TestCase):
    def test_workflow_keeps_selection_off_mac_and_preserves_publication(self):
        workflow = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/macos-release.yml').read_text()
        self.assert_macos_publication_workflow(workflow)

    def assert_macos_publication_workflow(self, workflow):
        self.assertNotIn('schedule:', workflow)
        self.assertIn('  push:\n    branches: [main]\n', workflow)
        select, release = workflow.split('\n  release:\n', 1)
        release, hosted = release.split('\n  hosted-artifact:\n', 1)
        hosted = hosted.split('\n  shipping-status:\n', 1)[0]
        self.assertIn('    runs-on: publish\n', select)
        self.assertIn("        if: steps.select.outputs.build == 'true'\n", select)
        self.assertIn('        run: python3 scripts/releases/coalesce.py macos\n', select)
        self.assertIn('        run: python3 scripts/releases/macos-hosted.py route\n', select)
        # #2139 prohibits corporate M4 CI and forces route() to hosted even for
        # stale local lane variables. Publication must survive on the active job.
        self.assertEqual([line.split('#', 1)[0].strip() for line in release.splitlines()
                          if line.startswith('    if:')], ['if: false'])
        self.assertIn('    runs-on: macos\n', release)
        self.assertIn('    needs: select\n', hosted)
        self.assertEqual([line.strip() for line in hosted.splitlines() if line.startswith('    if:')],
                         ["if: needs.select.outputs.build == 'true' && needs.select.outputs.lane == 'hosted' && "
                          "(github.event_name == 'push' || github.event_name == 'workflow_dispatch')"])
        self.assertIn('    runs-on: bridge\n', hosted)
        self.assertNotIn('continue-on-error:', hosted)
        producer = hosted.split('      - name: Produce a source-pinned hosted shipping artifact\n', 1)[1].split(
            '\n      - uses:', 1)[0]
        self.assertNotIn('if:', producer)
        self.assertIn('          PRODUCT_SOURCE: ${{ needs.select.outputs.source }}\n', producer)
        self.assertIn('        run: python3 scripts/ci/github-macos-shipping.py\n', producer)
        self.assertIn("      - uses: https://code.forgejo.org/actions/upload-artifact@v3\n"
                      "        if: github.ref == 'refs/heads/main' && github.event_name != 'pull_request'\n"
                      "        with:\n"
                      "          name: macos-publication\n"
                      "          path: ${{ runner.temp }}/publish/*\n", hosted)
        self.assertIn("      - name: Dispatch protected hosted beta publication\n"
                      "        if: github.ref == 'refs/heads/main' && github.event_name != 'pull_request'\n"
                      "        env:\n"
                      "          FORGEJO_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n"
                      "          FORGEJO_URL: ${{ github.server_url }}\n"
                      "        run: python3 scripts/releases/publication.py dispatch macos\n", hosted)
        self.assertIn("github.event_name == 'push' && 'main' || inputs.hourly_source || github.run_id", workflow)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'push' }}", workflow)
        self.assertIn('FORGEJO_TOKEN: ${{ secrets.GITHUB_TOKEN }}', workflow)
        self.assertIn('python3 scripts/releases/publication.py dispatch macos', workflow)
        self.assertLess(workflow.index('id: select'), workflow.index('id: route'))

    def test_workflow_publication_assertions_reject_disabled_hosted_path_and_local_tick(self):
        workflow = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/macos-release.yml').read_text()
        prefix, hosted = workflow.split('\n  hosted-artifact:\n', 1)
        for label, broken in [
            ('local reenabled', workflow.replace('    if: false # Corporate', '    if: true # Corporate')),
            ('push removed', workflow.replace('  push:\n    branches: [main]\n', '', 1)),
            ('cron restored', workflow.replace('  workflow_dispatch:', "  schedule:\n    - cron: '17 * * * *'\n  workflow_dispatch:", 1)),
            ('selector on Mac', workflow.replace('    runs-on: publish', '    runs-on: macos', 1)),
            ('hosted disabled', prefix + '\n  hosted-artifact:\n' + hosted.replace(
                "    if: needs.select.outputs.build == 'true'", '    if: false && true', 1)),
            ('producer disabled', workflow.replace('      - name: Produce a source-pinned hosted shipping artifact\n',
                                                   '      - name: Produce a source-pinned hosted shipping artifact\n'
                                                   '        if: false\n')),
            ('upload disabled', prefix + '\n  hosted-artifact:\n' + hosted.replace(
                "        if: github.ref == 'refs/heads/main' && github.event_name != 'pull_request'", '        if: false', 1)),
            ('dispatch disabled', workflow.replace('      - name: Dispatch protected hosted beta publication\n'
                                                  "        if: github.ref == 'refs/heads/main' && github.event_name != 'pull_request'",
                                                  '      - name: Dispatch protected hosted beta publication\n'
                                                  '        if: false')),
        ]:
            with self.subTest(regression=label):
                self.assertNotEqual(workflow, broken)
                with self.assertRaises(AssertionError):
                    self.assert_macos_publication_workflow(broken)
