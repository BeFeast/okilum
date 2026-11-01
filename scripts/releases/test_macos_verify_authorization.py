import json
import unittest
from unittest.mock import patch

import macos_verify_authorization as auth

SOURCE = 'a' * 40


class VerifyAuthorization(unittest.TestCase):
    def env(self):
        return dict(VERIFY_ONLY='true', VERIFY_SOURCE=SOURCE, VERIFY_PR='2218',
                    GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF='refs/heads/main',
                    GITHUB_REPOSITORY=auth.REPO, GITHUB_RUN_ATTEMPT='1')

    def api(self, path):
        if path.startswith('/pulls/'):
            return dict(state='open', draft=True, merged=False,
                        base=dict(ref='main', repo=dict(full_name=auth.REPO)),
                        head=dict(sha=SOURCE, repo=dict(full_name=auth.REPO)))
        return dict(sha=SOURCE, statuses=[dict(context='fast / check (pull_request)', status='success')])

    def test_only_explicit_authorized_current_green_internal_head_even_when_draft(self):
        with patch.object(auth.Path, 'read_text', return_value=json.dumps({'2218': SOURCE})):
            self.assertEqual(auth.authorize(self.env(), self.api), SOURCE)
            for changes in [dict(VERIFY_ONLY='false'), dict(VERIFY_SOURCE=''),
                            dict(VERIFY_SOURCE='b' * 40), dict(VERIFY_PR='9'),
                            dict(GITHUB_EVENT_NAME='push'), dict(GITHUB_RUN_ATTEMPT='2'),
                            dict(GITHUB_REF='refs/heads/pr')]:
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    auth.authorize(self.env() | changes, self.api)

    def test_stale_fork_or_unverified_head_cannot_reach_signer(self):
        with patch.object(auth.Path, 'read_text', return_value=json.dumps({'2218': SOURCE})):
            for failure in ['stale', 'fork', 'red']:
                def api(path):
                    result = self.api(path)
                    if path.startswith('/pulls/'):
                        if failure == 'stale': result['head']['sha'] = 'b' * 40
                        if failure == 'fork': result['head']['repo']['full_name'] = 'fork/repo'
                    elif failure == 'red': result['statuses'][0]['status'] = 'pending'
                    return result
                with self.subTest(failure=failure), self.assertRaises(ValueError):
                    auth.authorize(self.env(), api)


if __name__ == '__main__':
    unittest.main()
