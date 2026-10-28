"""Real Git D/F conflicts and cleanup ownership across bridge generations."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bridge = load('collision_transport', 'github-macos.py')
pr = load('collision_pr', 'github-pr.py')
linux = load('collision_linux', 'github-linux.py')
SHA = 'a' * 40


class RefCollisionTests(unittest.TestCase):
    def test_real_git_legacy_leaf_blocks_old_child_but_not_new_sibling(self):
        with tempfile.TemporaryDirectory() as tmp:
            def git(*args, **kwargs):
                return subprocess.run(['git', '-C', tmp, *args], capture_output=True,
                                      text=True, **kwargs)
            git('init', '--bare', check=True)
            tree = git('mktree', input='', check=True).stdout.strip()
            env = dict(os.environ, GIT_AUTHOR_NAME='test', GIT_AUTHOR_EMAIL='test@example.org',
                       GIT_COMMITTER_NAME='test', GIT_COMMITTER_EMAIL='test@example.org')
            sha = git('commit-tree', tree, input='fixture\n', env=env, check=True).stdout.strip()
            for namespace in bridge.BRIDGE_PREFIXES:
                legacy = f'refs/heads/{namespace}/1698'
                old_child = f'{legacy}/{sha}-20779-1'
                new = 'refs/heads/' + bridge.ref_name('1698', sha, '20779', '1', '1' * 32)
                new = new.replace('forgejo-pr/', namespace + '/', 1)
                git('update-ref', legacy, sha, check=True)
                failed = git('update-ref', old_child, sha)
                self.assertNotEqual(failed.returncode, 0, namespace)
                self.assertIn('exists; cannot create', failed.stderr)
                git('update-ref', new, sha, check=True)
                self.assertEqual(git('rev-parse', new, check=True).stdout.strip(), sha)
                # Reverse conflict also has a positive control: an old child blocks
                # its parent, while another new sibling still works.
                git('update-ref', '-d', legacy, check=True)
                git('update-ref', old_child, sha, check=True)
                self.assertNotEqual(git('update-ref', legacy, sha).returncode, 0)
                another = new.replace('1' * 32, '2' * 32)
                git('update-ref', another, sha, check=True)

    def test_close_cleanup_owns_only_this_pr_all_lanes_old_and_new(self):
        expected = set()
        foreign = {'refs/heads/main', 'refs/tags/beta'}
        for namespace in bridge.BRIDGE_PREFIXES:
            root = f'refs/heads/{namespace}/1698'
            expected.update([root, f'{root}/{SHA}-20779-1', f'{root}-{SHA}-20779-1-' + '1' * 32])
            foreign.update([root + '0', root + '0/' + SHA + '-20779-1', root + '-manual'])
        class API:
            def __init__(self):
                self.deleted = []
            def request(self, path, method='GET'):
                if method == 'GET':
                    return [{'ref': ref} for ref in expected | foreign]
                self.deleted.append(path.removeprefix('git/'))
        api = API()
        bridge.cleanup(api, '1698')
        self.assertEqual(set(api.deleted), expected)
        self.assertEqual(len(api.deleted), len(expected))

    def test_failed_push_never_deletes_a_ref_that_was_not_created(self):
        for module in [pr, linux]:
            with self.subTest(module=module.__name__), tempfile.TemporaryDirectory() as tmp:
                env = dict(CI_LANE='windows-release', PR_HEAD_SHA=SHA, PR_NUMBER='1698',
                           GITHUB_RUN_ID='20779', GITHUB_OUTPUT=str(Path(tmp) / 'output'))
                with patch.dict(os.environ, env), patch.object(module.transport, 'GitHub') as api, \
                        patch.object(module.transport, 'push_head', side_effect=module.transport.Unavailable('D/F conflict')), \
                        patch.object(module.transport, 'wait_for_run') as wait:
                    self.assertEqual(module.main(), 1)
                    api.return_value.request.assert_not_called()
                    wait.assert_not_called()
                self.assertEqual(Path(env['GITHUB_OUTPUT']).read_text(), 'result=failure\n')
