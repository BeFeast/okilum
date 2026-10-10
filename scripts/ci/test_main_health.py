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
        original = health.first_parents
        health.first_parents = lambda sha, limit: history
        try:
            api = FakeApi(green={'c0'}, authors={3: 'ana', 1: 'bo'})
            health.red(api, 'c3', 'run/1')
            health.red(api, 'c3', 'run/2')
        finally:
            health.first_parents = original
        pr_comments = [w for w in api.writes if w[1] in ('/issues/3/comments', '/issues/1/comments')]
        self.assertEqual(len(pr_comments), 2, 'one comment per suspect PR per red episode')
        self.assertIn('@ana', api.comments[3][0]['body'])
        self.assertNotIn(9, api.comments, 'the last green merge is not a suspect')
        issue = next(w for w in api.writes if w[1] == '/issues')
        self.assertIn('c2 chore: direct push (direct push)', issue[2]['body'])
        self.assertIn('20 minutes', issue[2]['body'])


if __name__ == '__main__':
    unittest.main()
