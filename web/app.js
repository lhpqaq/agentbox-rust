import Xterm from './vendor/xterm.js';
import { stripAnsi } from './ansi-text.js';
import { createInputTransport } from './input-transport.js';
import { appendSseChunk, SseResourceLimitError } from './stream-limits.js';
import { applyAuthoritativeSnapshot, mergeEventMetadata, commandPresentation, serverHistoryMayBeIncomplete } from './replay-state.js';
import { createTerminalLinkHandler } from './link-safety.js';
import { createTerminalTransport, isLiveOutput, outputCommandId, TerminalResourceLimitError } from './terminal-input.js';
import { prepareSubmission, confirmSubmission, readSubmissionResult, mergeSubmissionResult, uncertainSubmissionMessage } from './submission-state.js';
const $ = id => document.getElementById(id);
let token = sessionStorage.getItem('agentbox-token') || '';
let sessions = [], selected = null, streamController = null, generation = 0, connected = false;
const caches = new Map();
// Keep unresolved submission identities even when transcript caches are evicted.
// This page-memory journal does not persist command text into browser storage.
const pendingSubmissionJournals = new Map();
const terminal = new Xterm.Terminal({linkHandler:createTerminalLinkHandler({confirmOpen:message=>window.confirm(message),openLink:(url,target,features)=>window.open(url,target,features),showBlocked:message=>error(message)}),fontFamily:'ui-monospace, SFMono-Regular, Consolas, monospace',fontSize:12,lineHeight:1.35,cursorBlink:true,scrollback:10000,convertEol:false,theme:{background:'#0f131b',foreground:'#d3dce5',cursor:'#8bdfb7',selectionBackground:'#375b4b',black:'#151b25',red:'#f19b91',green:'#8bdfb7',yellow:'#e5cc92',blue:'#96b9e9',magenta:'#baa9e5',cyan:'#8bd7d8',white:'#dce4ed'}});
const fit = new Xterm.FitAddon(); terminal.loadAddon(fit); terminal.open($('terminal'));
const state = () => caches.get(selected);
const session = () => sessions.find(s => s.id === selected);
const statusText = s => ({running:'运行中',interactive:'交互中',idle:'待命',closed:'已关闭'}[s] || '无会话');
const unwrapList = (data,key) => Array.isArray(data) ? data : data[key] || [];
function error(message){ $('error-banner').textContent = message; $('error-banner').hidden = !message; }
function setConnection(label, ok=false){ connected=ok; $('connection').textContent=label; $('connection').classList.toggle('online',ok); }
function lock(message='') { $('session-dialog').close(); generation++;inputTransport.cancel('工作台已锁定'); streamController?.abort(); token=''; sessionStorage.removeItem('agentbox-token'); caches.clear(); sessions=[]; selected=null; terminalTransport.reset(); $('commands').replaceChildren(); renderSessions(); renderInfo(); $('app').inert=true; $('unlock-screen').hidden=false; $('unlock-error').textContent=message; $('token').value=''; setConnection('已锁定'); }
async function api(path, options={}) {
  let response;
  try { response = await fetch('/api'+path,{...options,headers:{'Authorization':'Bearer '+token,'Content-Type':'application/json',...options.headers}}); }
  catch(e){ if(e.name==='AbortError')throw e; throw new Error('无法连接本地服务。请确认 AgentBox 服务仍在运行。'); }
  if(response.status===401){lock('令牌无效或已失效，请重新连接。');throw new Error('访问令牌无效。');}
  if(!response.ok){let data;try{data=await response.json();}catch{}const failure=new Error(typeof data?.error==='string'?data.error:typeof data?.detail==='string'?data.detail:`请求失败 (${response.status})`);failure.status=response.status;throw failure;}
  if(response.status===204)return {}; return response.json();
}
async function action(fn){try{error('');await fn();}catch(e){if(e.name!=='AbortError')error(e.message);}}
function renderSessions(){
  $('session-list').replaceChildren(); $('session-count').textContent=sessions.filter(s=>s.status!=='closed').length;
  for(const s of sessions){const button=document.createElement('button');button.className=`session-item ${s.status} ${s.id===selected?'active':''}`;button.setAttribute('aria-current',s.id===selected?'true':'false');
    const dot=document.createElement('span');dot.className='session-dot';const copy=document.createElement('span');copy.className='session-copy';const name=document.createElement('strong');name.textContent=s.name;const path=document.createElement('small');path.textContent=s.cwd||'本地 shell';copy.append(name,path);const arrow=document.createElement('span');arrow.className='session-arrow';arrow.textContent=s.id===selected?'↗':'';button.append(dot,copy,arrow);button.onclick=()=>action(()=>selectSession(s.id));$('session-list').append(button);
  }
}
function renderInfo(){const s=session(), c=state();
  $('terminal-view').querySelector('.terminal-hint span').textContent=c?.rawTruncated?'浏览器仅回放最近 2M 字符；旧输出可能清理，请及时导出。':'实时 PTY · 仅回放保留输出 · SSH / REPL';$('session-title').textContent=s?.name||'终端工作台';$('session-status').textContent=statusText(s?.status);$('session-status').className='status-chip '+(s?.status||'');
  $('detail-status').textContent=statusText(s?.status);$('detail-pid').textContent=s?.pid||'—';$('detail-count').textContent=(c?.commands.length??s?.command_count??0)+' 条命令';$('command-count').textContent=c?.commands.length??0;
  $('detail-exit').textContent=s?.exit_code??'—';$('detail-cwd').textContent=s?.cwd||'—';$('cwd-label').textContent=s?.cwd||'尚未选择工作目录';
  const running=s?.status==='running'||s?.status==='interactive', closed=!s||s.status==='closed';$('run-command').hidden=running;$('interrupt').hidden=!running;$('run-command').disabled=closed;$('command-input').disabled=closed||running;$('rename').disabled=!s;$('close-session').disabled=closed;
  $('composer-state').textContent=closed?(s?'会话已关闭 · 记录仍可查看':'选择或创建会话后执行'):s?.status==='interactive'?'交互输入中 · 请在终端中继续':running?'执行中 · 可切换交互终端输入':'持久 shell 已就绪';
  $('empty-state').hidden=!!s && !!c?.commands.length;
  if(s&&!c?.commands.length){$('empty-state').querySelector('h2').textContent=s.status==='closed'?'会话已结束':'工作空间已就绪';$('empty-state').querySelector('p').textContent=s.status==='closed'?'此会话尚无结构化执行记录。终端输出仍可在交互终端中回看。':'在下方执行命令，或切换交互终端启动你的 coding agent。会话会保留目录与环境。';$('empty-create').hidden=true;}else if(!s){$('empty-state').querySelector('h2').textContent='一个会话，持续上下文';$('empty-state').querySelector('p').textContent='创建本地 shell 会话。工作目录、环境变量和正在运行的进程，都能继续留在这里。';$('empty-create').hidden=false;}
}
function appendHighlighted(node,text,query){if(!query){node.textContent=text;return;}const lower=text.toLowerCase(),q=query.toLowerCase();let from=0,index;while((index=lower.indexOf(q,from))!==-1){node.append(document.createTextNode(text.slice(from,index)));const mark=document.createElement('mark');mark.textContent=text.slice(index,index+q.length);node.append(mark);from=index+q.length;}node.append(document.createTextNode(text.slice(from)));}
function renderCommands(){const list=$('commands'),nearBottom=list.scrollHeight-list.scrollTop-list.clientHeight<100;list.replaceChildren();const c=state();if(!c)return;const query=$('log-search').value.trim();let matches=0;
  for(const cmd of c.commands){const output=stripAnsi(c.outputs[cmd.id]||'');if(query&&!((cmd.command||'')+'\n'+output).toLowerCase().includes(query.toLowerCase()))continue;matches++;const card=document.createElement('article');card.className='command-card';const head=document.createElement('div');head.className='command-head';const dollar=document.createElement('span');dollar.className='dollar';dollar.textContent='❯';const code=document.createElement('code');code.textContent=cmd.command;const replay=document.createElement('button');replay.textContent='再次运行 ↗';replay.title='将历史命令填入输入框，确认后执行';replay.onclick=()=>fillCommand(cmd.command);head.append(dollar,code,replay);const pre=document.createElement('pre');pre.className='command-output';appendHighlighted(pre,(c.truncatedOutputs?.has(cmd.id)?'[浏览器缓存已截断或清理此输出。]\n':'')+output,query);const meta=document.createElement('div');meta.className='command-meta';const status=document.createElement('span');const presentation=commandPresentation(cmd);status.className=presentation.tone;status.textContent=presentation.label;const duration=document.createElement('span');duration.textContent=presentation.duration;const time=document.createElement('span');if(cmd.started_at){const date=new Date(typeof cmd.started_at==='number'&&cmd.started_at<1e12?cmd.started_at*1000:cmd.started_at);time.textContent=isNaN(date)?'':date.toLocaleTimeString('zh-CN',{hour12:false});}meta.append(status,duration,time);card.append(head);if(serverHistoryMayBeIncomplete(cmd,c.retainedFirstSeq)){const warning=document.createElement('p');warning.className='command-retention';warning.textContent='服务端较早输出可能已清理，回放可能不完整。';card.append(warning);}card.append(pre,meta);list.append(card);}
  if(query&&!matches){const p=document.createElement('p');p.className='recipe-note';p.textContent='没有匹配的命令或输出。';list.append(p);}if(nearBottom)list.scrollTop=list.scrollHeight;
}
function fillCommand(command){$('command-input').value=command;$('command-input').focus();}
let renderPending=false;
function scheduleRender(){if(renderPending)return;renderPending=true;setTimeout(()=>{renderPending=false;renderCommands();renderInfo();},80);}
function trimCache(c){
  c.commands.sort((a,b)=>(a.started_at||0)-(b.started_at||0));
  if(c.commands.length>500)c.commands.splice(0,c.commands.length-500);
  let size=Object.values(c.outputs).reduce((sum,text)=>sum+text.length,0);
  for(const id of Object.keys(c.outputs)){
    if(size<=2000000)break;
    size-=c.outputs[id].length;delete c.outputs[id];c.truncatedOutputs.add(id);
  }
  const retained=new Set(c.commands.map(command=>command.id));
  for(const id of c.truncatedOutputs)if(!retained.has(id))c.truncatedOutputs.delete(id);
}
function processEvent(c,event,writeContext){if(event.seq&&event.seq<=c.seq)return;c.seq=Math.max(c.seq,event.seq||0);const data=event.data||{};
  if(event.type==='output'){const text=data.text||'';c.raw+=text;if(c.raw.length>2000000){c.raw=c.raw.slice(-2000000);c.rawTruncated=true;}const commandId=outputCommandId(data,c.active);if(commandId){c.outputs[commandId]=(c.outputs[commandId]||'')+text;if(c.outputs[commandId].length>250000){c.outputs[commandId]=c.outputs[commandId].slice(-250000);c.truncatedOutputs.add(commandId);}}if(c===state()&&!terminalTransport.write(text,writeContext))throw new TerminalResourceLimitError('终端显示缓冲已清理，自动终端应答已停用；已暂停读取，请切换会话重新载入。');}
  else{mergeEventMetadata(c,sessions.find(s=>s.id===event.session_id),event,writeContext?.isLive===true);renderSessions();}
  trimCache(c);if(c===state())scheduleRender();
}
async function stream(id,c,myGeneration){let delay=700;
  while(myGeneration===generation&&token){const controller=new AbortController();streamController=controller;try{
    const detail=await api('/sessions/'+encodeURIComponent(id),{signal:controller.signal});
    if(myGeneration!==generation)return;
    applyAuthoritativeSnapshot(c,sessions.find(s=>s.id===id),detail.session||detail,detail.commands,detail);trimCache(c);renderSessions();renderInfo();renderCommands();
    const response=await fetch(`/api/sessions/${encodeURIComponent(id)}/events?after=${c.seq}`,{headers:{Authorization:'Bearer '+token},signal:controller.signal});
    if(response.status===401){lock('令牌已失效，请重新连接。');return;}if(!response.ok)throw new Error('事件流请求失败 ('+response.status+')');if(myGeneration!==generation)return;setConnection('实时连接',true);if($('error-banner').textContent.startsWith('实时连接中断'))error('');delay=700;
    const reader=response.body.getReader(),decoder=new TextDecoder();let buffer='',replayThrough=null,syncPending=true;
    while(myGeneration===generation){
      // Backpressure also waits for cached session replay before requesting more bytes.
      await terminalTransport.whenIdle({signal:controller.signal});
      const {done,value}=await reader.read();if(myGeneration!==generation)return;if(done)break;
      buffer=appendSseChunk(buffer,decoder.decode(value,{stream:true}));let split;
      while((split=buffer.indexOf('\n\n'))!==-1){
        const chunk=buffer.slice(0,split);buffer=buffer.slice(split+2);
        const payload=chunk.split('\n').filter(line=>line.startsWith('data:')).map(line=>line.slice(5).trimStart()).join('\n');
        if(!payload)continue;
        let event;try{event=JSON.parse(payload);}catch(e){error('收到无法解析的事件数据：'+e.message);continue;}
        if(event.type==='stream_sync'){
          if(syncPending){
            replayThrough=event.session_id===id&&Number.isSafeInteger(event.data?.replay_through)&&event.data.replay_through>=0?event.data.replay_through:null;
            if(replayThrough!==null&&event.data.session?.id===id){applyAuthoritativeSnapshot(c,sessions.find(s=>s.id===id),event.data.session,event.data.commands,event.data);trimCache(c);renderSessions();renderInfo();renderCommands();}
          }syncPending=false;
        }else{
          syncPending=false;
          if(event.session_id===id)processEvent(c,event,{session_id:id,generation:myGeneration,event_seq:event.seq,isLive:isLiveOutput(event.seq,replayThrough),allowDeviceReplies:isLiveOutput(event.seq,replayThrough)});
        }
        // Never retain an unbounded queue when network output outruns xterm parsing.
        await terminalTransport.whenIdle({signal:controller.signal});
        if(myGeneration!==generation)return;
      }
    }
    if(myGeneration!==generation)return;if(session()?.status==='closed'){setConnection('记录已同步',true);return;}throw new Error('事件连接中断');
  }catch(e){
    controller.abort();
    if(myGeneration!==generation||e.name==='AbortError')return;
    if(e instanceof TerminalResourceLimitError||e instanceof SseResourceLimitError){c.rawTruncated=true;setConnection('输出已暂停');error(e.message);renderInfo();return;}
    setConnection('重连中…');error('实时连接中断，正在自动重连。终端进程仍由本地服务管理。');
  }
    await new Promise(resolve=>setTimeout(resolve,delay));delay=Math.min(delay*1.7,10000);
  }
}
async function selectSession(id){generation++;streamController?.abort();const myGeneration=generation;selected=id;sessionStorage.setItem('agentbox-selected',id);$('app').classList.remove('nav-open');error('');inputTransport.cancel('会话已切换');renderSessions();terminalTransport.reset();
  let c=caches.get(id);if(!c){while(caches.size>=8)caches.delete(caches.keys().next().value);if(!pendingSubmissionJournals.has(id))pendingSubmissionJournals.set(id,new Map());c={seq:0,commands:[],outputs:{},raw:'',active:null,truncatedOutputs:new Set(),rawTruncated:false,pendingRequests:pendingSubmissionJournals.get(id)};caches.set(id,c);try{const detail=await api('/sessions/'+encodeURIComponent(id));if(myGeneration!==generation)return;applyAuthoritativeSnapshot(c,sessions.find(s=>s.id===id),detail.session||detail,detail.commands||[],detail);}catch(e){renderInfo();throw e;}}
  if(myGeneration!==generation)return;terminalTransport.write(c.raw,{session_id:id,generation:myGeneration,allowDeviceReplies:false});renderCommands();renderInfo();renderSessions();setConnection('连接中…');stream(id,c,myGeneration);fitTerminal();
}
async function connect(){const status=await api('/status');sessions=unwrapList(await api('/sessions'),'sessions');$('broker-info').textContent=`v${status.version||'1.0'} · broker ${status.broker_pid||'—'}`;$('app').inert=false;$('unlock-screen').hidden=true;sessionStorage.setItem('agentbox-token',token);renderSessions();const saved=sessionStorage.getItem('agentbox-selected');const chosen=sessions.find(s=>s.id===saved)||sessions.find(s=>s.status!=='closed')||sessions[0];if(chosen)await selectSession(chosen.id);else{renderInfo();setConnection('本地服务已连接',true);}}
function newSession(){$('session-form').querySelector('button[type="submit"]').disabled=false;$('session-error').textContent='';$('new-name').value='';$('new-cwd').value=session()?.cwd||'';$('session-dialog').showModal();$('new-name').focus();}
$('unlock-form').onsubmit=async e=>{e.preventDefault();token=$('token').value.trim();if(!token)return;$('unlock-error').textContent='';$('unlock-button').disabled=true;try{await connect();$('token').value='';}catch(e){$('unlock-error').textContent=e.message;}finally{$('unlock-button').disabled=false;}};
$('lock').onclick=()=>lock();$('new-session').onclick=newSession;$('empty-create').onclick=newSession;$('cancel-session').onclick=()=>$('session-dialog').close();$('mobile-nav').onclick=()=>$('app').classList.toggle('nav-open');
$('session-form').onsubmit=async e=>{
  e.preventDefault();const button=e.submitter,requestGeneration=generation;button.disabled=true;
  const stillCurrent=()=>requestGeneration===generation&&!!token&&!$('app').inert;
  try{
    const created=await api('/sessions',{method:'POST',body:JSON.stringify({name:$('new-name').value.trim(),cwd:$('new-cwd').value.trim()||undefined,cols:100,rows:28})});
    // Creation may already have happened; never undo/retry or revive a locked UI.
    if(!stillCurrent())return;
    const s=created.session||created;sessions.unshift(s);$('session-dialog').close();await action(()=>selectSession(s.id));
  }catch(e){if(stillCurrent())$('session-error').textContent=e.message;}
  finally{if(stillCurrent())button.disabled=false;}
};
$('rename').onclick=()=>{const s=session();if(!s)return;const name=prompt('新的会话名称',s.name);if(name?.trim())action(async()=>{const updated=await api('/sessions/'+encodeURIComponent(s.id),{method:'PATCH',body:JSON.stringify({name:name.trim()})});Object.assign(s,updated.session||updated);renderSessions();renderInfo();});};
$('close-session').onclick=()=>{const s=session();if(!s||!confirm(`关闭「${s.name}」？其 shell 和正在运行的进程将被终止，执行记录会保留。`))return;action(async()=>{await api('/sessions/'+encodeURIComponent(s.id),{method:'DELETE'});s.status='closed';renderSessions();renderInfo();});};
$('command-form').onsubmit=e=>{
  e.preventDefault();const command=$('command-input').value.trim(),s=session(),id=selected,c=state();
  if(!command||!s||s.status!=='idle'||c.submitting)return;
  let attempt;try{attempt=prepareSubmission(c,command);}catch(e){error(e.message||'无法生成请求 ID，命令未提交。');return;}
  c.submitting=true;$('run-command').disabled=true;
  action(async()=>{try{
    let confirmed;
    try{const result=await api('/sessions/'+encodeURIComponent(id)+'/commands',{method:'POST',body:JSON.stringify(attempt)});confirmed=readSubmissionResult(result,attempt);}
    catch(e){throw new Error(uncertainSubmissionMessage(e,attempt));}
    const cmd=mergeSubmissionResult(c.commands,confirmed);
    confirmSubmission(c,attempt);
    // A replayed finished/failed command is confirmation, never a new running job.
    if(cmd.status==='running'){const current=sessions.find(item=>item.id===id);if(current&&current.status!=='closed')current.status='running';}
    if(selected===id){if($('command-input').value.trim()===command)$('command-input').value='';renderCommands();}
    renderSessions();
  }finally{c.submitting=false;renderInfo();}});
};
$('command-input').onkeydown=e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing){e.preventDefault();$('command-form').requestSubmit();}};
$('interrupt').onclick=()=>action(()=>api('/sessions/'+encodeURIComponent(selected)+'/interrupt',{method:'POST',body:'{}'}));
$('log-search').oninput=renderCommands;
function showTab(raw){$('commands-view').hidden=raw;$('terminal-view').hidden=!raw;$('tab-commands').classList.toggle('active',!raw);$('tab-terminal').classList.toggle('active',raw);$('tab-commands').setAttribute('aria-selected',String(!raw));$('tab-terminal').setAttribute('aria-selected',String(raw));if(raw){requestAnimationFrame(()=>{fitTerminal();terminal.focus();});}}
$('tab-commands').onclick=()=>showTab(false);$('tab-terminal').onclick=()=>showTab(true);$('focus-terminal').onclick=()=>terminal.focus();
const inputTransport=createInputTransport({
  isCurrent:context=>context.session_id===selected&&context.generation===generation&&!!token&&!$('app').inert&&session()?.status!=='closed',
  send:(item,{signal})=>api('/sessions/'+encodeURIComponent(item.context.session_id)+'/input',{method:'POST',signal,body:JSON.stringify(item.userInput?{data:item.data}:{data:item.data,source:'terminal_reply',event_seq:item.context.event_seq})}),
  onError:message=>error(message),onDeviceDrop:message=>error(message)
});
function enqueueTerminalInput(data,context,userInput){
  if(inputTransport.enqueue(data,context,userInput)&&userInput&&session()?.status==='idle'){session().status='interactive';renderInfo();renderSessions();}
}
const terminalTransport=createTerminalTransport(terminal,{
  sendUserInput:data=>enqueueTerminalInput(data,{session_id:selected,generation},true),
  sendDeviceReply:()=>{}, // Rust edition intentionally disables automatic terminal device replies.
  isContextCurrent:context=>context.session_id===selected&&context.generation===generation&&!!token&&!$('app').inert,
  onError:e=>error('终端显示错误：'+e.message)
});
let sizeTimer;function fitTerminal(){if($('terminal-view').hidden||!selected)return;try{fit.fit();clearTimeout(sizeTimer);const id=selected;sizeTimer=setTimeout(()=>action(()=>api('/sessions/'+encodeURIComponent(id)+'/resize',{method:'POST',body:JSON.stringify({cols:terminal.cols,rows:terminal.rows})})),150);}catch{}}
new ResizeObserver(fitTerminal).observe($('terminal'));window.addEventListener('resize',fitTerminal);
const recipes={state:'printf "Directory: "; pwd\nprintf "Shell PID: %s\\n" "$$"\nprintf "AGENTBOX_DEMO=%s\\n" "${AGENTBOX_DEMO:-not set}"',git:'git status --short --branch && git log -5 --oneline',test:'if [ -f package.json ]; then npm test; elif [ -f Cargo.toml ]; then cargo test; elif [ -f pyproject.toml ] || [ -d tests ]; then if python3 -c \"import pytest\" >/dev/null 2>&1; then python3 -m pytest; else python3 -m unittest discover -s tests -v; fi; elif [ -f Makefile ]; then make test; else printf "No supported test configuration found.\\n"; fi',build:'if [ -f package.json ]; then npm run build; elif [ -f Cargo.toml ]; then cargo build; elif [ -f Makefile ]; then make; else printf "No supported build configuration found.\\n"; fi'};
document.querySelectorAll('[data-recipe]').forEach(button=>button.onclick=()=>{if(!session()){newSession();return;}showTab(false);fillCommand(recipes[button.dataset.recipe]);});
document.addEventListener('keydown',e=>{if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==='k'&&!$('app').inert){e.preventDefault();newSession();}if(e.ctrlKey&&e.key==='c'&&session()?.status==='running'&&e.target===$('command-input')){e.preventDefault();$('interrupt').click();}});
if(token){connect().catch(e=>{lock(e.message);});}else{$('token').focus();}

// Reflect sessions started by another client without executing anything.
let refreshing=false;
setInterval(async()=>{if(!token||$('app').inert||document.hidden||refreshing)return;refreshing=true;const stamp=generation;try{const fresh=unwrapList(await api('/sessions'),'sessions');if(stamp!==generation||!token)return;sessions=fresh;renderSessions();renderInfo();}catch(e){if(token&&e.name!=='AbortError')setConnection('服务连接异常');}finally{refreshing=false;}},5000);
