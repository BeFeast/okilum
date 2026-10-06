import { creationOptions, requestOptions, credentialJSON } from './webauthn.js';

export function mountDevices({api,post,owner,joinToken,signIn}) {
 const $=id=>document.getElementById(id), el=(tag,text)=>{const n=document.createElement(tag);n.textContent=text;return n;};
 let epoch=0,busy=false,link='',joining=false;
 const errorText=e=>e.name==='NotAllowedError'?'Passkey request cancelled.':e.message;
 async function confirmOwner(){
  const options=await post('/auth/login/start',{});
  const credential=await navigator.credentials.get(requestOptions(options));
  await post('/auth/login/finish',credentialJSON(credential));
 }
 async function action(work){if(busy)return;busy=true;$('devices-status').textContent='';try{await work();}catch(e){$('devices-status').textContent=errorText(e);}finally{busy=false;}}
 function reset(){epoch++;link='';$('invite-link').value='';$('invite-link-row').hidden=true;$('passkey-list').replaceChildren();$('device-invitations').replaceChildren();$('devices-dialog').close();}
 async function refresh(){
  if(!owner()){reset();return;}const who=owner(),e=epoch;
  const [keys,invites]=await Promise.all([api('/passkeys'),api('/devices/invitations')]);
  if(who!==owner()||e!==epoch)return;
  $('passkey-list').replaceChildren(...keys.keys.map(key=>{
   const row=el('li',''),info=el('div','');info.append(el('strong',key.name),el('small',key.created_at?'Added '+new Date(key.created_at*1000).toLocaleDateString():'Original key · added before device settings'),el('small',`${key.current?'This passkey · ':''}${key.last_used?'Last used '+new Date(key.last_used*1000).toLocaleDateString():'Not used yet'}`));
   const revoke=el('button','Revoke');revoke.className='quiet';revoke.disabled=keys.keys.length<=1;revoke.title=keys.keys.length<=1?'Add another passkey before revoking this one':'Revoke this passkey and its sessions';
   revoke.onclick=()=>action(async()=>{if(!confirm(`Revoke ${key.name}? All synchronized copies and their sessions will stop working.`))return;await confirmOwner();const result=await post('/passkeys/revoke',{id:key.id});link='';$('invite-link-row').hidden=true;if(result.session_revoked){reset();await signIn();}else await refresh();});row.append(info,revoke);return row;
  }));
  $('device-invitations').replaceChildren(...invites.invitations.filter(i=>i.state!=='approved').map(i=>{
   const row=el('li',''),info=el('div','');info.append(el('strong',i.name||'Waiting for the new device'),el('small',i.code?`Match this code: ${i.code}`:`Expires ${new Date(i.expires*1000).toLocaleTimeString()}`));row.append(info);
   if(i.state==='confirm'){const approve=el('button','Approve');approve.onclick=()=>action(async()=>{if(!confirm(`Does ${i.code} match the code shown on your new device?`))return;await confirmOwner();await post('/devices/approve',{id:i.id,code:i.code});link='';$('invite-link-row').hidden=true;await refresh();});row.append(approve);}
   const cancel=el('button','Cancel');cancel.className='quiet';cancel.onclick=()=>action(async()=>{await post('/devices/cancel',{id:i.id});link='';$('invite-link-row').hidden=true;await refresh();});row.append(cancel);return row;
  }));
 }
 $('devices-settings').onclick=()=>{if(!owner()){signIn();return;}$('devices-dialog').showModal();refresh().catch(e=>$('devices-status').textContent=errorText(e));};
 $('devices-close').onclick=()=>{if(!busy)$('devices-dialog').close();};$('devices-dialog').addEventListener('cancel',e=>{if(busy)e.preventDefault();});
 $('add-passkey-form').onsubmit=e=>{e.preventDefault();action(async()=>{await confirmOwner();const options=await post('/passkeys/add/start',{name:$('passkey-name').value});const key=await navigator.credentials.create(creationOptions(options));await post('/passkeys/add/finish',credentialJSON(key));$('passkey-name').value='';await refresh();$('devices-status').textContent='Passkey added.';});};
 $('invite-device').onclick=()=>action(async()=>{await confirmOwner();const result=await post('/devices/invitations',{});link=`${location.origin}/#device=${encodeURIComponent(result.token)}`;$('invite-link').value=link;$('invite-link-row').hidden=false;await refresh();});
 $('copy-invite').onclick=()=>action(async()=>{if(link){await navigator.clipboard.writeText(link);$('devices-status').textContent='Link copied. Open it on your new device within five minutes.';}});
 setInterval(()=>{if(!document.hidden&&$('devices-dialog').open&&!busy)refresh().catch(e=>$('devices-status').textContent=errorText(e));},5000);
 $('join-device-form').onsubmit=async e=>{e.preventDefault();if(joining)return;joining=true;$('join-device-save').disabled=true;try{const options=await post('/devices/register/start',{token:joinToken});const key=await navigator.credentials.create(creationOptions(options));const info=await post('/devices/register/finish',{name:$('join-device-name').value,credential:credentialJSON(key)});$('join-device-form').hidden=true;$('join-device-code').textContent=info.code;$('join-device-status').textContent='Compare this code and approve on your signed-in device.';}catch(e){$('join-device-status').textContent=errorText(e);}finally{joining=false;$('join-device-save').disabled=false;}};
 $('join-device-login').onclick=async()=>{try{await signIn();if(owner()){$('join-device-dialog').close();joinToken=null;}}catch(e){$('join-device-status').textContent=errorText(e);}};
 $('join-device-close').onclick=()=>{if(!joining)$('join-device-dialog').close();};
 setInterval(async()=>{if(!joinToken||joining||document.hidden||!$('join-device-dialog').open||!$('join-device-form').hidden)return;try{const info=await post('/devices/status',{token:joinToken});if(info.state==='approved'){$('join-device-status').textContent='Device approved. Sign in with your new passkey.';$('join-device-login').hidden=false;joinToken=null;}}catch(e){$('join-device-status').textContent='This link expired or was cancelled. Ask for a new link.';joinToken=null;}},5000);
 if(joinToken)$('join-device-dialog').showModal();
 return {reset};
}
