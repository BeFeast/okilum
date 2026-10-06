import { requestOptions, credentialJSON } from './webauthn.js';

export const syncState = state => ({provisioning:'Connecting to the vault',hub_ready:'Hub connected · finish setup in Tessera',removal_pending:'Removal pending · hub may still sync',revoked:'Removed from this hub'}[state] || 'Checking connection');
export function mountSyncPairing({api,post,owner,requestId,signIn}) {
 const $=id=>document.getElementById(id), node=(tag,text)=>{const e=document.createElement(tag);e.textContent=text;return e;};
 let busy=false,epoch=0,current=null,dismissed=false,loadedOwner=null;
 async function freshPasskey(){
  const who=owner(),e=epoch;if(!who)throw new Error('Sign in before approving a computer.');
  const options=await post('/auth/login/start',{});const credential=await navigator.credentials.get(requestOptions(options));
  if(e!==epoch||who!==owner())throw new Error('Account changed. Start approval again.');
  await post('/auth/login/finish',credentialJSON(credential));
  if(e!==epoch||who!==owner())throw new Error('Account changed. Start approval again.');
 }
 async function action(fn){if(busy)return;busy=true;try{await fn();}catch(e){$('sync-pairing-status').textContent=e.name==='NotAllowedError'?'Passkey confirmation cancelled.':e.message;}finally{busy=false;}}
 function reset(){epoch++;current=null;loadedOwner=null;$('sync-pairing-details').hidden=true;$('sync-pairing-list').replaceChildren();$('sync-pairing-status').textContent='';$('sync-pairing-dialog').close();}
 async function refresh(){
  if(!owner()){$('sync-pairing-status').textContent='Sign in to approve this computer.';$('sync-pairing-login').hidden=false;return;}
  $('sync-pairing-login').hidden=true;const e=epoch,who=owner();
  const vaults=await api('/sync/vaults');if(e!==epoch||who!==owner())return;
  if(!vaults.vaults.length){$('sync-pairing-status').textContent='Folder sync is not configured on this service.';return;}
  const selectedVault=$('sync-pairing-vault').value;
  $('sync-pairing-vault').replaceChildren(...vaults.vaults.map(v=>{const n=node('option',v.name);n.value=v.id;return n;}));
  if(vaults.vaults.some(v=>v.id===selectedVault))$('sync-pairing-vault').value=selectedVault;
  if(requestId){
   let p;
   try{p=await api(`/sync/requests/${encodeURIComponent(requestId)}`);}catch(error){
    if(error.status!==404)throw error;
    requestId=null;current=null;$('sync-pairing-details').hidden=true;$('sync-pairing-status').textContent='This request expired or was cancelled. Start again in Tessera.';
   }
   if(e!==epoch||who!==owner())return;
   if(p){current=p;
   $('sync-pairing-name').textContent=p.name;$('sync-pairing-device').textContent=p.device_id;$('sync-pairing-code').textContent=p.code;
   $('sync-pairing-details').hidden=false;$('sync-pairing-match').checked=false;
   if(p.vault)$('sync-pairing-vault').value=p.vault;
   $('sync-pairing-vault').disabled=p.state!=='requested';$('sync-pairing-match').disabled=p.state!=='requested';
   $('sync-pairing-approve').disabled=p.state!=='requested';$('sync-pairing-reject').disabled=!['requested','approved'].includes(p.state);
   $('sync-pairing-status').textContent=p.state==='requested'?`Request expires ${new Date(p.expires*1000).toLocaleTimeString()}.`:p.state==='cancelled'?'Request cancelled.':'Approved. Return to Tessera to finish setup.';
   if(p.state!=='requested')requestId=null;
   }
  }
  const list=await api('/sync/registrations');if(e!==epoch||who!==owner())return;
  $('sync-pairing-list').replaceChildren(...list.registrations.map(r=>{
   const row=node('li',''),info=node('div','');info.append(node('strong',r.name),node('small',vaults.vaults.find(v=>v.id===r.vault)?.name || 'Unavailable vault'),node('small',syncState(r.state)),node('small',r.device_id));if(r.last_error)info.append(node('small','Hub unavailable or configuration needs attention. Retrying.'));row.append(info);
   if(r.state!=='revoked'){const remove=node('button','Remove');remove.className='quiet';remove.onclick=()=>action(async()=>{if(!confirm(`Remove ${r.name} from this hub? Local files and copies on other computers remain. Existing sync elsewhere may continue.`))return;await freshPasskey();await post('/sync/remove',{id:r.id});await refresh();});row.append(remove);}return row;
  }));
 }
 $('sync-pairing-open').onclick=()=>{dismissed=false;action(async()=>{if(!$('sync-pairing-dialog').open)$('sync-pairing-dialog').showModal();await refresh();});};
 $('sync-pairing-close').onclick=()=>{if(!busy){dismissed=true;$('sync-pairing-dialog').close();}};
 $('sync-pairing-dialog').addEventListener('cancel',e=>{if(busy)e.preventDefault();else dismissed=true;});
 $('sync-pairing-login').onclick=()=>action(async()=>{await signIn();await refresh();});
 $('sync-pairing-approve').onclick=()=>action(async()=>{
  if(!current||!$('sync-pairing-match').checked){$('sync-pairing-status').textContent='Check that the code matches Tessera on your computer.';return;}
  const id=current.id,code=current.code,vault_id=$('sync-pairing-vault').value;
  await freshPasskey();await post('/sync/approve',{id,code,vault_id});await refresh();
 });
 $('sync-pairing-reject').onclick=()=>action(async()=>{if(!current)return;await freshPasskey();await post('/sync/cancel',{id:current.id});await refresh();});
 async function ready(){if(requestId&&!dismissed&&loadedOwner!==owner()){await action(async()=>{if(!$('sync-pairing-dialog').open)$('sync-pairing-dialog').showModal();await refresh();loadedOwner=owner();});}}
 if(requestId){$('sync-pairing-dialog').showModal();$('sync-pairing-status').textContent='Sign in to approve this computer.';}
 setInterval(()=>{if(!document.hidden&&$('sync-pairing-dialog').open&&owner()&&!busy&&current?.state!=='requested')action(refresh);},5000);
 return {reset,ready};
}
