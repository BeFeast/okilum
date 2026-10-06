import test from 'node:test';
import assert from 'node:assert/strict';
import {mountSyncPairing} from '../sync-pairing.js';
function fixture(t,{expired=false}={}){
 const elements=new Map();
 function element(id=''){return {id,hidden:false,disabled:false,checked:false,value:'',children:[],textContent:'',open:false,
  replaceChildren(...children){this.children=children;if(id==='sync-pairing-vault')this.value=children[0]?.value||'';},append(...children){this.children.push(...children);},showModal(){this.open=true;},close(){this.open=false;},addEventListener(){}};}
 const $=id=>{if(!elements.has(id))elements.set(id,element(id));return elements.get(id);};
 const saved=['document','navigator','setInterval'].map(k=>[k,Object.getOwnPropertyDescriptor(globalThis,k)]);
 t.after(()=>{for(const[k,d]of saved){if(d)Object.defineProperty(globalThis,k,d);else delete globalThis[k];}});
 const credential={id:'key',rawId:new Uint8Array([1]).buffer,type:'public-key',response:{clientDataJSON:new Uint8Array([1]).buffer,authenticatorData:new Uint8Array([2]).buffer,signature:new Uint8Array([3]).buffer},getClientExtensionResults:()=>({})};
 const credentials={get:async()=>credential};
 for(const[k,value]of Object.entries({document:{getElementById:$,createElement:()=>element(),hidden:false},navigator:{credentials},setInterval:()=>0}))Object.defineProperty(globalThis,k,{configurable:true,value});
 let owner='owner',state='requested';const calls=[];
 const api=async path=>{if(path==='/sync/vaults')return {vaults:[{id:'v1',name:'First'},{id:'v2',name:'Second'}]};if(path.startsWith('/sync/requests/')){if(expired)throw Object.assign(new Error('Expired'),{status:404});return {id:'request',name:'Laptop',device_id:'DEVICE',code:'ABCD1234',state,expires:9999999999,vault:state==='approved'?'v2':null};}if(path==='/sync/registrations')return {registrations:[{id:'other',vault:'v1',name:'Other laptop',state:'removal_pending',device_id:'OTHER'}]};throw new Error(path);};
 const post=async(path,body)=>{calls.push([path,body]);if(path==='/auth/login/start')return {publicKey:{challenge:'AQ'}};if(path==='/sync/approve')state='approved';return {};};
 const ui=mountSyncPairing({api,post,owner:()=>owner,requestId:'request',signIn:async()=>{}});
 return {$,ui,calls,credentials,logout:()=>{owner=null;ui.reset();}};
}
test('approval requires code confirmation and completed passkey, retaining chosen vault',async t=>{
 const {$,ui,calls,credentials}=fixture(t);await ui.ready();
 await $('sync-pairing-approve').onclick();assert.equal(calls.length,0);
 $('sync-pairing-match').checked=true;$('sync-pairing-vault').value='v2';
 const real=credentials.get;credentials.get=async()=>null;await $('sync-pairing-approve').onclick();assert.ok(!calls.some(([p])=>p==='/sync/approve'));
 credentials.get=real;await $('sync-pairing-approve').onclick();
 assert.deepEqual(calls.at(-1),['/sync/approve',{id:'request',code:'ABCD1234',vault_id:'v2'}]);
 assert.equal($('sync-pairing-vault').value,'v2');assert.equal($('sync-pairing-approve').disabled,true);
});
test('expired approval still shows existing computer registrations',async t=>{
 const {$,ui}=fixture(t,{expired:true});await ui.ready();assert.equal($('sync-pairing-details').hidden,true);assert.equal($('sync-pairing-list').children.length,1);assert.match($('sync-pairing-status').textContent,/expired/);
});
test('sign-out during passkey prompt cannot approve a stale request',async t=>{
 const {$,ui,calls,credentials,logout}=fixture(t);await ui.ready();$('sync-pairing-match').checked=true;
 const original=credentials.get;credentials.get=async()=>{logout();return original();};await $('sync-pairing-approve').onclick();assert.ok(!calls.some(([p])=>p==='/sync/approve'||p==='/auth/login/finish'));
});
test('background session verification never reopens a dismissed approval or clears confirmation',async t=>{
 const {$,ui}=fixture(t);await ui.ready();$('sync-pairing-match').checked=true;await ui.ready();assert.equal($('sync-pairing-match').checked,true);
 $('sync-pairing-close').onclick();assert.equal($('sync-pairing-dialog').open,false);await ui.ready();assert.equal($('sync-pairing-dialog').open,false);
});
