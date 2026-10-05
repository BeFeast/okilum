#!/usr/bin/env python3
"""Exercise only a fresh t3-target-native-fixture's production service.

Usage: python3 scripts/verify-t3-target-native-fixture.py FIXTURE_DIRECTORY
This adopts the synthetic candidate; use a separate fresh fixture for GUI review.
"""
import hashlib,json,pathlib,socket,sys,uuid
root=pathlib.Path(sys.argv[1]); f=json.loads((root/'fixture.json').read_text())
assert f.get('schema') == 'tessera-native-fixture/v1' and f.get('synthetic_provider') is True
assert f.get('production_runner') is True and f.get('production_service') is True
host,port=f['workspace']['endpoint'].split(':')
assert host == '127.0.0.1'
assert f['candidate']['environment_id'] == 'synthetic-new-environment'
assert f['candidate']['project_id'] == 'synthetic-project'
assert f['candidate']['base_url'].startswith('http://127.0.0.1:')
assert f['candidate']['token_env'] == 'file:' + str(root / 'token')
assert (root / 'token').read_text() == 'synthetic-only-319'
assert pathlib.Path(f['workspace']['identity']['root']).resolve() == (root/'brain').resolve()
def call(op,**payload):
 with socket.create_connection((host,int(port))) as s:
  s.sendall((json.dumps({'schema':'ai-brain/workspace-v1','id':str(uuid.uuid4()),'expected_workspace':f['workspace']['identity'],'op':op,**payload})+'\n').encode()); r=json.loads(s.makefile().readline()); assert r['ok'],r; return r['data']
def manifest():
 return {str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for folder in ['brain','runtime'] for p in (root/folder).rglob('*') if p.is_file() and 'index' not in str(p.relative_to(root))}
before=manifest(); review=call('t3_target_prepare',candidate=f['candidate']); assert review['ready'] and not review['blockers']; assert review['associations'][0]['association']['kind']=='historical_terminal_unroutable'; assert before==manifest(),'prepare wrote durable state'
old=json.loads((root/'runtime/state.json').read_text()); request={'operation_id':str(uuid.uuid4()),'review':review}; outcome=call('t3_target_adopt',request=request); assert outcome['status']=='committed',outcome
replay=call('t3_target_adopt',request=request); assert replay==outcome,'exact adoption replay changed'
current=call('t3_target_get'); assert current['historical_terminal_unroutable_count']==1 and current['active']['environment_id']=='synthetic-new-environment'
new=json.loads((root/'runtime/state.json').read_text())['state']; assert old['dispatch']==new['dispatch'],'historical dispatch changed'; assert old['events']==new['events'],'historical events changed'
after=manifest(); added=sorted(set(after)-set(before)); changed=[k for k in before if before[k]!=after.get(k)]; assert changed==['runtime/state.json'],changed
report={'schema':'tessera-319-production-probe/v1','production_runner':True,'production_service':True,'prepare_read_only':True,'unknown_origin_review':True,'adopt_committed':True,'exact_replay':True,'current_local_only_count':1,'historical_dispatch_events_unchanged':True,'changed_existing_files':changed,'added_files':added,'outcome':outcome,'current':current}; (root/'probe.json').write_text(json.dumps(report,indent=2)); print(json.dumps({k:v for k,v in report.items() if k not in ['outcome','current']}))
