import copy
import unittest
from fast_evidence import executed, PLATFORMS, STEP, WORKFLOW

SHA = 'a' * 40
BRANCH = 'forgejo-fast-pr/2079'


class FastEvidence(unittest.TestCase):
    def receipt(self):
        run = dict(id=1, head_sha=SHA, head_branch=BRANCH, path=WORKFLOW,
                   event='push', run_attempt=1, status='queued', conclusion=None)
        jobs = [dict(id=i, run_id=1, head_sha=SHA, name=name, labels=[label],
                     status='completed', conclusion='success',
                     steps=[dict(name=STEP, status='completed', conclusion='success')])
                for i, (name, label) in enumerate(PLATFORMS.items())]
        return run, jobs

    def test_three_executed_platforms_pass_even_when_summary_lags(self):
        run, jobs = self.receipt()
        self.assertTrue(executed(run, jobs, BRANCH, SHA))
        self.assertTrue(executed(run | {'status': 'completed', 'conclusion': 'success'}, jobs, BRANCH, SHA))
        self.assertFalse(executed(run | {'conclusion': 'cancelled'}, jobs, BRANCH, SHA))

    def test_each_platform_and_its_execution_identity_is_mandatory(self):
        for key, value in [('run_id', 2), ('head_sha', 'b' * 40), ('labels', []),
                           ('status', 'queued'), ('conclusion', 'skipped'), ('steps', [])]:
            for index in range(3):
                run, jobs = self.receipt(); jobs[index][key] = value
                self.assertFalse(executed(run, jobs, BRANCH, SHA), (key, index))
        for conclusion in ['pending', 'skipped', 'failure', None]:
            run, jobs = self.receipt(); jobs[1]['steps'][0]['conclusion'] = conclusion
            self.assertFalse(executed(run, jobs, BRANCH, SHA))
        run, jobs = self.receipt()
        self.assertFalse(executed(run, jobs[:2], BRANCH, SHA))
        self.assertFalse(executed(run, jobs + [jobs[0]], BRANCH, SHA))

    def test_wrong_source_ref_event_workflow_or_attempt_never_pass(self):
        run, jobs = self.receipt()
        for key, value in [('head_sha', 'b' * 40), ('head_branch', 'main'),
                           ('path', 'wrong.yml'), ('event', 'workflow_dispatch'), ('run_attempt', 2)]:
            with self.assertRaises(ValueError):
                executed(run | {key: value}, jobs, BRANCH, SHA)


if __name__ == '__main__':
    unittest.main()
