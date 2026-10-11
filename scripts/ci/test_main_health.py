import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('health', Path(__file__).with_name('main-health.py'))
health = importlib.util.module_from_spec(spec)
spec.loader.exec_module(health)


class FakeApi:
    """Statuses per commit and a record of writes."""

    def __init__(self, green, authors):
        self.green_commits = set(green)
        self.authors = authors
        self.comments = {}
        self.writes = []

    def green(self, sha):
        return sha in self.green_commits

    def call(self, method, path, body=None):
        if method == 'GET' and path.startswith('/pulls/'):
            return {'user': {'login': self.authors[int(path.split('/')[2])]}}
        if method == 'GET' and path.endswith('/comments?limit=50'):
            return self.comments.get(int(path.split('/')[2]), [])
        self.writes.append((method, path, body))
        if method == 'POST' and path.endswith('/comments'):
            self.comments.setdefault(int(path.split('/')[2]), []).append(body)
        return {}

    def open_issue(self):
        return None


class MainHealthTests(unittest.TestCase):
    def test_lane_success_is_green_and_cancelled_check_is_not(self):
        newest_first = [
            {'context': 'ci / check (push)', 'status': 'failure'},
            {'context': 'ci / linux-local (push)', 'status': 'success'},
            {'context': 'ci / linux-local (push)', 'status': 'pending'},
        ]
        self.assertTrue(health.lane_passed(newest_first))
        cancelled = [{'context': 'ci / linux-local (push)', 'status': 'failure'},
                     {'context': 'ci / check (push)', 'status': 'pending'}]
        self.assertFalse(health.lane_passed(cancelled))
        self.assertFalse(health.lane_passed([]))

    def test_pull_number_from_merge_subject(self):
        subject = "Merge pull request 'fix(x): y (#12)' (#1086) from ci/996-qa-fixes into main"
        self.assertEqual(health.pull_number(subject), 1086)
        self.assertIsNone(health.pull_number('fix: direct push'))

    def test_every_merge_since_green_is_a_suspect_once_per_episode(self):
        history = [('c3', "Merge pull request 'a' (#3) from x into main"),
                   ('c2', 'chore: direct push'),
                   ('c1', "Merge pull request 'b' (#1) from y into main"),
                   ('c0', "Merge pull request 'c' (#9) from z into main")]
        original = health.first_parents, health.tip
        health.first_parents = lambda sha, limit: history
        health.tip = lambda: 'c3'
        try:
            api = FakeApi(green={'c0'}, authors={3: 'ana', 1: 'bo'})
            health.red(api, 'c3', 'run/1')
            health.red(api, 'c3', 'run/2')
        finally:
            health.first_parents, health.tip = original
        pr_comments = [w for w in api.writes if w[1] in ('/issues/3/comments', '/issues/1/comments')]
        self.assertEqual(len(pr_comments), 2, 'one comment per suspect PR per red episode')
        self.assertIn('@ana', api.comments[3][0]['body'])
        self.assertNotIn(9, api.comments, 'the last green merge is not a suspect')
        issue = next(w for w in api.writes if w[1] == '/issues')
        self.assertIn('c2 chore: direct push (direct push)', issue[2]['body'])
        self.assertIn('20 minutes', issue[2]['body'])

    def test_older_green_does_not_close_a_newer_red(self):
        api = FakeApi(green=set(), authors={})
        issue = {'number': 7, 'body': 'The Linux gate failed on main at c5c5c5c5: run'}
        api.open_issue = lambda: issue
        original = health.is_ancestor
        health.is_ancestor = lambda old, new: (old, new) == ('c5c5c5c5', 'c6')
        try:
            health.green(api, 'c4', 'run/older')
            self.assertEqual(api.writes, [], 'c4 does not contain the red c5')
            health.green(api, 'c6', 'run/newer')
        finally:
            health.is_ancestor = original
        self.assertIn(('PATCH', '/issues/7', {'state': 'closed'}), api.writes)

    def test_red_already_fixed_by_a_newer_green_commit_is_not_reported(self):
        history = [('c6', 'm6'), ('c5', 'm5'), ('c4', 'm4')]
        saved = health.first_parents, health.tip, health.is_ancestor
        health.first_parents = lambda sha, limit: history[[c for c, _ in history].index(sha):]
        health.tip = lambda: 'c6'
        health.is_ancestor = lambda old, new: True
        try:
            api = FakeApi(green={'c6'}, authors={})
            health.red(api, 'c5', 'run/late')
            self.assertEqual(api.writes, [], 'c6 contains c5 and is green')
            api = FakeApi(green={'c4'}, authors={})
            health.red(api, 'c5', 'run/real')
            self.assertTrue(any(w[1] == '/issues' for w in api.writes), 'a live red is reported')
        finally:
            health.first_parents, health.tip, health.is_ancestor = saved

    def test_failing_tests_are_named_and_infrastructure_blames_no_merge(self):
        cargo = ('2026-10-10T23:00:00Z    Compiling okilum v0.1.0\n'
                 '2026-10-10T23:02:51Z ---- platform::clip::tests::pipe stdout ----\n'
                 '2026-10-10T23:02:51Z test result: FAILED. 817 passed; 1 failed\n')
        self.assertEqual(health.classify(cargo), ('code', ['platform::clip::tests::pipe']))
        self.assertEqual(health.classify('2026Z error[E0425]: cannot find value\n')[0], 'code')
        self.assertEqual(health.classify('2026Z Error response from daemon: no such image\n'),
                         ('infra', 'Error response from daemon'))
        self.assertEqual(health.classify('2026Z Run Main checkout\n2026Z Job failed\n')[0], 'infra')
        self.assertEqual(health.classify('2026Z    Compiling x\n2026Z Job failed\n')[0], 'unknown')
        history = [('c3c3c3c3', "Merge pull request 'a' (#3) from x into main"), ('c0c0c0c0', 'm0')]
        saved = health.first_parents, health.tip, health.lane_log
        health.first_parents = lambda sha, limit: history
        health.tip = lambda: 'c3c3c3c3'
        try:
            health.lane_log = lambda api, run_id: '2026Z Job failed\n'
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/1', run_id=7)
            self.assertEqual([w[1] for w in api.writes], ['/issues'])
            self.assertIn('looks like infrastructure', api.writes[0][2]['body'])
            # One failing test: the first red waits for confirmation and wakes nobody.
            health.lane_log = lambda api, run_id: cargo
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/2', run_id=8)
            self.assertEqual([w[1] for w in api.writes], ['/issues'])
            body = api.writes[0][2]['body']
            self.assertIn('`platform::clip::tests::pipe`', body)
            self.assertIn(health.AWAITING, body)
            # The next main commit is red too: now the merges are blamed.
            api.open_issue = lambda: {'number': 5, 'body': body}
            health.red(api, 'c3c3c3c3', 'run/3', run_id=9)
            self.assertIn('/issues/3/comments', [w[1] for w in api.writes])
            # A compile error is deterministic and blames at once.
            health.lane_log = lambda api, run_id: '2026Z    Compiling x\n2026Z error[E0308]: mismatched types\n'
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/4', run_id=10)
            self.assertIn('/issues/3/comments', [w[1] for w in api.writes])
        finally:
            health.first_parents, health.tip, health.lane_log = saved


if __name__ == '__main__':
    unittest.main()
