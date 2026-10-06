"""Explicit native launch and read-only recovery; no automatic launch retries."""
import json
import uuid
from t3_questions import canonical, require, Unavailable


def command_for(op, config):
    target = op['target']
    require(target in config.get('launch_targets', []), 'launch_target_not_allowed')
    require(target['project_id'] == config['project_id'] and
            target['instance_id'] == config['instance_id'] and
            target['source_project_id'] == config['source_project_id'], 'launch_scope')
    t = target['target']
    require(op['brief']['target_id'] == t['id'] and op['brief']['project_id'] == config['project_id'], 'launch_scope')
    for key in ('thread_id','command_id','message_id'):
        require(str(uuid.UUID(op[key])) == op[key], 'invalid_launch_identity')
    require(op['request']['brief_id'] == op['brief']['id'] and
            op['request']['expected_revision'] == op['brief']['revision'], 'invalid_launch_intent')
    branch = 'inbox-' + str(uuid.UUID(op['request']['operation_id']))
    require(len(t['base_commit']) == 40 and all(c in '0123456789abcdef' for c in t['base_commit']), 'invalid_launch_base')
    return {'commandId':op['command_id'],'threadId':op['thread_id'],
        'projectId':target['source_project_id'],'title':op['brief']['title'],
        'modelSelection':t['model_selection'],'runtimeMode':t['runtime_mode'],
        'interactionMode':t['interaction_mode'],
        'workspaceStrategy':{'type':'worktree','baseRef':t['base_commit'],
                             'branch':branch,'startFromOrigin':False},
        'initialMessage':{'messageId':op['message_id'],'text':op['brief']['text'],'attachments':[]}}


def observed(snapshot, command):
    require(snapshot.get('hasMoreHistory') is False and snapshot.get('payloadBudgetExceeded') is False, 'partial_launch_source')
    p = snapshot['projection']; thread = p['thread']
    require(thread['id'] == command['threadId'] and thread['projectId'] == command['projectId'], 'launch_identity_changed')
    require(thread['modelSelection'] == command['modelSelection'] and thread['runtimeMode'] == command['runtimeMode'] and thread['interactionMode'] == command['interactionMode'], 'launch_settings_changed')
    message = [m for m in p['messages'] if m['id'] == command['initialMessage']['messageId']]
    require(len(message) == 1 and message[0]['role'] == 'user' and message[0]['text'] == command['initialMessage']['text'], 'launch_message_unconfirmed')
    runs = [r for r in p['runs'] if r.get('userMessageId') == message[0]['id']]
    require(len(runs) == 1, 'launch_run_unconfirmed')
    run = runs[0]; status = run['status']
    states = {'preparing':'preparing','queued':'accepted','running':'running',
              'completed':'completed','failed':'failed','cancelled':'failed','interrupted':'failed'}
    require(status in states, 'launch_run_unconfirmed')
    path = thread.get('worktreePath')
    if status not in ('preparing','failed','cancelled'):
        require(thread.get('branch') == command['workspaceStrategy']['branch'] and isinstance(path,str) and path.startswith('/'), 'launch_workspace_unconfirmed')
    elif thread.get('branch') is not None:
        require(thread['branch'] == command['workspaceStrategy']['branch'], 'launch_workspace_changed')
    return {'next':states[status],'run_id':run['id'],'worktree_path':path,
            'error_code':'source_run_failed' if states[status]=='failed' else None}


class LaunchRunner:
    def __init__(self,config,journal,source,inbox):
        self.config,self.journal,self.source,self.inbox=config,journal,source,inbox
        self.ready=False
        self.outputs_seen=set()
        journal.db.execute('CREATE TABLE IF NOT EXISTS launch_intents(id TEXT PRIMARY KEY,body TEXT NOT NULL,command TEXT NOT NULL)')
        journal.db.commit()

    def operations(self):
        cursor=0
        for _ in range(100):
            page=self.inbox.request('GET',f'/api/bridge/v1/launches?after={cursor}&limit=100')
            require(isinstance(page.get('operations'),list),'invalid_launch_page')
            yield from page['operations']
            if page.get('has_more') is False:return
            next_cursor=page.get('next_cursor')
            require(type(next_cursor) is int and next_cursor>cursor,'invalid_launch_page')
            cursor=next_cursor
        raise Unavailable('launch_page_limit')

    def persist(self,op,command):
        body=canonical({k:op[k] for k in ('request','brief','target','thread_id','command_id','message_id')})
        key=op['request']['operation_id'];command=canonical(command)
        old=self.journal.db.execute('SELECT body,command FROM launch_intents WHERE id=?',(key,)).fetchone()
        if old:
            require(old==(body,command),'launch_payload_changed');return False
        with self.journal.db:self.journal.db.execute('INSERT INTO launch_intents VALUES(?,?,?)',(key,body,command))
        return True

    def advance(self,op,progress):
        if all(op.get(k)==v for k,v in progress.items() if k!='next') and op['state']==progress['next']:return
        self.inbox.request('POST','/api/bridge/v1/launches/'+op['request']['operation_id'],dict(progress,expected=op['state']))
        op.update({k:v for k,v in progress.items() if k!='next'});op['state']=progress['next']

    def uncertain(self,op,error=None):
        self.advance(op,{'next':'uncertain','run_id':op.get('run_id'),'worktree_path':op.get('worktree_path'),'error_code':error})

    def step(self):
        operations=list(self.operations())
        for op in operations:
            if op['state']=='failed':continue
            if op['state']=='completed':
                key=op['request']['operation_id']
                if key in self.outputs_seen:continue
                try:
                    command=command_for(op,self.config)
                    snapshot=self.source.snapshot(op['thread_id'])
                    progress=observed(snapshot,command)
                    require(progress['next']=='completed' and progress['run_id']==op['run_id'],'output_run_changed')
                    messages=[m for m in snapshot['projection']['messages'] if m.get('role')=='assistant' and m.get('runId')==op['run_id']]
                    if not messages:continue
                    message=messages[-1]
                    if message.get('streaming') or not message.get('text'):continue
                    require(isinstance(message['text'],str) and len(message['text'].encode())<=65536,'output_limit')
                    self.inbox.request('POST','/api/bridge/v1/launches/'+key+'/output',{'run_id':op['run_id'],'message_id':message['id'],'text':message['text']})
                    self.outputs_seen.add(key)
                except Unavailable as error:
                    print(canonical({'event':'launch_output','status':'unavailable','code':str(error)}),flush=True)
                continue
            command=command_for(op,self.config)
            fresh=self.persist(op,command)
            try:snapshot=self.source.snapshot(op['thread_id'])
            except Unavailable as error:
                if str(error)!='http_not_found':raise
                snapshot=None
            if snapshot is not None:
                progress=observed(snapshot,command)
                if op['state']=='queued':self.uncertain(op)
                self.advance(op,progress)
                continue
            if op['state']!='queued':continue
            self.uncertain(op,'recovery_required' if not fresh or not self.ready else None)
            if not fresh or not self.ready:continue
            # Native T3 owns worktree preparation. IDs and exact payload are
            # durable before this call. The result is never used as run completion.
            self.source.launch(command)
            self.advance(op,{'next':'accepted','run_id':None,'worktree_path':None,'error_code':None})
        self.ready=True
