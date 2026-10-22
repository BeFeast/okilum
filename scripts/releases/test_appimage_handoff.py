"""Independent handoff regressions; run by the hosted Linux gate."""
import copy
import unittest
from unittest.mock import Mock, patch

import appimage_handoff as handoff
from test_appimage_publication import evidence, SOURCE


class IndependentAcceptance(unittest.TestCase):
    def test_appimage_smoke_survives_failed_linux_aggregate(self):
        receipt, _, _, _ = evidence()
        receipt['run']['conclusion'] = 'failure'
        receipt['jobs'][1]['conclusion'] = 'failure'
        handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
        for changes in [{'head_branch': 'forgejo-linux-pr/1'}, {'run_attempt': 2},
                        {'repository': {'full_name': 'other/repo'}}]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                handoff.accepted({**receipt['run'], **changes}, receipt['jobs'], SOURCE)
        jobs = copy.deepcopy(receipt['jobs'])
        jobs[0]['steps'] = []
        with self.assertRaises(ValueError):
            handoff.accepted(receipt['run'], jobs, SOURCE)

    def test_trusted_origin_and_unique_artifact_before_key_import(self):
        run = {'id': 7, 'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'workflow_dispatch',
               'commit_sha': SOURCE, 'index_in_repo': 123, 'status': 'failure'}
        handoff.trusted(run, 7)
        for changes in [{'workflow_id': 'other.yml'}, {'prettyref': 'feature'},
                        {'is_fork_pull_request': True}, {'trigger_event': 'pull_request'}]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                handoff.trusted({**run, **changes}, 7)
        item = {'id': 9, 'name': 'appimage-publication', 'expired': False, 'run_id': 7}
        with patch.object(handoff.publisher.arch, 'SigningKey') as key:
            for items in [[], [item, item], [{**item, 'expired': True}]]:
                client = Mock()
                client.call.side_effect = [run, items]
                with self.assertRaises(ValueError):
                    handoff.publish(client, 7)
            key.assert_not_called()
