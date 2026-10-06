import test from 'node:test';
import assert from 'node:assert/strict';
import {freshness,sourceURL} from '../forgejo.js';
test('stale observations never masquerade as fresh and links stay on configured source',()=>{
 assert.match(freshness({discovered_at:100,stale:true}),/Stale/);
 assert.match(freshness({synced_at:100,stale:false}),/Synced/);
 assert.match(freshness({synced_at:null,error:'source_unavailable'}),/never/);
 const base='https://forgejo.example.test';
 assert.equal(sourceURL(base+'/team/repo',base),base+'/team/repo');
 for(const value of ['javascript:alert(1)','https://evil.test/a','https://user:pass@forgejo.example.test/a','http://forgejo.example.test/a'])assert.equal(sourceURL(value,base),null);
});
