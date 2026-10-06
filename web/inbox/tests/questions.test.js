import test from 'node:test';
import assert from 'node:assert/strict';
import { buildReply, questionStatus, replyJournal } from '../questions.js';
const question = () => ({id:'question',state:'pending',can_reply:true,source_fresh:true,source_revision:'r1',fields:[{id:'colour',prompt:'Colour?',options:[{id:' blue ',label:'Blue'}],allow_text:true,multiple:false}]});
const values = {colour:{text:'',option_ids:[' blue ']}};
test('native option IDs and exact consent survive storage/reload without reminting',()=>{
  const data=new Map(); const storage={getItem:k=>data.get(k)??null,setItem:(k,v)=>data.set(k,v),removeItem:k=>data.delete(k)};
  const body=buildReply(question(),values,'stable-id');
  assert.equal(body.answers[0].option_ids[0],' blue ');
  replyJournal(storage,'oleg').put(body);
  assert.deepEqual(replyJournal(storage,'oleg').get('question'),body);
  assert.equal(replyJournal(storage,'other').get('question'),null);
  assert.throws(()=>replyJournal(storage,'oleg').put({...body,operation_id:'new-id'}),/already saved/);
  assert.deepEqual(replyJournal(storage,'oleg').get('question'),body);
});
test('stale withdrawn reserved and ambiguous answers are never dispatched',()=>{
  for(const changes of [{source_fresh:false},{can_reply:false},{state:'withdrawn'},{pending_operation_id:'existing'}]) assert.throws(()=>buildReply({...question(),...changes},values,'op'));
  assert.throws(()=>buildReply(question(),{colour:{text:'custom',option_ids:[' blue ']}},'op'),/either/);
  assert.throws(()=>buildReply(question(),{},'op'),/every/);
  assert.throws(()=>buildReply(question(),{colour:{text:'',option_ids:['changed']}},'op'),/changed/);
});
test('accepted is not delivered and source freshness is visible',()=>{
  assert.match(questionStatus(question(),{state:'accepted'}),/not yet confirmed/);
  assert.match(questionStatus(question(),{state:'uncertain'}),/no automatic resend/);
  assert.match(questionStatus({...question(),source_fresh:false},null),/stale/);
  assert.match(questionStatus(question(),null,false),/Offline/);
});
test('storage failure does not return a saved intent',()=>{
  const journal=replyJournal({getItem:()=>null,setItem:()=>{throw new Error('quota');}},'owner');
  assert.throws(()=>journal.put(buildReply(question(),values,'op')),/quota/);
});

 test('option display removes redundant descriptions without changing meaningful detail', async () => {
 const {optionLabel} = await import('../questions.js');
 assert.equal(optionLabel('Blue — Choose Blue.'), 'Blue');
 assert.equal(optionLabel('Blue — Blue'), 'Blue');
 assert.equal(optionLabel('Blue — Recommended for the pilot'), 'Blue — Recommended for the pilot');
 });
