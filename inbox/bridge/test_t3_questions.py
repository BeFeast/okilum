import copy
import json
from pathlib import Path
import tempfile
import unittest
import uuid

from t3_questions import Journal, Runner, Unavailable, answers, project

CONFIG = {'instance_id':'pilot', 'source_project_id':'isolated-project',
          'project_id':'11111111-1111-4111-8111-111111111111', 'thread_ids':['pilot-thread'],
          't3_url':'http://127.0.0.1:9999', 'inbox_url':'https://inbox.example.test'}


def snapshot():
    return {'snapshotSequence':10,'hasMoreHistory':False,'payloadBudgetExceeded':False,
        'projection':{
            'thread':{'id':'pilot-thread','projectId':'isolated-project'},
            'attempts':[{'id':'attempt-1','runId':'run-1'}],
            'providerTurns':[{'id':'turn-1','runAttemptId':'attempt-1'}],
            'runtimeRequests':[{'id':'request-1','nodeId':'node-1','providerTurnId':'turn-1',
                'kind':'user_input','status':'pending','responseCapability':{'type':'live','providerSessionId':'session-1'}}],
            'turnItems':[{'type':'user_input_request','requestId':'request-1','threadId':'pilot-thread',
                'nodeId':'node-1','providerTurnId':'turn-1','runId':'run-1',
                'questions':[{'id':'colour','question':'Which colour?','options':[
                    {'label':'Blue','description':'Choose Blue.','value':' blue-id '},
                    {'label':'Green','description':'Choose Green.'}]}]}]}}


def operation(snap=None):
    _, records = project(snap or snapshot(),CONFIG,'pilot-thread')
    q = next(iter(records.values()))[0]
    return {'request':{'operation_id':str(uuid.uuid4()),'question_id':q['id'],
            'expected_revision':q['source_revision'],
            'answers':[{'id':'colour','text':'','option_ids':[' blue-id ']}]},
            'question':q,'state':'queued','delivery_id':None,'error_code':None}


class Source:
    def __init__(self):
        self.value = snapshot()
        self.sent = []
        self.lose_ack = False
        self.offline = False

    def snapshot(self, thread):
        if self.offline:
            raise Unavailable('source_offline')
        return copy.deepcopy(self.value)

    def respond(self, command):
        self.sent.append(copy.deepcopy(command))
        self.value['snapshotSequence'] += 1
        self.value['projection']['runtimeRequests'][0].update(status='resolved',answers=command['answers'])
        if self.lose_ack:
            raise Unavailable('source_outcome_uncertain')


class Inbox:
    def __init__(self):
        self.ops = []
        self.observations = []
        self.lose_uncertain_ack = False

    def observe(self, question, sequence):
        self.observations.append(copy.deepcopy((question,sequence)))

    def operations(self):
        return copy.deepcopy(self.ops)

    def advance(self, operation, state, error=None):
        saved = next(o for o in self.ops if o['request']['operation_id'] == operation['request']['operation_id'])
        saved['state'] = state
        saved['error_code'] = error
        if state == 'uncertain' and self.lose_uncertain_ack:
            raise Unavailable('http_unavailable')
        operation['state'] = state


class ProjectionTests(unittest.TestCase):
    def test_identity_revision_and_exact_option_values(self):
        snap = snapshot()
        seq, records = project(snap,CONFIG,'pilot-thread')
        q = next(iter(records.values()))[0]
        self.assertEqual(seq,10)
        self.assertTrue(q['can_reply'])
        self.assertEqual(q['fields'][0]['options'][0]['id'],' blue-id ')
        self.assertEqual(answers(operation()),{'colour':' blue-id '})
        snap['snapshotSequence'] += 1
        snap['projection']['runtimeRequests'][0]['status'] = 'resolved'
        done = next(iter(project(snap,CONFIG,'pilot-thread')[1].values()))[0]
        self.assertEqual(done['id'],q['id'])
        self.assertEqual(done['source_revision'],q['source_revision'])
        self.assertEqual(done['state'],'answered')
        self.assertFalse(done['can_reply'])
        snap['projection']['turnItems'][0]['questions'][0]['question'] = 'Different wording?'
        changed = next(iter(project(snap,CONFIG,'pilot-thread')[1].values()))[0]
        self.assertNotEqual(changed['source_revision'],q['source_revision'])

    def test_partial_mismatched_and_unsupported_sources_fail_closed(self):
        mutations = [lambda s:s.update(hasMoreHistory=True),
                     lambda s:s.update(payloadBudgetExceeded=True),
                     lambda s:s['projection']['thread'].update(projectId='production-project'),
                     lambda s:s['projection'].update(turnItems=[]),
                     lambda s:s['projection'].update(providerTurns=[]),
                     lambda s:s['projection']['turnItems'][0].update(nodeId='another-node'),
                     lambda s:s['projection']['turnItems'][0]['questions'][0].update(required=False),
                     lambda s:s['projection']['runtimeRequests'].append(copy.deepcopy(s['projection']['runtimeRequests'][0]))]
        for change in mutations:
            with self.subTest(change=change):
                s = snapshot(); change(s)
                with self.assertRaises(Unavailable):
                    project(s,CONFIG,'pilot-thread')
        self.assertTrue(project(snapshot(),CONFIG,'pilot-thread')[1])

    def test_capabilities_and_cancelled_states(self):
        for capability in ['message','not_resumable']:
            s = snapshot(); s['projection']['runtimeRequests'][0]['responseCapability'] = {'type':capability}
            self.assertFalse(next(iter(project(s,CONFIG,'pilot-thread')[1].values()))[0]['can_reply'])
        for state in ['expired','cancelled']:
            s = snapshot(); s['projection']['runtimeRequests'][0]['status'] = state
            self.assertEqual(next(iter(project(s,CONFIG,'pilot-thread')[1].values()))[0]['state'],'withdrawn')

    def test_native_text_multi_and_ambiguous_answers(self):
        op = operation(); op['request']['answers'][0]['text'] = 'both'
        with self.assertRaises(Unavailable): answers(op)
        op['request']['answers'][0]['option_ids'] = []
        self.assertEqual(answers(op),{'colour':'both'})
        op['question']['fields'][0]['allow_text'] = False
        with self.assertRaises(Unavailable): answers(op)
        op = operation();op['question']['fields'][0]['multiple'] = True
        op['request']['answers'][0]['option_ids'] = [' blue-id ','Green']
        self.assertEqual(answers(op),{'colour':[' blue-id ','Green']})
        op['request']['answers'][0]['option_ids'] = ['unexpected']
        with self.assertRaises(Unavailable): answers(op)


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name)/'state.db'
        self.source, self.inbox = Source(), Inbox()
        self.journal = Journal(self.path,CONFIG)
        self.runner = Runner(CONFIG,self.journal,self.source,self.inbox)
        self.runner.step()  # positive baseline, no pre-existing intents

    def tearDown(self):
        self.journal.db.close()
        self.dir.cleanup()

    def enqueue(self):
        op = operation(); self.inbox.ops.append(op); return op

    def restart(self):
        self.journal.db.close()
        self.journal = Journal(self.path,CONFIG)
        self.runner = Runner(CONFIG,self.journal,self.source,self.inbox)

    def test_answer_and_lost_ack_reconcile_once_even_after_restart(self):
        op = self.enqueue(); self.source.lose_ack = True
        with self.assertRaises(Unavailable): self.runner.step()
        self.assertEqual(len(self.source.sent),1)
        self.assertEqual(op['state'],'uncertain')
        self.restart();self.runner.step();self.runner.step()
        self.assertEqual(op['state'],'accepted')
        self.assertEqual(len(self.source.sent),1)
        self.assertEqual(self.source.sent[0]['answers'],{'colour':' blue-id '})
        # T3 resolved precedes provider callback; never claim delivered here.
        self.assertNotEqual(op['state'],'delivered')

    def test_receipt_and_repeated_poll_never_repeat_dispatch(self):
        op=self.enqueue();self.runner.step();self.runner.step()
        self.assertEqual(len(self.source.sent),1);self.assertEqual(op['state'],'accepted')

    def test_inbox_commit_lost_ack_prevents_external_send(self):
        op=self.enqueue();self.inbox.lose_uncertain_ack=True
        with self.assertRaises(Unavailable):self.runner.step()
        self.inbox.lose_uncertain_ack=False;self.restart();self.runner.step()
        self.assertEqual(op['state'],'uncertain');self.assertEqual(self.source.sent,[])

    def test_restore_of_queued_intent_does_not_release_old_work(self):
        op=self.enqueue();self.restart();self.runner.step();self.runner.step()
        self.assertEqual(self.source.sent,[]);self.assertEqual(op['state'],'uncertain')

    def test_local_intent_survives_server_restore_to_queued(self):
        op=self.enqueue();self.runner.step()
        # Both accepted Inbox receipt and source answer disappear in an older
        # backup; local journal still forbids release even without process restart.
        op['state']='queued';self.source.value=snapshot()
        self.source.value['snapshotSequence']=20
        self.runner.step()
        self.assertEqual(len(self.source.sent),1);self.assertEqual(op['state'],'uncertain')

    def test_stale_revision_before_first_send_is_rejected(self):
        op=self.enqueue();self.source.value['snapshotSequence']+=1
        self.source.value['projection']['turnItems'][0]['questions'][0]['question']='Changed question?'
        self.runner.step()
        self.assertEqual(self.source.sent,[]);self.assertEqual(op['state'],'rejected')
        self.assertEqual(op['error_code'],'source_stale')

    def test_absent_or_partial_source_is_not_withdrawal(self):
        op=self.enqueue();self.source.value['hasMoreHistory']=True
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(op['state'],'queued');self.assertEqual(self.source.sent,[])
        self.source.value['hasMoreHistory']=False
        self.source.value['snapshotSequence']+=1
        self.source.value['projection']['runtimeRequests']=[]
        self.source.value['projection']['turnItems']=[]
        self.runner.step()
        self.assertEqual(op['state'],'uncertain');self.assertEqual(self.source.sent,[])

    def test_scope_and_payload_changes_are_rejected(self):
        op=self.enqueue();op['question']['source']['thread_id']='production-thread'
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(self.source.sent,[])
        op['question']['source']['thread_id']='pilot-thread';self.runner.step()
        self.assertEqual(len(self.source.sent),1)
        op['state']='uncertain';op['request']['answers'][0]['option_ids']=['Green']
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(len(self.source.sent),1)

    def test_cursor_and_instance_binding_prevent_restore_confusion(self):
        self.source.value['snapshotSequence']=9
        with self.assertRaises(Unavailable):self.runner.step()
        other=dict(CONFIG,instance_id='different-source')
        with self.assertRaises(Unavailable):Journal(self.path,other)
        self.assertEqual(self.source.sent,[])


if __name__ == '__main__':
    unittest.main()
