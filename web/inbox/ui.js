const paths = {
 inbox:'M4 4h16v16H4z M4 14h5l2 3h2l2-3h5',
 questions:'M8 8a4 4 0 1 1 6 3.5c-2 1-2 1.5-2 3 M12 18h.01',
 projects:'M3 6h7l2 2h9v12H3z',
 overview:'M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z',
 plus:'M12 5v14 M5 12h14', refresh:'M20 7v5h-5 M4 17v-5h5 M6 7a7 7 0 0 1 12-1l2 6 M4 12l2 6a7 7 0 0 0 12-1',
 close:'M6 6l12 12 M6 18L18 6', theme:'M20 14A8 8 0 0 1 10 4a8 8 0 1 0 10 10',
 arrow:'M7 17L17 7 M7 7h10v10', back:'M19 12H5 M11 6l-6 6 6 6',
 issue:'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18',
 pull:'M6 7v10 M9 4a3 3 0 1 1-6 0 3 3 0 0 1 6 0 M9 20a3 3 0 1 1-6 0 3 3 0 0 1 6 0 M15 4h1a3 3 0 0 1 3 3v10 M22 20a3 3 0 1 1-6 0 3 3 0 0 1 6 0',
 release:'M12 3l9 5v9l-9 5-9-5V8z M3 8l9 5 9-5 M12 13v9',
 search:'M10 3a7 7 0 1 0 0 14 7 7 0 0 0 0-14 M15 15l6 6',
};
export function icon(name){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');svg.classList.add('icon');const p=document.createElementNS(svg.namespaceURI,'path');p.setAttribute('d',paths[name]||paths.issue);svg.append(p);return svg;}
export function glyph(button,name,label){button.replaceChildren(icon(name));button.setAttribute('aria-label',label);button.title=label;button.classList.add('icon-button');}
export function age(value,now=Date.now()){const time=typeof value==='number'?value*1000:Date.parse(value);if(!Number.isFinite(time))return '—';const days=Math.max(0,Math.floor((now-time)/86400000));return days===0?'Today':days===1?'1d':days<30?`${days}d`:new Date(time).toLocaleDateString(undefined,{month:'short',day:'numeric'});}
export function mountShell(){
 const $=id=>document.getElementById(id);
 function route(view){if(!['inbox','questions','projects','overview'].includes(view))view='inbox';document.body.dataset.view=view;window.scrollTo({top:0});document.title=`${view[0].toUpperCase()+view.slice(1)} · Tessera`;document.querySelectorAll('[data-nav]').forEach(b=>{b.setAttribute('aria-current',b.dataset.nav===view?'page':'false');});document.querySelectorAll('[data-screen]').forEach(s=>s.hidden=s.dataset.screen!==view);$('screen-title').textContent={inbox:'Inbox',questions:'Questions',projects:'Projects',overview:'Overview'}[view];}
 document.querySelectorAll('[data-nav]').forEach(b=>{b.prepend(icon(b.dataset.nav));b.onclick=()=>{history.replaceState(null,'',`?view=${b.dataset.nav}`);route(b.dataset.nav);};});
 route(new URL(location.href).searchParams.get('view'));
 for(const [id,name,label] of [['theme-toggle','theme','Switch color theme'],['project-refresh','refresh','Refresh project'],['executor-refresh','refresh','Refresh questions'],['forgejo-refresh','refresh','Refresh overview'],['launch-refresh','refresh','Refresh execution'],['close-detail','back','Back to Inbox'],['executor-close','back','Back to questions'],['overview-close','back','Back to overview']])glyph($(id),name,label);
 for(const id of ['project-select','executor-project','forgejo-project','launch-project'])projectChoices($(id));
 const preferred=()=>matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light';let theme;try{theme=localStorage.getItem('tessera-theme');}catch{}document.documentElement.dataset.theme=theme||preferred();
 const syncIcons=()=>{const dark=document.documentElement.dataset.theme==='dark';document.querySelector('.brand img').src=dark?'/icon-dark.svg':'/icon.svg';document.querySelectorAll('link[rel=icon]').forEach(link=>{link.media='all';link.href=dark?'/icon-dark.svg':'/icon.svg';});};syncIcons();
 $('theme-toggle').onclick=()=>{const t=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=t;syncIcons();try{localStorage.setItem('tessera-theme',t);}catch{}};
 $('new-thought').prepend(icon('plus'));$('new-thought').onclick=()=>{route('inbox');$('capture-compose').open=true;$('thought').focus();};
 document.querySelectorAll('dialog.drawer').forEach(dialog=>{dialog.addEventListener('click',e=>{if(e.target===dialog){const r=dialog.getBoundingClientRect();if(e.clientX<r.left||e.clientX>r.right||e.clientY<r.top||e.clientY>r.bottom)dialog.close();}});});
}

// Keep the existing guarded controllers as the selection authority. Only the
// presentation changes: named project buttons replace the native select popup.
export function projectChoices(select){
 const group=document.createElement('div');group.className='project-choices';group.setAttribute('role','group');
 const label=document.querySelector(`label[for="${select.id}"]`);group.setAttribute('aria-label',label?.textContent||'Projects');
 if(label)label.hidden=true;select.hidden=true;select.before(group);
 function render(){
  const focused=group.contains(document.activeElement)?document.activeElement.dataset.value:null;
  group.replaceChildren(...[...select.options].map(option=>{
   const button=document.createElement('button');button.type='button';button.textContent=option.textContent;button.dataset.value=option.value;
   button.setAttribute('aria-pressed',String(option.value===select.value));button.disabled=select.disabled||option.disabled;
   button.onclick=()=>{if(select.disabled)return;select.value=option.value;select.dispatchEvent(new Event('change',{bubbles:true}));render();};return button;
  }));
  if(focused!==null)[...group.children].find(button=>button.dataset.value===focused)?.focus({preventScroll:true});
 }
 new MutationObserver(render).observe(select,{childList:true,subtree:true,attributes:true,attributeFilter:['disabled','selected']});
 select.addEventListener('change',render);render();return group;
}
