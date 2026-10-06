import copy
import json
from pathlib import Path
import tempfile
import unittest
import uuid
from collector import Client, Unavailable, collect, configuration, empty, write_cache

BASE='https://forgejo.example.test'
CONFIG={'owner_id':str(uuid.uuid4()),'base_url':BASE,'account_id':7,'projects':{},'credential_file':'/private/key','cache_file':'/derived/cache'}
REPO={'id':10,'full_name':'team/one','html_url':BASE+'/team/one'}
ITEM={'id':22,'number':3,'title':'A question','html_url':BASE+'/team/one/issues/3','state':'open','updated_at':'2026-10-06T01:00:00Z','assignees':[{'login':'oleg'}]}
class Source(Client):
    def __init__(self):
        self.base=BASE;self.calls=[];self.fail=None
        self.rows={'/user/repos':[REPO],'/repos/team/one/issues':[ITEM],'/repos/team/one/pulls':[], '/repos/team/one/releases':[]}
    def get(self,path):
        self.calls.append(path)
        if self.fail and self.fail in path:raise Unavailable('source_unavailable')
        if path=='/user':return {'id':7,'login':'oleg'}
        route,query=path.split('?');from urllib.parse import parse_qs
        page=int(parse_qs(query)['page'][0])
        # Deliberately return one row/page despite requested limit=50.
        return copy.deepcopy(self.rows[route][page-1:page])

class Tests(unittest.TestCase):
    def test_all_repository_pages_and_checks_are_bound_to_pr_commit(self):
        s=Source();s.rows['/user/repos'].append(dict(REPO,id=11,full_name='team/two',html_url=BASE+'/team/two'))
        for suffix in ('issues','pulls','releases'):s.rows['/repos/team/two/'+suffix]=[]
        s.rows['/repos/team/one/pulls']=[dict(ITEM,head={'sha':'a'*40})]
        s.rows['/repos/team/one/commits/'+'a'*40+'/statuses']=[{'id':9,'context':'ci','status':'success','updated_at':'now'}]
        v=collect(s,CONFIG,None,100)
        self.assertIsNone(v['error']);self.assertEqual(len(v['repos']),2)
        self.assertEqual(v['repos'][0]['pulls'][0]['checks'][0]['state'],'success')
        self.assertTrue(any('page=3' in p for p in s.calls))
        self.assertFalse(any('token' in p for p in s.calls))
    def test_partial_discovery_keeps_every_previous_record(self):
        s=Source();old=collect(s,CONFIG,None,100);s.fail='user/repos?limit=50&page=2'
        v=collect(s,CONFIG,old,200)
        self.assertEqual(v['repos'],old['repos']);self.assertEqual(v['discovered_at'],100);self.assertEqual(v['error'],'source_unavailable')
    def test_repo_failure_and_missing_repo_never_erase_last_good_data(self):
        s=Source();old=collect(s,CONFIG,None,100);s.fail='/pulls'
        v=collect(s,CONFIG,old,200);self.assertEqual(v['repos'][0]['issues'],old['repos'][0]['issues']);self.assertEqual(v['repos'][0]['synced_at'],100);self.assertIsNotNone(v['repos'][0]['error'])
        s.fail=None;s.rows['/user/repos']=[]
        v=collect(s,CONFIG,v,300);self.assertFalse(v['repos'][0]['listed']);self.assertEqual(len(v['repos'][0]['issues']),1)
    def test_malformed_and_repeated_pages_are_unavailable_not_empty(self):
        class Bad(Source):
            def get(self,path):
                if path.startswith('/user/repos?'):return [REPO]
                return super().get(path)
        v=collect(Bad(),CONFIG,None,100);self.assertEqual(v['error'],'duplicate_page')
        s=Source();s.rows['/user/repos']=[{'id':10}]
        self.assertEqual(collect(s,CONFIG,None,100)['error'],'invalid_source')
    def test_account_or_cache_rebinding_fails_closed(self):
        s=Source();c=dict(CONFIG,account_id=8)
        self.assertEqual(collect(s,c,None,100)['error'],'account_changed')
        with self.assertRaises(Unavailable):collect(s,c,empty(CONFIG),100)
    def test_releases_remain_distinct_from_failed_pr_checks(self):
        s=Source();s.rows['/repos/team/one/releases']=[{'id':1,'tag_name':'beta-1','name':'Beta','html_url':BASE+'/release/1','prerelease':True,'published_at':'now','target_commitish':'main','assets':[{'name':'package.zst','browser_download_url':BASE+'/asset/1'}]}]
        first=collect(s,CONFIG,None,100);self.assertEqual(first['repos'][0]['releases'][0]['tag'],'beta-1')
        s.rows['/repos/team/one/releases'][0]['assets'][0]['browser_download_url']='https://foreign.invalid/x'
        last=collect(s,CONFIG,first,200);self.assertEqual(last['repos'][0]['releases'],first['repos'][0]['releases']);self.assertEqual(last['repos'][0]['error'],'foreign_link')
    def test_cache_atomic_restart_and_permissions(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'cache';v=collect(Source(),CONFIG,None,100);write_cache(path,v)
            self.assertEqual(json.loads(path.read_text()),v);self.assertEqual(path.stat().st_mode&0o777,0o600)
            write_cache(path,collect(Source(),CONFIG,json.loads(path.read_text()),200));self.assertEqual(json.loads(path.read_text())['discovered_at'],200)
    def test_transport_is_get_only_and_does_not_forward_credentials_in_urls(self):
        import io
        from collector import NoRedirect
        requests=[]
        class Transport:
            def open(self,request,timeout):
                requests.append(request);return io.BytesIO(b'[]')
        client=Client(BASE,'secret-test-token');client.http=Transport()
        self.assertEqual(client.pages('/user/repos'),[])
        self.assertEqual(len(requests),1);self.assertEqual(requests[0].get_method(),'GET')
        self.assertIsNone(requests[0].data);self.assertNotIn('secret-test-token',requests[0].full_url)
        self.assertIsNone(NoRedirect().redirect_request(None,None,None,None,None,None))
        class Broken:
            def open(self,request,timeout):return io.BytesIO(b'{partial')
        client.http=Broken()
        with self.assertRaises(Unavailable):client.pages('/user/repos')
    def test_config_rejects_non_https_and_arbitrary_link_fields(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'config';path.write_text(json.dumps(CONFIG));path.chmod(0o600)
            self.assertEqual(configuration(path),CONFIG)
            c=dict(CONFIG,base_url='http://localhost');path.write_text(json.dumps(c))
            with self.assertRaises(Unavailable):configuration(path)

if __name__=='__main__':unittest.main()
