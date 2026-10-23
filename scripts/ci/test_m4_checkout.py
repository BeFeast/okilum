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
    step = block.split('      - name: Check out the exact source from the runner server\n', 1)[1]
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
                           'CHECKOUT_REPOSITORY': 'DifferentOwner/source',
                           'CHECKOUT_SERVER_URL': 'http://127.0.0.1:3000/',
                           # Process-only URL substitution: never a stored credential/config.
                           'GIT_CONFIG_COUNT': '2',
                           'GIT_CONFIG_KEY_0': 'url.' + source.as_uri() + '.insteadOf',
                           'GIT_CONFIG_VALUE_0': 'http://127.0.0.1:3000/DifferentOwner/source.git',
                           'GIT_CONFIG_KEY_1': 'http.http://127.0.0.1:3000/.extraHeader',
                           'GIT_CONFIG_VALUE_1': 'Authorization: fixture-only'}
                    subprocess.run(['bash', '-c', script], cwd=workspace, env=env,
                                   check=True, capture_output=True)
                    self.assertEqual(self.git(workspace, 'rev-parse', 'HEAD'), expected)
                    self.assertFalse((workspace / 'stale-output').exists())
                    self.assertEqual(self.git(workspace, 'status', '--porcelain'), '')
                    self.assertEqual(self.git(workspace, 'remote', 'get-url', 'origin'),
                                     'http://127.0.0.1:3000/DifferentOwner/source.git')
                    # Existing runner auth matches only this origin. It is inherited,
                    # never copied to the repository or an unrelated server namespace.
                    for url, matches in [('http://127.0.0.1:3000/DifferentOwner/source.git', True),
                                         ('http://127.0.0.1:3000/BeFeast/actions-upload-artifact.git', True),
                                         ('https://git.oklabs.uk/DifferentOwner/source.git', False),
                                         ('http://127.0.0.1:3001/DifferentOwner/source.git', False)]:
                        auth = subprocess.run(['git', 'config', '--get-urlmatch', 'http.extraHeader', url],
                                              cwd=workspace, env=env, text=True, capture_output=True)
                        self.assertEqual(auth.returncode == 0, matches)
                        self.assertEqual(auth.stdout.strip(), 'Authorization: fixture-only' if matches else '')
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
                    injected = subprocess.run(['bash', '-c', script], cwd=workspace,
                                              env={**env, 'CHECKOUT_REPOSITORY': '$(touch context-injected)'},
                                              capture_output=True)
                    self.assertNotEqual(injected.returncode, 0)
                    self.assertFalse((workspace / 'context-injected').exists())

    def test_pinned_action_bootstrap_uses_runner_origin_and_replaces_stale_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'mirror'
            source.mkdir()
            self.git(source, 'init', '-q')
            self.git(source, 'config', 'user.name', 'Hosted action regression')
            self.git(source, 'config', 'user.email', 'action@example.invalid')
            (source / 'action.yml').write_text("runs: {using: node16, main: dist/index.js}\n")
            (source / 'dist').mkdir()
            (source / 'dist/index.js').write_text('// fixture; never execute\n')
            self.git(source, 'add', '.')
            self.git(source, 'commit', '-qm', 'pinned fixture')
            pin = self.git(source, 'rev-parse', 'HEAD')
            (source / 'dist/index.js').write_text('// moving branch; wrong code\n')
            self.git(source, 'commit', '-qam', 'moving tip')
            for index, (workflow, job) in enumerate([('ci.yml', 'macos-local'),
                                                    ('macos-release.yml', 'release')]):
                with self.subTest(workflow=workflow):
                    block = job_text(workflow, job)
                    step = block.split('      - name: Prepare the pinned upload action from the runner server\n', 1)[1]
                    step = re.split(r'^      - ', step, maxsplit=1, flags=re.M)[0]
                    script = '\n'.join(line[10:] for line in step.split('        run: |\n', 1)[1].splitlines())
                    workspace = root / str(index)
                    action = workspace / '.forgejo-local-actions/upload-artifact'
                    action.mkdir(parents=True)
                    (action / 'stale-code').write_text('must disappear')
                    env = {**os.environ, 'ACTION_SHA': pin, 'ACTION_SERVER_URL': 'http://127.0.0.1:3000/',
                           'GIT_CONFIG_COUNT': '1',
                           'GIT_CONFIG_KEY_0': 'url.' + source.as_uri() + '.insteadOf',
                           'GIT_CONFIG_VALUE_0': 'http://127.0.0.1:3000/BeFeast/actions-upload-artifact.git'}
                    subprocess.run(['bash', '-c', script], cwd=workspace, env=env,
                                   check=True, capture_output=True)
                    self.assertEqual(self.git(action, 'rev-parse', 'HEAD'), pin)
                    self.assertFalse((action / 'stale-code').exists())
                    self.assertEqual((action / 'dist/index.js').read_text(), '// fixture; never execute\n')
                    config = (action / '.git/config').read_text().lower()
                    for forbidden in ['extraheader', 'credential', 'insteadof', 'token']:
                        self.assertNotIn(forbidden, config)
                    # A missing pin must not execute or retain the moving branch.
                    result = subprocess.run(['bash', '-c', script], cwd=workspace,
                                            env={**env, 'ACTION_SHA': '0' * 40}, capture_output=True)
                    self.assertNotEqual(result.returncode, 0)

    def test_scoped_jobs_have_no_external_action_fetches(self):
        local = './.forgejo-local-actions/upload-artifact'
        for workflow, job, _ in JOBS:
            with self.subTest(workflow=workflow, job=job):
                block = job_text(workflow, job)
                self.assertNotIn('code.forgejo.org', block)
                self.assertNotIn('git.oklabs.uk', block)
                self.assertIn('CHECKOUT_SERVER_URL: ${{ github.server_url }}', block)
                self.assertIn('CHECKOUT_REPOSITORY: ${{ github.repository }}', block)
                # Context is data in env, never interpolated into a shell program.
                for run in re.findall(r'        run: \|\n((?:          .*\n|\n)+)', block):
                    self.assertNotIn('${{', run)
                uses = re.findall(r'uses: (\S+)', block)
                self.assertEqual(uses, [local] if job in ['macos-local', 'release'] else [])
                if uses:
                    self.assertIn('ACTION_SERVER_URL: ${{ github.server_url }}', block)
                    self.assertIn('ACTION_SHA: ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5', block)
                    prepare = block.index('      - name: Prepare the pinned upload action')
                    self.assertLess(prepare, block.index('uses: ' + local))
                    self.assertIn('if: always()' if job == 'macos-local'
                                  else "if: github.ref == 'refs/heads/main'", block[prepare:])


if __name__ == '__main__':
    unittest.main()
