import test from 'node:test';
import assert from 'node:assert/strict';
import { launchRequest,launchLabel,launchStorage } from '../launches.js';
test('launch requires exact displayed brief and target, stable saved identity survives reload',()=>{
 const brief={id:'brief',revision:4,target_id:'pilot',project_id:'project'};
 const target={revision:'hash',snapshot:{project_id:'project',target:{id:'pilot'}}};
 const body=launchRequest(brief,target,'stable-operation');
 assert.deepEqual(body,{operation_id:'stable-operation',brief_id:'brief',expected_revision:4,target_revision:'hash'});
 assert.throws(()=>launchRequest({...brief,project_id:'other'},target,'new'));
 const data=new Map(),storage={getItem:k=>data.get(k)??null,setItem:(k,v)=>data.set(k,v),removeItem:k=>data.delete(k)};
 launchStorage(storage,'owner').put({launch:body});assert.deepEqual(launchStorage(storage,'owner').get().launch,body);assert.equal(launchStorage(storage,'other').get(),null);
});
test('launch status distinguishes preparation acceptance execution and uncertainty',()=>{
 assert.match(launchLabel({state:'accepted'}),/waiting for preparation/);
 assert.match(launchLabel({state:'uncertain'}),/no automatic relaunch/);
 assert.match(launchLabel({state:'completed'}),/completed/);
});
