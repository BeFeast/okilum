import copy
import tempfile
from pathlib import Path
import unittest
import uuid
from t3_questions import Journal,Unavailable
from t3_launch import LaunchRunner,command_for,observed
from test_t3_questions import CONFIG


def fixture():
    target={'project_id':CONFIG['project_id'],'instance_id':CONFIG['instance_id'],'source_project_id':CONFIG['source_project_id'],
            'target':{'id':'pilot','label':'Pilot','repository':'fixture','base_commit':'a'*40,'model_selection':{'instanceId':'codex','model':'test'},'runtime_mode':'approval-required','interaction_mode':'default'}}
    brief={'id':str(uuid.uuid4()),'project_id':CONFIG['project_id'],'revision':1,'title':'Brief','text':'Exact text','target_id':'pilot'}
    op={'request':{'operation_id':str(uuid.uuid4()),'brief_id':brief['id'],'expected_revision':1,'target_revision':'opaque'},'brief':brief,'target':target,
        'thread_id':str(uuid.uuid4()),'command_id':str(uuid.uuid4()),'message_id':str(uuid.uuid4()),'state':'queued','run_id':None,'worktree_path':None,'error_code':None}
    return dict(CONFIG,launch_targets=[target]),op


class Source:
    def __init__(self):self.sent=[];self.snap=None;self.lose=False
    def snapshot(self,thread):
        if self.snap is None:raise Unavailable('http_not_found')
        return copy.deepcopy(self.snap)
    def launch(self,command):
        self.sent.append(copy.deepcopy(command))
        self.snap={'hasMoreHistory':False,'payloadBudgetExceeded':False,'projection':{
            'thread':{'id':command['threadId'],'projectId':command['projectId'],'branch':command['workspaceStrategy']['branch'],'worktreePath':'/isolated/new-worktree',
                      'modelSelection':command['modelSelection'],'runtimeMode':command['runtimeMode'],'interactionMode':command['interactionMode']},
            'messages':[{'id':command['initialMessage']['messageId'],'role':'user','text':command['initialMessage']['text']}],
            'runs':[{'id':'original-run','userMessageId':command['initialMessage']['messageId'],'status':'preparing'}]}}
        if self.lose:raise Unavailable('source_outcome_uncertain')


class Inbox:
    def __init__(self):self.ops=[];self.lose=False
    def request(self,method,path,body=None):
        if method=='GET':return {'operations':copy.deepcopy(self.ops),'next_cursor':len(self.ops),'has_more':False}
        op=next(o for o in self.ops if o['request']['operation_id']==path.split('/')[-1])
        self.assert_expected=body['expected']==op['state']
        if not self.assert_expected:raise Unavailable('conflict')
        op.update({k:v for k,v in body.items() if k not in ('expected','next')});op['state']=body['next']
        if self.lose:raise Unavailable('http_unavailable')
        return copy.deepcopy(op)


class LaunchTests(unittest.TestCase):
    def setUp(self):
        self.dir=tempfile.TemporaryDirectory();self.config,self.op=fixture();self.source=Source();self.inbox=Inbox()
        self.journal=Journal(Path(self.dir.name)/'state',self.config)
        self.runner=LaunchRunner(self.config,self.journal,self.source,self.inbox);self.runner.step()
    def tearDown(self):self.journal.db.close();self.dir.cleanup()
    def test_one_launch_worktree_binding_and_run_progress(self):
        self.inbox.ops=[self.op];self.runner.step();self.runner.step()
        self.assertEqual(len(self.source.sent),1);self.assertEqual(self.op['state'],'preparing')
        self.assertEqual(self.op['worktree_path'],'/isolated/new-worktree')
        self.source.snap['projection']['runs'][0]['status']='completed';self.runner.step();self.runner.step()
        self.assertEqual(self.op['state'],'completed');self.assertEqual(self.op['run_id'],'original-run');self.assertEqual(len(self.source.sent),1)
        self.assertEqual(self.source.sent[0]['workspaceStrategy']['baseRef'],'a'*40)
    def test_lost_launch_ack_restart_reads_same_thread_without_relaunch(self):
        self.inbox.ops=[self.op];self.source.lose=True
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(self.op['state'],'uncertain')
        self.runner=LaunchRunner(self.config,self.journal,self.source,self.inbox);self.runner.step()
        self.assertEqual(self.op['state'],'preparing');self.assertEqual(len(self.source.sent),1)
    def test_lost_inbox_ack_never_calls_source(self):
        self.inbox.ops=[self.op];self.inbox.lose=True
        with self.assertRaises(Unavailable):self.runner.step()
        self.inbox.lose=False;self.runner=LaunchRunner(self.config,self.journal,self.source,self.inbox);self.runner.step()
        self.assertEqual(self.op['state'],'uncertain');self.assertEqual(self.source.sent,[])
    def test_restored_queued_without_local_journal_is_quarantined(self):
        self.inbox.ops=[self.op];self.runner=LaunchRunner(self.config,self.journal,self.source,self.inbox)
        self.runner.step();self.runner.step();self.assertEqual(self.op['state'],'uncertain');self.assertEqual(self.source.sent,[])
    def test_existing_journal_with_server_rollback_does_not_resend(self):
        self.inbox.ops=[self.op];self.runner.step();self.source.snap=None;self.op['state']='queued'
        self.runner.step();self.assertEqual(self.op['state'],'uncertain');self.assertEqual(len(self.source.sent),1)
    def test_foreign_target_and_changed_intent_fail_closed(self):
        self.op['target']=copy.deepcopy(self.op['target']);self.op['target']['source_project_id']='production'
        with self.assertRaises(Unavailable):command_for(self.op,self.config)
        config,op=fixture();self.config=config;self.runner=LaunchRunner(config,self.journal,self.source,self.inbox);self.runner.step()
        self.inbox.ops=[op];self.runner.step();op['brief']['text']='Different consent'
        with self.assertRaises(Unavailable):self.runner.step()
        self.assertEqual(len(self.source.sent),1)
    def test_snapshot_wrong_message_worktree_or_partial_is_not_success(self):
        command=command_for(self.op,self.config);self.source.launch(command)
        self.assertEqual(observed(self.source.snap,command)['next'],'preparing')
        for change in [lambda p:p.update(hasMoreHistory=True),lambda p:p['projection']['messages'][0].update(text='Other text'),lambda p:p['projection']['thread'].update(projectId='another'),lambda p:p['projection']['thread'].update(branch='other')]:
            s=copy.deepcopy(self.source.snap);change(s)
            with self.assertRaises(Unavailable):observed(s,command)


if __name__=='__main__':unittest.main()
