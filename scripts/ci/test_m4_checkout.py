"""Hosted-only integration coverage for the inline M4 source checkouts (#1557)."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
JOBS = [('ci.yml', 'native-scope', False), ('ci.yml', 'macos-local', False),
        ('macos-release.yml', 'select', True), ('macos-release.yml', 'release', False),
        ('macos-release.yml', 'hosted-artifact', True),
        ('release-publish.yml', 'publish', True)]


def job_text(workflow, job):
    text = (ROOT / '.forgejo/workflows' / workflow).read_text()
    return re.search(r'^  ' + re.escape(job) + r':\n(.*?)(?=^  [\w-]+:|\Z)',
                     text, re.M | re.S).group(1)


def checkout_script(block):
    step = block.split('      - name: Check out the exact source from git.oklabs.uk\n', 1)[1]
    step = step.split('      - name:', 1)[0]
    return '\n'.join(line[10:] for line in step.split('        run: |\n', 1)[1].splitlines())


class M4Checkout(unittest.TestCase):
    def git(self, cwd, *args):
        return subprocess.check_output(['git', '-C', str(cwd), *args],
                                       text=True, stderr=subprocess.PIPE).strip()

    def test_actual_inline_checkouts_keep_identity_and_full_history(self):
        # A distinct old commit, PR head and merge commit are positive controls:
        # checking out the default branch or event merge must not pass as PR head.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'
            source.mkdir()
            self.git(source, 'init', '-q', '-b', 'main')
            self.git(source, 'config', 'user.name', 'Hosted checkout regression')
            self.git(source, 'config', 'user.email', 'checkout@example.invalid')
            (source / 'identity').write_text('base')
            self.git(source, 'add', '.')
            self.git(source, 'commit', '-qm', 'base')
            base = self.git(source, 'rev-parse', 'HEAD')
            self.git(source, 'checkout', '-qb', 'pr-head')
            (source / 'identity').write_text('head')
            self.git(source, 'commit', '-qam', 'head')
            head = self.git(source, 'rev-parse', 'HEAD')
            self.git(source, 'checkout', '-q', 'main')
            self.git(source, 'merge', '--no-ff', '-qm', 'merge', 'pr-head')
            merge = self.git(source, 'rev-parse', 'HEAD')
            self.assertEqual(len({base, head, merge}), 3)
            for index, (workflow, job, full) in enumerate(JOBS):
                with self.subTest(workflow=workflow, job=job):
                    block = job_text(workflow, job)
                    script = checkout_script(block)
                    expected = head if job == 'macos-local' else merge
                    expression = ('${{ github.event.pull_request.head.sha }}'
                                  if job == 'macos-local' else '${{ github.sha }}')
                    self.assertIn('CHECKOUT_SHA: ' + expression, block)
                    workspace = root / str(index)
                    workspace.mkdir()
                    # Reuse a shallow, stale checkout, as a persistent Mac runner can.
                    self.git(workspace, 'init', '-q')
                    self.git(workspace, 'fetch', '-q', '--depth=1', source.as_uri(), base)
                    self.git(workspace, 'checkout', '-q', '--detach', 'FETCH_HEAD')
                    (workspace / 'stale-output').write_text('must be removed')
                    env = {**os.environ, 'CHECKOUT_SHA': expected,
                           'CHECKOUT_REPOSITORY': 'fixture/repository',
                           # Process-only URL substitution: never a stored credential/config.
                           'GIT_CONFIG_COUNT': '1',
                           'GIT_CONFIG_KEY_0': 'url.' + source.as_uri() + '.insteadOf',
                           'GIT_CONFIG_VALUE_0': 'https://git.oklabs.uk/fixture/repository.git'}
                    subprocess.run(['bash', '-c', script], cwd=workspace, env=env,
                                   check=True, capture_output=True)
                    self.assertEqual(self.git(workspace, 'rev-parse', 'HEAD'), expected)
                    self.assertFalse((workspace / 'stale-output').exists())
                    self.assertEqual(self.git(workspace, 'status', '--porcelain'), '')
                    self.assertEqual(self.git(workspace, 'remote', 'get-url', 'origin'),
                                     'https://git.oklabs.uk/fixture/repository.git')
                    config = (workspace / '.git/config').read_text().lower()
                    for forbidden in ['extraheader', 'credential', 'insteadof', 'token']:
                        self.assertNotIn(forbidden, config)
                    if full:
                        self.assertEqual(self.git(workspace, 'rev-parse', '--is-shallow-repository'), 'false')
                        self.git(workspace, 'merge-base', '--is-ancestor', base, 'origin/main')
                    # Missing or malformed source must fail instead of using stale HEAD.
                    for missing in ['0' * 40, 'main', '']:
                        result = subprocess.run(['bash', '-c', script], cwd=workspace,
                                                env={**env, 'CHECKOUT_SHA': missing}, capture_output=True)
                        self.assertNotEqual(result.returncode, 0)

    def test_scoped_jobs_have_no_external_action_fetches(self):
        pin = 'https://git.oklabs.uk/BeFeast/actions-upload-artifact@ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5'
        for workflow, job, _ in JOBS:
            with self.subTest(workflow=workflow, job=job):
                block = job_text(workflow, job)
                self.assertNotIn('code.forgejo.org', block)
                uses = re.findall(r'uses: (\S+)', block)
                self.assertEqual(uses, [pin] if job in ['macos-local', 'release'] else [])


if __name__ == '__main__':
    unittest.main()
