"""Contract fixtures pinned to Maestro PR1289 / 0812fd6, no live fleet access."""
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
import uuid
from maestro import Journal, Runner, Source, Unavailable, canonical, command, project


def config():
    return dict(instance_id='instance',source_project_id='pilot',project_id='11111111-1111-4111-8111-111111111111',
                maestro_url='http://127.0.0.1:1',inbox_url='https://inbox.example.test',approval_actions=['merge_pr'])


def question():
    return dict(kind='question',capabilities=['reply'],instance_id='instance',project_id='pilot',worker_id='worker',
                worker_generation='attempt',thread_id='thread',question_id='q1',revision='opaque:1',
                prompt='Which colour?',options=[{'id':'blue','label':'Blue'}],allow_text=True,status='pending')


def approval():
    return dict(kind='approval',instance_id='instance',project_id='pilot',id='a1',revision='digest-1',capabilities=['approve','reject'],
                approval=dict(id='a1',action='merge_pr',repo='BeFeast/fixture',target={'pr':1,'head_sha':'abc123'},summary='Merge fixture change',risk='high',payload_hash='payload',target_state_hash='head',status='pending'))


def operation(q, option='blue'):
    return {'question':copy.deepcopy(q),'state':'queued','delivery_id':None,'error_code':None,
            'request':{'operation_id':str(uuid.uuid4()),'question_id':q['id'],'expected_revision':q['source_revision'],
                       'answers':[{'id':q['fields'][0]['id'],'text':'','option_ids':[option]}]}}


class InboxFixture:
    def __init__(self):
        self.ops=[];self.questions={}
    def operations(self):return self.ops
    def observe(self,q,seq):self.questions[q['id']]=copy.deepcopy(q)
    def advance(self,op,state,error=None):op['state']=state;op['error_code']=error


class SourceFixture:
    def __init__(self,c):
        self.c=c;self.q=question();self.a=approval();self.receipts={};self.sent=[];self.cursor=1;self.lose=False;self.race=False
    def snapshot(self):
        return self.cursor,{q['id']:q for q in [project(self.q,self.c),project(self.a,self.c,True)]}
    def lookup(self,key):return copy.deepcopy(self.receipts.get(key,(404,{})))
    def send(self,path,body):
        self.sent.append((path,copy.deepcopy(body)))
        key=body['operation_id']
        if self.race:
            result=(409,{'operation_id':key,'status':'rejected','error':'stale_revision','current':self.a})
        elif path.startswith('/questions/'):
            self.q.update(pending_operation_id=key,answer=body['answer'],revision='opaque:2',capabilities=[])
            result=(200,{'operation_id':key,'status':'accepted','question':copy.deepcopy(self.q)})
        else:
            self.a['approval']['status']='approved' if path.endswith('/approve') else 'rejected';self.a['capabilities']=[];self.a['revision']='digest-2'
            result=(200,{'operation_id':key,'status':'delivered','current':copy.deepcopy(self.a)})
        self.receipts[key]=result
        if self.lose:raise Unavailable('source_unavailable')
        return copy.deepcopy(result)
    def ack(self,key):
        self.q.pop('pending_operation_id',None);self.q.update(answer_operation_id=key,status='answered',revision='opaque:3')
        self.receipts[key]=(200,{'operation_id':key,'status':'delivered','question':copy.deepcopy(self.q)})


class MaestroTests(unittest.TestCase):
    def setUp(self):
        self.dir=tempfile.TemporaryDirectory();os.chmod(self.dir.name,0o700)
        self.c=config();self.path=Path(self.dir.name)/'state.db';self.j=Journal(self.path,self.c)
        self.source=SourceFixture(self.c);self.inbox=InboxFixture();self.runner=Runner(self.c,self.j,self.source,self.inbox)
        self.runner.step()
    def tearDown(self):self.j.db.close();self.dir.cleanup()
    def queue(self,approval_item=False):
        q=project(self.source.a if approval_item else self.source.q,self.c,approval_item)
        op=operation(q,'approve' if approval_item else 'blue');self.inbox.ops.append(op);return op
    def test_lost_response_restart_lookup_then_worker_ack_without_resend(self):
        op=self.queue();self.source.lose=True
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(op['state'],'uncertain');self.assertEqual(len(self.source.sent),1)
        self.j.db.close();self.j=Journal(self.path,self.c)
        self.runner=Runner(self.c,self.j,self.source,self.inbox);self.runner.step()
        self.assertEqual(op['state'],'accepted');self.assertEqual(len(self.source.sent),1)
        self.source.ack(op['request']['operation_id']);self.runner.step()
        self.assertEqual(op['state'],'delivered');self.assertEqual(len(self.source.sent),1)
    def test_approval_stale_before_send_and_racing_409(self):
        op=self.queue(True);self.source.a['revision']='changed'
        self.runner.step();self.assertEqual(op['state'],'rejected');self.assertEqual(self.source.sent,[])
        self.inbox.ops=[];op=self.queue(True);self.source.race=True;self.runner.step()
        self.assertEqual(op['state'],'rejected');self.assertEqual(len(self.source.sent),1)
    def test_approval_decision_receipt_is_not_worker_or_execution_completion(self):
        op=self.queue(True);self.runner.step();self.assertEqual(op['state'],'delivered')
        path,body=self.source.sent[0];self.assertEqual(path,'/approvals/a1/approve')
        self.assertEqual(set(body),{'operation_id','instance_id','project_id','expected_revision'})
        self.assertEqual(body['expected_revision'],'digest-1');self.runner.step();self.assertEqual(len(self.source.sent),1)
    def test_restart_queued_and_unknown_outcome_never_send(self):
        op=self.queue();restarted=Runner(self.c,self.j,self.source,self.inbox);restarted.step();restarted.step()
        self.assertEqual(op['state'],'uncertain');self.assertEqual(self.source.sent,[])
    def test_changed_intent_receipt_identity_and_instance_fail_closed(self):
        op=self.queue();path,body=command(op);self.j.intent(op,path,body)
        op['request']['answers'][0]['text']='changed'
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(self.source.sent,[])
        self.source.q['instance_id']='other'
        with self.assertRaises(Unavailable):self.runner.observe()
    def test_capability_change_invalidates_consent_and_opaque_revision_survives(self):
        q=project(self.source.a,self.c,True);self.source.a['capabilities']=['reject']
        changed=project(self.source.a,self.c,True);self.assertNotEqual(q['source_revision'],changed['source_revision'])
        self.assertEqual(changed['fields'][0]['options'],[{'id':'reject','label':'Reject'}])
        op=operation(project(self.source.q,self.c));self.assertEqual(command(op)[1]['expected_revision'],'opaque:1')
    def test_cursor_regression_and_binding_change_refused(self):
        self.runner.observe();self.source.cursor=0
        with self.assertRaises(Unavailable):self.runner.observe()
        changed=dict(self.c,instance_id='replacement')
        with self.assertRaises(Unavailable):Journal(self.path,changed)
    def test_receipt_with_wrong_answer_does_not_claim_delivery(self):
        op=self.queue();self.runner.step();receipt=self.source.receipts[op['request']['operation_id']][1]
        receipt['question']['answer']={'text':'wrong'}
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(op['state'],'accepted')


class GlobalApprovalTests(unittest.TestCase):
    def test_global_null_and_absent_project_and_recover_without_resending(self):
        for absent in (False, True):
            with self.subTest(absent=absent), tempfile.TemporaryDirectory() as d:
                os.chmod(d,0o700);c=config();c['approval_actions']=['change_global_config']
                source=SourceFixture(c);source.a['approval']['action']='change_global_config'
                source.a['approval']['target']=None
                if absent:source.a['approval'].pop('target')
                j=Journal(Path(d)/'db',c);inbox=InboxFixture();runner=Runner(c,j,source,inbox)
                try:
                    runner.step();q=project(source.a,c,True)
                    self.assertEqual(q['approval']['target'],{'scope':'global'})
                    op=operation(q,'approve');inbox.ops=[op];source.lose=True
                    with self.assertRaises(Unavailable):runner.step()
                    self.assertEqual(op['state'],'uncertain')
                    self.assertEqual(len(source.sent),1)
                    self.assertNotIn('target',source.sent[0][1])
                    key=op['request']['operation_id'];correct=copy.deepcopy(source.receipts[key])
                    source.receipts[key][1]['current']['approval']['target']={'pr':99}
                    with self.assertRaises(Unavailable):Runner(c,j,source,inbox).step()
                    self.assertEqual(op['state'],'uncertain')
                    source.receipts[key]=correct
                    j.db.close();j=Journal(Path(d)/'db',c)
                    restarted=Runner(c,j,source,inbox);restarted.step();restarted.step()
                    self.assertEqual(op['state'],'delivered');self.assertEqual(len(source.sent),1)
                finally:j.db.close()

    def test_invalid_targets_and_scoped_missing_targets_still_fail_closed(self):
        for action in ('change_global_config','merge_pr'):
            c=config();c['approval_actions']=[action]
            invalid=['global',[],1,True,{'too_large':'x'*16385}]
            if action!='change_global_config':invalid.append(None)
            for value in invalid:
                with self.subTest(action=action,target_type=type(value).__name__):
                    raw=approval();raw['approval'].update(action=action,target=value)
                    with self.assertRaises(Unavailable):project(raw,c,True)
            if action!='change_global_config':
                raw=approval();raw['approval'].pop('target')
                with self.assertRaises(Unavailable):project(raw,c,True)


class PaginationTests(unittest.TestCase):
    def test_truncated_snapshot_replays_all_changes_and_rejects_partial_pages(self):
        c=config();source=Source(c,'fixture-secret');q=question();a=approval();calls=[]
        def read(path):
            calls.append(path)
            if path=='/questions':return {'truncated':True,'questions':[],'snapshot_cursor':1}
            if path=='/changes?cursor=0':return {'changes':[{'cursor':1,'question':q}],'next_cursor':1,'has_more':True}
            if path=='/changes?cursor=1':return {'changes':[],'next_cursor':1,'has_more':False}
            if path=='/approvals':return {'approvals':[a]}
            raise AssertionError(path)
        source.read=read;cursor,rows=source.snapshot();self.assertEqual(cursor,1);self.assertEqual(len(rows),2)
        self.assertIn('/changes?cursor=1',calls)
        source.read=lambda _: {'truncated':True,'questions':[],'snapshot_cursor':1} if _=='/questions' else {'changes':[],'next_cursor':0,'has_more':True}
        with self.assertRaises(Unavailable):source.snapshot()



class HttpContractTests(unittest.TestCase):
    def test_real_http_uses_project_scope_exact_reply_and_replays_persisted_receipt(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        import sqlite3
        with tempfile.TemporaryDirectory() as directory:
            database=str(Path(directory)/'source.db')
            with sqlite3.connect(database) as db:db.execute('CREATE TABLE operations(id TEXT PRIMARY KEY, body TEXT NOT NULL)')
            requests=[]
            class Handler(BaseHTTPRequestHandler):
                def log_message(self,*args):pass
                def answer(self,status,body):
                    data=canonical(body).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(data)
                def do_GET(self):
                    if self.headers.get('Authorization')!='Bearer fixture-token':return self.answer(403,{'error':'scope'})
                    prefix='/api/v1/inbox/projects/pilot'
                    if self.path==prefix+'/questions':return self.answer(200,{'instance_id':'instance','questions':[question()],'snapshot_cursor':1,'truncated':False})
                    if self.path==prefix+'/approvals':return self.answer(200,{'instance_id':'instance','approvals':[approval()]})
                    if self.path.startswith(prefix+'/operations/'):
                        key=self.path.split('/')[-1]
                        with sqlite3.connect(database) as db:row=db.execute('SELECT body FROM operations WHERE id=?',(key,)).fetchone()
                        return self.answer(200,json.loads(row[0])) if row else self.answer(404,{'error':'not found'})
                    self.answer(404,{'error':'route'})
                def do_POST(self):
                    body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    requests.append((self.path,body))
                    if self.path!='/api/v1/inbox/projects/pilot/questions/q1/reply':return self.answer(404,{'error':'route'})
                    expected={'operation_id','instance_id','project_id','thread_id','worker_generation','expected_revision','answer'}
                    if set(body)!=expected or body['expected_revision']!='opaque:1':return self.answer(400,{'error':'contract'})
                    q=question();q.update(pending_operation_id=body['operation_id'],answer=body['answer'],revision='opaque:2',capabilities=[])
                    receipt={'operation_id':body['operation_id'],'status':'accepted','question':q}
                    with sqlite3.connect(database) as db:db.execute('INSERT INTO operations VALUES(?,?)',(body['operation_id'],canonical(receipt)))
                    # Commit before deliberately losing the response.
                    self.connection.close()
            server=ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
            try:
                c=config();c['maestro_url']='http://127.0.0.1:'+str(server.server_port)
                source=Source(c,'fixture-token');cursor,records=source.snapshot();self.assertEqual(cursor,1);self.assertEqual(len(records),2)
                q=project(question(),c);op=operation(q);path,body=command(op)
                with self.assertRaises(Unavailable):source.send(path,body)
                recovered=Source(c,'fixture-token');status,receipt=recovered.lookup(body['operation_id'])
                self.assertEqual(status,200);self.assertEqual(receipt['status'],'accepted');self.assertEqual(len(requests),1)
                self.assertEqual(Source(c,'wrong-token').lookup(body['operation_id'])[0],403)
                self.assertEqual(recovered.lookup(str(uuid.uuid4()))[0],404)
            finally:server.shutdown();server.server_close();thread.join()


class AdditionalRecoveryTests(unittest.TestCase):
    def test_lost_approval_receipt_is_recovered_once_and_wrong_target_refused(self):
        with tempfile.TemporaryDirectory() as d:
            os.chmod(d,0o700);c=config();j=Journal(Path(d)/'db',c);source=SourceFixture(c);inbox=InboxFixture();runner=Runner(c,j,source,inbox)
            try:
                runner.step();op=operation(project(source.a,c,True),'reject');inbox.ops=[op];source.lose=True
                with self.assertRaises(Unavailable):runner.step()
                self.assertEqual(op['state'],'uncertain');self.assertEqual(len(source.sent),1)
                key=op['request']['operation_id'];receipt=source.receipts[key][1]
                correct=copy.deepcopy(receipt);receipt['current']['approval']['target']={'pr':99}
                with self.assertRaises(Unavailable):runner.step()
                self.assertEqual(op['state'],'uncertain')
                source.receipts[key]=(200,correct);Runner(c,j,source,inbox).step()
                self.assertEqual(op['state'],'delivered');self.assertEqual(len(source.sent),1)
            finally:j.db.close()

if __name__=='__main__':unittest.main()
