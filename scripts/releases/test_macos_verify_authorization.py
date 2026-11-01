import json
import hashlib
from datetime import datetime, timedelta, timezone
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

    def test_hosted_requires_app_attested_exact_fresh_mirror_without_forgejo(self):
        evidence = dict(schema=1, repository=auth.REPO, source=SOURCE, pr='2218',
                        producer='700', attempt='1', tooling='b' * 40,
                        ref=auth.mirror_ref('2218', SOURCE, '700'), fast='success',
                        issued=datetime.now(timezone.utc).isoformat())
        environment = self.env() | dict(HOSTED_VERIFY='true', PRODUCER='700', GITHUB_SHA='b' * 40,
            GITHUB_ACTOR=auth.BRIDGE_ACTOR[0], GITHUB_ACTOR_ID=str(auth.BRIDGE_ACTOR[1]),
            VERIFY_EVIDENCE=auth.evidence_bytes(evidence).decode())
        def api(path):
            self.assertFalse(path.startswith('/pulls/') or path.endswith('/status') and path.startswith('/commits/'))
            if path.startswith('git/ref/'):
                return dict(ref='refs/heads/' + evidence['ref'], object=dict(sha=SOURCE))
            return dict(sha=SOURCE, statuses=[dict(context='okilum/verify-admission/700',
                state='success', description='sha256:' + hashlib.sha256(auth.evidence_bytes(evidence)).hexdigest(),
                creator=dict(login=auth.BRIDGE_ACTOR[0], id=auth.BRIDGE_ACTOR[1]))])
        with patch.object(auth.Path, 'read_text', return_value=json.dumps({'2218': SOURCE})):
            self.assertEqual(auth.authorize(environment, api), SOURCE)
            invalid = [dict(VERIFY_EVIDENCE=''), dict(VERIFY_EVIDENCE='{}'),
                       dict(GITHUB_ACTOR_ID='1'), dict(VERIFY_SOURCE='c' * 40), dict(VERIFY_PR='9')]
            for key, value in [('source', 'c' * 40), ('producer', '701'),
                               ('tooling', 'd' * 40), ('fast', 'pending'),
                               ('issued', (datetime.now(timezone.utc) - timedelta(hours=3)).isoformat())]:
                invalid.append(dict(VERIFY_EVIDENCE=auth.evidence_bytes(evidence | {key: value}).decode()))
            for change in invalid:
                with self.subTest(change=change), self.assertRaises((ValueError, KeyError)):
                    auth.authorize(environment | change, api)
            for failure in ['digest', 'actor', 'missing', 'ref']:
                def bad(path):
                    result = api(path)
                    if path.startswith('git/ref/'):
                        if failure == 'ref': result['object']['sha'] = 'c' * 40
                    elif failure == 'missing': result['statuses'] = []
                    elif failure == 'digest': result['statuses'][0]['description'] = 'sha256:bad'
                    elif failure == 'actor': result['statuses'][0]['creator']['id'] = 1
                    return result
                with self.subTest(failure=failure), self.assertRaises(ValueError):
                    auth.authorize(environment, bad)


if __name__ == '__main__':
    unittest.main()
