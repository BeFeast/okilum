"""A failed publication of a completed build is retried, bounded, and never repeated after success (#1559)."""
from pathlib import Path
import tempfile
import unittest

import yaml

import publication

ROOT = Path(__file__).resolve().parents[2]
DISPATCH = '/actions/workflows/release-publish.yml/dispatches'


class FakeForgejo:
    def __init__(self, issues=(), fail_posts=0):
        self.calls = []
        self.issues = list(issues)
        self.fail_posts = fail_posts

    def call(self, method, path, body=None):
        self.calls.append((method, path, body))
        if method == 'POST' and path == DISPATCH and self.fail_posts:
            self.fail_posts -= 1
            raise SystemExit(f'POST {path}: 502')
        if method == 'GET' and path.startswith('/issues?'):
            return self.issues
        return None

    def dispatches(self):
        return [body for method, path, body in self.calls if method == 'POST' and path == DISPATCH]


def release_publish_jobs():
    return yaml.load((ROOT / '.forgejo/workflows/release-publish.yml').read_text(),
                     Loader=yaml.BaseLoader)


def run_publication_chain(client, platform, run_id, outcomes):
    """Dry-run of release-publish: each dispatched run publishes, and on failure the retry job runs.

    `outcomes` is the publish job result of each successive release-publish run.
    Returns the publish results that ran and whether the chain ended red.
    """
    pending = [{'platform': platform, 'run_id': str(run_id)}]
    results = []
    ended_red = False
    while pending:
        inputs = pending.pop(0)
        result = outcomes[len(results)]
        results.append(result)
        # The workflow gate of the retry job, evaluated as Forgejo would.
        if result != 'failure':
            continue
        before = len(client.dispatches())
        try:
            publication.retry(client, inputs['platform'], int(inputs['run_id']),
                              int(inputs.get('attempt', '1')), sleep=lambda _: None)
        except SystemExit:
            ended_red = True
        pending.extend(body['inputs'] for body in client.dispatches()[before:])
    return results, ended_red


class PublicationRetry(unittest.TestCase):
    def test_failed_publication_is_redispatched_for_the_same_build(self):
        client = FakeForgejo()
        delays = []
        publication.retry(client, 'macos', 16225, 1, sleep=delays.append)
        # Same completed run, release-publish only: reuse, never a rebuild dispatch.
        self.assertEqual(client.dispatches(), [{'ref': 'main', 'inputs': {
            'platform': 'macos', 'run_id': '16225', 'attempt': '2'}}])
        self.assertTrue(all(path == DISPATCH for method, path, _ in client.calls if method == 'POST'))
        self.assertEqual(delays, [publication.RETRY_DELAY_SECONDS])

    def test_transient_failure_publishes_on_a_retry_and_stops(self):
        client = FakeForgejo()
        results, red = run_publication_chain(client, 'macos', 16225, ['failure', 'success'])
        self.assertEqual(results, ['failure', 'success'])
        self.assertFalse(red)
        self.assertEqual(len(client.dispatches()), 1)
        self.assertFalse(any(path.startswith('/issues') for _, path, _ in client.calls))

    def test_successful_publication_is_never_repeated(self):
        for result in ('success', 'cancelled', 'skipped'):
            with self.subTest(result=result):
                client = FakeForgejo()
                results, red = run_publication_chain(client, 'macos', 16225, [result])
                self.assertEqual(results, [result])
                self.assertFalse(red)
                self.assertEqual(client.calls, [])

    def test_retries_are_bounded_and_the_last_failure_is_visible(self):
        client = FakeForgejo()
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / 'summary.md'
            original = publication.retry

            def retry(*args, **kwargs):
                return original(*args, summary=summary, run_url='https://ci/run/9', **kwargs)
            publication.retry = retry
            try:
                results, red = run_publication_chain(client, 'macos', 16225,
                                                     ['failure'] * (publication.MAX_PUBLICATION_ATTEMPTS + 2))
            finally:
                publication.retry = original
            self.assertIn('failed 3 times', summary.read_text())
        self.assertEqual(results, ['failure'] * publication.MAX_PUBLICATION_ATTEMPTS)
        self.assertTrue(red)
        self.assertEqual([d['inputs'].get('attempt') for d in client.dispatches()], ['2', '3'])
        created = [body for method, path, body in client.calls if method == 'POST' and path == '/issues']
        self.assertEqual(len(created), 1)
        self.assertEqual(created[0]['title'], publication.FAILED_ISSUE_TITLE)
        self.assertIn('run 16225', created[0]['body'])
        self.assertIn('https://ci/run/9', created[0]['body'])

    def test_exhausted_retry_comments_on_the_open_issue(self):
        client = FakeForgejo(issues=[{'number': 77, 'title': publication.FAILED_ISSUE_TITLE}])
        with self.assertRaises(SystemExit):
            publication.retry(client, 'windows', 38712, publication.MAX_PUBLICATION_ATTEMPTS,
                              sleep=lambda _: None)
        self.assertEqual(client.dispatches(), [])
        self.assertEqual([path for method, path, _ in client.calls if method == 'POST'],
                         ['/issues/77/comments'])

    def test_issue_api_failure_still_ends_red(self):
        class Broken(FakeForgejo):
            def call(self, method, path, body=None):
                if path.startswith('/issues'):
                    raise SystemExit('GET /issues: 500')
                return super().call(method, path, body)
        with self.assertRaises(SystemExit) as raised:
            publication.retry(Broken(), 'macos', 1, publication.MAX_PUBLICATION_ATTEMPTS)
        self.assertIn('no further automatic retry', str(raised.exception))

    def test_dispatch_survives_a_transient_api_error_and_keeps_the_original_payload(self):
        client = FakeForgejo(fail_posts=1)
        publication.dispatch_publication(client, 'macos', 16225, sleep=lambda _: None)
        self.assertEqual(client.dispatches(), [{'ref': 'main', 'inputs': {'platform': 'macos', 'run_id': '16225'}}] * 2)
        client = FakeForgejo(fail_posts=publication.DISPATCH_ATTEMPTS)
        with self.assertRaises(SystemExit):
            publication.dispatch_publication(client, 'macos', 16225, sleep=lambda _: None)

    def test_invalid_requests_are_refused(self):
        for args in [('ios', 1, 1), ('macos', 0, 1), ('macos', 1, 0)]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                publication.retry(FakeForgejo(), *args, sleep=lambda _: None)

    def test_workflow_retries_only_a_failed_publish_job(self):
        workflow = release_publish_jobs()
        self.assertEqual(workflow['on']['workflow_dispatch']['inputs']['attempt']['default'], '1')
        job = workflow['jobs']['retry']
        self.assertEqual(job['needs'], 'publish')
        self.assertIn("needs.publish.result == 'failure'", job['if'])
        self.assertIn('always()', job['if'])
        self.assertIn("github.ref == 'refs/heads/main'", job['if'])
        self.assertNotIn('container', job)
        self.assertEqual(job['permissions']['issues'], 'write')
        self.assertEqual(job['permissions']['actions'], 'write')
        step = next(s for s in job['steps'] if 'publication.py retry' in s.get('run', ''))
        self.assertEqual(step['run'], 'python3 scripts/releases/publication.py retry "$PLATFORM" '
                                      '--run "$SOURCE_RUN" --attempt "$ATTEMPT"')
        self.assertEqual(step['env']['SOURCE_RUN'], '${{ inputs.run_id }}')
        self.assertEqual(step['env']['ATTEMPT'], "${{ inputs.attempt || '1' }}")
        # No compiler, signing or feed credentials in the retry job.
        text = yaml.safe_dump(job)
        for forbidden in ('R2_', 'GPG', 'SPARKLE', 'cargo', 'MIRROR_TOKEN'):
            self.assertNotIn(forbidden, text)
        # Retry timeout covers the longest backoff with checkout to spare.
        backoff = publication.RETRY_DELAY_SECONDS * (publication.MAX_PUBLICATION_ATTEMPTS - 1)
        self.assertGreater(int(job['timeout-minutes']) * 60, backoff + 120)


if __name__ == '__main__':
    unittest.main()
