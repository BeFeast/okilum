import { requestOptions, credentialJSON } from './webauthn.js';

export const formatMatchingCode = code => code.match(/.{1,4}/g)?.join(' ') || '';
export const syncState = state => ({provisioning:'Connecting to the vault',hub_ready:'Hub connected · finish setup in Tessera',removal_pending:'Removal pending · hub may still sync',revoked:'Removed from this hub'}[state] || 'Checking connection');
export function mountSyncPairing({api,post,owner,requestId,signIn}) {
 const $=id=>document.getElementById(id), node=(tag,text)=>{const e=document.createElement(tag);e.textContent=text;return e;};
 let busy=false,epoch=0,current=null,dismissed=false,loadedOwner=null,view=requestId?'approval':'devices';
 async function freshPasskey(){
  const who=owner(),e=epoch;if(!who)throw new Error('Sign in before approving a computer.');
  const options=await post('/auth/login/start',{});const credential=await navigator.credentials.get(requestOptions(options));
  if(e!==epoch||who!==owner())throw new Error('Account changed. Start approval again.');
  await post('/auth/login/finish',credentialJSON(credential));
  if(e!==epoch||who!==owner())throw new Error('Account changed. Start approval again.');
 }
 async function action(fn){if(busy)return;busy=true;try{await fn();}catch(e){$('sync-pairing-status').textContent=e.name==='NotAllowedError'?'Passkey confirmation cancelled.':e.message;}finally{busy=false;}}
 function reset(){epoch++;current=null;loadedOwner=null;$('sync-pairing-details').hidden=true;$('sync-pairing-list').replaceChildren();$('sync-pairing-pending').replaceChildren();$('sync-pairing-removed-list').replaceChildren();$('sync-pairing-removed').open=false;$('sync-pairing-status').textContent='';$('sync-pairing-dialog').close();}
 function present(mode){
  view=mode;$('sync-pairing-all').hidden=mode==='devices';
  $('sync-pairing-title').textContent=mode==='approval'?'Add this computer':'Folder sync computers';
  $('sync-pairing-intro').hidden=mode!=='approval';
  $('sync-pairing-computers').hidden=mode==='approval';
  if(mode==='devices'){$('sync-pairing-details').hidden=true;$('sync-pairing-status').textContent='';}
  if($('devices-dialog')?.open)$('devices-dialog').close();
  if($('mobile-settings'))$('mobile-settings').open=false;
  if(!$('sync-pairing-dialog').open)$('sync-pairing-dialog').showModal();
 }
 async function refresh(){
  if(!owner()){$('sync-pairing-status').textContent='Sign in to approve this computer.';$('sync-pairing-login').hidden=false;return;}
  $('sync-pairing-login').hidden=true;const e=epoch,who=owner();
  const vaults=await api('/sync/vaults');if(e!==epoch||who!==owner())return;
  if(!vaults.vaults.length){$('sync-pairing-status').textContent='Folder sync is not configured on this service.';return;}
  const selectedVault=$('sync-pairing-vault').value;
  $('sync-pairing-vault').replaceChildren(...vaults.vaults.map(v=>{const n=node('option',v.name);n.value=v.id;return n;}));
  if(vaults.vaults.some(v=>v.id===selectedVault))$('sync-pairing-vault').value=selectedVault;
  if(requestId&&view==='approval'){
   let p;
   try{p=await api(`/sync/requests/${encodeURIComponent(requestId)}`);}catch(error){
    if(error.status!==404)throw error;
    requestId=null;current=null;$('sync-pairing-computers').hidden=false;$('sync-pairing-details').hidden=true;$('sync-pairing-status').textContent='This request expired or was cancelled. Start again in Tessera.';
   }
   if(e!==epoch||who!==owner())return;
   if(p){
   $('sync-pairing-name').textContent=p.name;$('sync-pairing-device').textContent=p.device_id;$('sync-pairing-code').textContent=formatMatchingCode(p.code);
   $('sync-pairing-details').hidden=false;if(current?.id!==p.id||current?.state!==p.state)$('sync-pairing-match').checked=false;
   if(p.vault)$('sync-pairing-vault').value=p.vault;
   $('sync-pairing-vault').disabled=p.state!=='requested';$('sync-pairing-match').disabled=p.state!=='requested';
   $('sync-pairing-approve').disabled=p.state!=='requested';$('sync-pairing-reject').disabled=!['requested','approved'].includes(p.state);
   $('sync-pairing-status').textContent=p.state==='requested'?`Request expires ${new Date(p.expires*1000).toLocaleTimeString()}.`:p.state==='cancelled'?'Request cancelled.':'Approved. Return to Tessera to finish setup.';
   current=p;
   }
  }
  const list=await api('/sync/registrations');if(e!==epoch||who!==owner())return;
  $('sync-pairing-pending').replaceChildren(...(list.pending||[]).map(p=>{
   const row=node('li',''),info=node('div','');info.append(node('strong',p.name),node('small',p.state==='approved'?'Approved · waiting for Tessera':'Waiting for approval'),node('small',p.device_id));row.append(info);
   const review=node('button','Review');review.className='quiet';review.onclick=()=>action(async()=>{requestId=p.id;current=null;present('approval');await refresh();});row.append(review);return row;
  }));
  $('sync-pairing-pending-empty').hidden=!!list.pending?.length;
  const connected=list.registrations.filter(r=>r.state!=='revoked'),removed=list.registrations.filter(r=>r.state==='revoked');
  $('sync-pairing-list-empty').hidden=!!connected.length;
  $('sync-pairing-removed').hidden=!removed.length;
  const registrationRow=r=>{
   const row=node('li',''),info=node('div','');info.append(node('strong',r.name),node('small',vaults.vaults.find(v=>v.id===r.vault)?.name || 'Unavailable vault'),node('small',syncState(r.state)),node('small',r.device_id));if(r.last_error)info.append(node('small','Hub unavailable or configuration needs attention. Retrying.'));row.append(info);
   if(r.state!=='revoked'){const remove=node('button','Remove');remove.className='quiet';remove.onclick=()=>action(async()=>{if(!confirm(`Remove ${r.name} from this hub? Local files and copies on other computers remain. Existing sync elsewhere may continue.`))return;await freshPasskey();await post('/sync/remove',{id:r.id});await refresh();});row.append(remove);}return row;
  };
  $('sync-pairing-list').replaceChildren(...connected.map(registrationRow));
  $('sync-pairing-removed-list').replaceChildren(...removed.map(registrationRow));
 }
 $('sync-pairing-open').onclick=()=>{dismissed=false;return action(async()=>{present('devices');await refresh();});};
 $('sync-pairing-all').onclick=()=>action(async()=>{present('devices');await refresh();});
 $('sync-pairing-close').onclick=()=>{if(!busy){dismissed=true;$('sync-pairing-dialog').close();}};
 $('sync-pairing-dialog').addEventListener('cancel',e=>{if(busy)e.preventDefault();else dismissed=true;});
 $('sync-pairing-login').onclick=()=>action(async()=>{await signIn();await refresh();});
 $('sync-pairing-approve').onclick=()=>action(async()=>{
  if(!current||!$('sync-pairing-match').checked){$('sync-pairing-status').textContent='Check that the code matches Tessera on your computer.';return;}
  const id=current.id,code=current.code,vault_id=$('sync-pairing-vault').value;
  await freshPasskey();await post('/sync/approve',{id,code,vault_id});await refresh();
 });
 $('sync-pairing-reject').onclick=()=>action(async()=>{if(!current)return;await freshPasskey();await post('/sync/cancel',{id:current.id});await refresh();});
 async function ready(){if(requestId&&!dismissed&&loadedOwner!==owner()){await action(async()=>{present('approval');await refresh();loadedOwner=owner();});}}
 if(requestId){present('approval');$('sync-pairing-status').textContent='Sign in to approve this computer.';}
 setInterval(()=>{if(!document.hidden&&$('sync-pairing-dialog').open&&owner()&&!busy)action(refresh);},5000);
 return {reset,ready};
}
