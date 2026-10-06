import test from 'node:test';import assert from 'node:assert/strict';
import {latestPublished,safeResultURL,projectJournal,validateResult,validateStatus} from '../projects.js';
test('later failures never hide last successful publication per platform and channel',()=>{
 const row=(platform,channel,publication,version)=>({report:{platform,channel,publication,version}});
 const rows=[row('web','QA','published','1'),row('Arch','beta','published','2'),row('web','QA','failed','3'),row('web','stable','published','4')];
 assert.deepEqual(latestPublished(rows).map(r=>r.report.version),['1','2','4']);
 rows.push(row('web','QA','published','5'));assert.deepEqual(latestPublished(rows).map(r=>r.report.version),['5','2','4']);
});
test('saved exact metadata retries stay scoped to owner project and operation kind',()=>{
 const data=new Map(),storage={getItem:k=>data.get(k)??null,setItem:(k,v)=>data.set(k,v),removeItem:k=>data.delete(k)};
 const j=projectJournal(storage,'owner','project','result');const request={operation_id:'stable',commit:'b'.repeat(40),what_to_check:'Exact QA'};j.put(request);assert.deepEqual(j.get(),request);
 assert.equal(projectJournal(storage,'other','project','result').get(),null);assert.equal(projectJournal(storage,'owner','other','result').get(),null);assert.equal(projectJournal(storage,'owner','project','status').get(),null);
 for(const bad of ['javascript:alert(1)','http://example.test','https://user:pass@example.test'])assert.equal(safeResultURL(bad),null);
});

test('reject invalid report fields before making an immutable retry journal',()=>{
 const r={platform:'web',channel:'QA',version:'1',what_to_check:'Open it',url:'https://example.test'};
 assert.doesNotThrow(()=>validateResult(r));
 for(const patch of [{platform:' '},{channel:'я'.repeat(65)},{version:'a\0b'},{what_to_check:' '},{url:'javascript:alert(1)'}])assert.throws(()=>validateResult({...r,...patch}));
});

test('status byte limits match the API before persisting a retry',()=>{
 assert.doesNotThrow(()=>validateStatus({status:'',next_step:''}));
 assert.throws(()=>validateStatus({status:'я'.repeat(129),next_step:''}));
 assert.throws(()=>validateStatus({status:'ready',next_step:'a\0b'}));
});
