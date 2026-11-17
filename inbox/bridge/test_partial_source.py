"""#2344: pending T3 questions survive partial or reconnecting sources.

T3 bounds turn items, not runtime requests, and keeps the rows of pending
requests in the window. Rows of answered/withdrawn requests scroll out and are
recovered from bounded `/history` pages; until then they are held.
"""
import copy
from pathlib import Path
import tempfile
import unittest

import t3_questions
from t3_questions import Journal, Runner, Unavailable, project
from test_t3_questions import CONFIG, Inbox, Source, operation, snapshot

THREADS = dict(CONFIG, thread_ids=['pilot-thread', 'other-thread'])


def row(item, visibility='local'):
    return {'visibility': visibility, 'sourceThreadId': item['threadId'],
            'sourceItemId': item['id'], 'item': item}


def scroll_out(snap, cursor='older'):
    """Model T3's bounded window: non-pending question rows leave turnItems."""
    p = snap['projection']
    pending = {r['id'] for r in p['runtimeRequests'] if r['status'] == 'pending'}
    old = [i for i in p['turnItems'] if i['requestId'] not in pending]
    p['turnItems'] = [i for i in p['turnItems'] if i['requestId'] in pending]
    snap.pop('payloadBudgetExceeded', None)
    snap.update(hasMoreHistory=True, historyCursor=cursor)
    return old


def with_ids(snap):
    for n, item in enumerate(snap['projection']['turnItems']):
        item.setdefault('id', f'row-{n}-{item["requestId"]}')
    return snap


def second_question(snap, request='request-2', status='pending'):
    p = snap['projection']
    req = copy.deepcopy(p['runtimeRequests'][0])
    req.update(id=request, status=status, nodeId='node-2')
    item = copy.deepcopy(p['turnItems'][0])
    item.update(requestId=request, nodeId='node-2', id='row-' + request)
    item['questions'][0]['question'] = 'Which size?'
    p['runtimeRequests'].append(req)
    p['turnItems'].append(item)
    return snap


class ThreadSource(Source):
    """Per-thread snapshots plus a scripted `/history` endpoint."""
    def __init__(self, threads=('pilot-thread',)):
        super().__init__()
        self.values = {}
        for thread in threads:
            snap = with_ids(snapshot())
            snap['projection']['thread']['id'] = thread
            for item in snap['projection']['turnItems']:
                item['threadId'] = thread
            self.values[thread] = snap
        self.value = self.values['pilot-thread']
        self.pages = {}
        self.history_calls = []
        self.snapshot_calls = 0
        self.after_history = None

    def snapshot(self, thread):
        self.snapshot_calls += 1
        if self.offline:
            raise Unavailable('source_offline')
        return copy.deepcopy(self.values[thread])

    def history(self, thread, cursor):
        self.history_calls.append((thread, cursor))
        page = self.pages.get((thread, cursor))
        if self.after_history:
            self.after_history()
        if isinstance(page, Exception):
            raise page
        if page is None:
            raise Unavailable('http_not_found')
        return copy.deepcopy(page)

    def publish(self, thread, items, cursor='older', sequence=None, **extra):
        page = {'items': [row(i) for i in items], 'nextCursor': None, 'hasMoreHistory': False,
                'snapshotSequence': sequence or self.values[thread]['snapshotSequence']}
        page.update(extra)
        self.pages[(thread, cursor)] = page


class ProjectionBoundaryTests(unittest.TestCase):
    def test_modern_snapshot_without_removed_budget_flag_projects(self):
        snap = snapshot(); snap.pop('payloadBudgetExceeded'); snap['historyCursor'] = None
        _, records, incomplete = project(snap, CONFIG, 'pilot-thread')
        self.assertEqual(len(records), 1); self.assertEqual(incomplete, set())

    def test_bounded_window_keeps_pending_and_holds_scrolled_out_rows(self):
        snap = with_ids(second_question(snapshot(), status='resolved'))
        old_id = next(q['id'] for q, _ in project(snap, CONFIG, 'pilot-thread')[1].values()
                      if q['source']['question_id'] == 'request-2')
        scroll_out(snap)
        _, records, incomplete = project(snap, CONFIG, 'pilot-thread')
        self.assertEqual([q['state'] for q, _ in records.values()], ['pending'])
        self.assertTrue(next(iter(records.values()))[0]['can_reply'])
        self.assertEqual(incomplete, {old_id})

    def test_complete_snapshot_missing_a_row_is_still_partial(self):
        snap = snapshot(); snap['projection']['turnItems'] = []
        with self.assertRaisesRegex(Unavailable, 'partial_source'):
            project(snap, CONFIG, 'pilot-thread')


class PartialRunnerTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name) / 'state.db'
        self.source, self.inbox = ThreadSource(THREADS['thread_ids']), Inbox()
        self.journal = Journal(self.path, THREADS)
        self.runner = Runner(THREADS, self.journal, self.source, self.inbox)
        self.assertEqual(self.runner.step(), [])

    def tearDown(self):
        self.journal.db.close()
        self.dir.cleanup()

    def restart(self):
        self.journal.db.close()
        self.journal = Journal(self.path, THREADS)
        self.runner = Runner(THREADS, self.journal, self.source, self.inbox)

    def enqueue(self, thread='pilot-thread'):
        op = operation()
        _, records, _ = project(self.source.values[thread], THREADS, thread)
        q = next(iter(records.values()))[0]
        op['question'] = q
        op['request'].update(question_id=q['id'], expected_revision=q['source_revision'])
        self.inbox.ops.append(op)
        return op

    def lose_ack_then_scroll_out(self):
        op = self.enqueue(); self.source.lose_ack = True
        with self.assertRaises(Unavailable):
            self.runner.step()
        self.assertEqual((op['state'], len(self.source.sent)), ('uncertain', 1))
        old = scroll_out(self.source.value)
        return op, old

    def test_partial_thread_does_not_block_a_healthy_thread(self):
        held = self.enqueue('other-thread')
        observed = len(self.inbox.observations)
        self.source.values['other-thread']['payloadBudgetExceeded'] = True
        op = self.enqueue()
        self.assertEqual(self.runner.step(), ['other-thread'])
        self.assertEqual((op['state'], len(self.source.sent)), ('accepted', 1))
        self.assertEqual(held['state'], 'queued')
        # Only the healthy thread was re-observed; the partial one goes stale.
        threads = {q['source']['thread_id'] for q, _ in self.inbox.observations[observed:]}
        self.assertEqual(threads, {'pilot-thread'})

    def test_lost_ack_is_acknowledged_from_recovered_history_only(self):
        op, old = self.lose_ack_then_scroll_out()
        # History endpoint unavailable: the answered question is held, not
        # acknowledged, withdrawn or resent.
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual((op['state'], len(self.source.sent)), ('uncertain', 1))
        self.source.publish('pilot-thread', old)
        self.assertEqual(self.runner.step(), [])
        self.assertEqual((op['state'], len(self.source.sent)), ('accepted', 1))

    def test_pending_question_stays_answerable_while_history_is_partial(self):
        snap = self.source.value
        second_question(snap, status='resolved'); with_ids(snap)
        snap['snapshotSequence'] += 1
        scroll_out(snap)
        self.runner.step()
        op = self.enqueue()
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual((op['state'], len(self.source.sent)), ('accepted', 1))

    def test_history_is_bounded_by_pages_cursor_and_bytes(self):
        op, old = self.lose_ack_then_scroll_out()
        for n in range(100):
            self.source.publish('pilot-thread', [], cursor='older' if n == 0 else f'c{n}',
                                nextCursor=f'c{n + 1}', hasMoreHistory=True)
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertLessEqual(len(self.source.history_calls), 2 * t3_questions.HISTORY_PAGES)
        self.assertEqual(op['state'], 'uncertain')
        for name, page in [('repeat', {'nextCursor': 'older', 'hasMoreHistory': True}),
                           ('cursor', {'nextCursor': 'x' * 5000, 'hasMoreHistory': True}),
                           ('bytes', {'padding': 'x' * (t3_questions.HISTORY_BYTES + 1)})]:
            with self.subTest(name=name):
                self.source.history_calls.clear()
                self.source.pages.clear()
                self.source.publish('pilot-thread', [], **page)
                self.source.pages[('pilot-thread', 'x' * 5000)] = self.source.pages[('pilot-thread', 'older')]
                self.assertEqual(self.runner.step(), ['pilot-thread'])
                self.assertLessEqual(len(self.source.history_calls), 2)
                self.assertEqual((op['state'], len(self.source.sent)), ('uncertain', 1))

    def test_history_read_against_a_changed_snapshot_is_discarded(self):
        op, old = self.lose_ack_then_scroll_out()
        self.source.publish('pilot-thread', old)
        def advance():
            self.source.value['snapshotSequence'] += 1
            self.source.after_history = None
        self.source.after_history = advance
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual(op['state'], 'uncertain')
        # Page carries the older cursor's sequence: still not combined.
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual(op['state'], 'uncertain')
        self.source.publish('pilot-thread', old)
        self.assertEqual(self.runner.step(), [])
        self.assertEqual((op['state'], len(self.source.sent)), ('accepted', 1))

    def test_inherited_rows_never_answer_this_thread(self):
        op, old = self.lose_ack_then_scroll_out()
        inherited = copy.deepcopy(old[0]); inherited['threadId'] = 'parent-thread'
        self.source.pages[('pilot-thread', 'older')] = {
            'items': [row(inherited, 'inherited')], 'nextCursor': None,
            'hasMoreHistory': False, 'snapshotSequence': self.source.value['snapshotSequence']}
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual(op['state'], 'uncertain')
        local_foreign = row(inherited, 'local')
        self.source.pages[('pilot-thread', 'older')]['items'] = [local_foreign]
        with self.assertRaisesRegex(Unavailable, 'source_scope'):
            self.runner.step()
        self.assertEqual(len(self.source.sent), 1)

    def test_same_cursor_may_gain_or_lose_recovered_rows(self):
        op, old = self.lose_ack_then_scroll_out()
        self.source.publish('pilot-thread', old)
        self.runner.step()
        del self.source.pages[('pilot-thread', 'older')]
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual((op['state'], len(self.source.sent)), ('accepted', 1))

    def test_restart_while_partial_never_releases_queued_work(self):
        op = self.enqueue()
        self.source.values['pilot-thread']['payloadBudgetExceeded'] = True
        self.restart()
        self.assertEqual(self.runner.step(), ['pilot-thread'])
        self.assertEqual(op['state'], 'queued')
        self.source.values['pilot-thread']['payloadBudgetExceeded'] = False
        self.runner.step(); self.runner.step()
        self.assertEqual((op['state'], self.source.sent), ('uncertain', []))

    def test_replacement_attempt_never_receives_the_answer(self):
        op = self.enqueue(); self.source.lose_ack = True
        with self.assertRaises(Unavailable):
            self.runner.step()
        queued = self.enqueue('other-thread')
        for thread in THREADS['thread_ids']:
            p = self.source.values[thread]['projection']
            self.source.values[thread]['snapshotSequence'] += 1
            p['attempts'].append({'id': 'attempt-2', 'runId': 'run-1'})
            p['providerTurns'][0]['runAttemptId'] = 'attempt-2'
        self.runner.step(); self.runner.step()
        self.assertEqual(len(self.source.sent), 1)
        self.assertEqual((op['state'], op['error_code']), ('uncertain', None))
        self.assertEqual((queued['state'], queued['error_code']),
                         ('uncertain', 'source_request_unavailable'))

    def test_answer_on_a_replaced_provider_session_is_not_our_receipt(self):
        op = self.enqueue(); self.source.lose_ack = True
        with self.assertRaises(Unavailable):
            self.runner.step()
        request = self.source.value['projection']['runtimeRequests'][0]
        request['responseCapability'] = {'type': 'live', 'providerSessionId': 'session-2'}
        self.source.value['snapshotSequence'] += 1
        self.restart(); self.runner.step(); self.runner.step()
        self.assertEqual((op['state'], len(self.source.sent)), ('uncertain', 1))

    def test_native_answer_without_our_dispatch_is_not_acknowledged(self):
        op = self.enqueue()
        request = self.source.value['projection']['runtimeRequests'][0]
        request.update(status='resolved', answers={'colour': ' blue-id '})
        self.source.value['snapshotSequence'] += 1
        self.runner.step(); self.runner.step()
        self.assertEqual(self.source.sent, [])
        self.assertEqual((op['state'], op['error_code']), ('uncertain', 'source_outcome_unconfirmed'))


if __name__ == '__main__':
    unittest.main()
