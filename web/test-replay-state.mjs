import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source=await fs.readFile(new URL('./replay-state.js',import.meta.url),'utf8');
const {applyAuthoritativeSnapshot,mergeEventMetadata,commandPresentation,serverHistoryMayBeIncomplete}=await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
const session={id:'s',status:'closed',cwd:'/current',exit_code:0};
const cache={commands:[],raw:'retained raw output',outputs:{c:'retained command output'}};
const finished={id:'c',command:'pwd',status:'finished',exit_code:0,cwd:'/current',finished_at:20};
applyAuthoritativeSnapshot(cache,session,{...session},[finished]);
const event=(type,data)=>({type,session_id:'s',data});
mergeEventMetadata(cache,session,event('command_started',{...finished,status:'running',cwd:'/old',finished_at:null}),false);
assert.equal(cache.commands[0].status,'finished');
assert.equal(cache.commands[0].finished_at,20);
assert.equal(session.status,'closed');
assert.equal(session.cwd,'/current');
mergeEventMetadata(cache,session,event('session_updated',{id:'s',status:'idle',cwd:'/old'}),false);
assert.deepEqual(session,{id:'s',status:'closed',cwd:'/current',exit_code:0});
mergeEventMetadata(cache,session,event('command_finished',{...finished,status:'failed',exit_code:1,cwd:'/old'}),false);
assert.equal(cache.commands[0].status,'finished','Authoritative terminal metadata outranks old terminal events too');
assert.equal(cache.raw,'retained raw output');assert.equal(cache.outputs.c,'retained command output');
// Crash recovery metadata without a matching finish event must remain authoritative.
applyAuthoritativeSnapshot(cache,session,{id:'s',status:'closed',cwd:'/recovered'},[{...finished,status:'lost',exit_code:null}]);
mergeEventMetadata(cache,session,event('command_started',{...finished,status:'running'}),false);
assert.equal(cache.commands[0].status,'lost');assert.equal(session.status,'closed');
// Incomplete old history must not fabricate a currently running job.
mergeEventMetadata(cache,session,event('command_started',{id:'older',command:'sleep 5',status:'running'}),false);
assert.equal(cache.commands.find(c=>c.id==='older').status,'unknown');
mergeEventMetadata(cache,session,event('command_finished',{id:'older',command:'sleep 5',status:'finished',exit_code:0}),false);
assert.equal(cache.commands.find(c=>c.id==='older').status,'finished');
// Live updates remain authoritative, not globally suppressed.
mergeEventMetadata(cache,session,event('session_updated',{id:'s',status:'idle',cwd:'/live'}),true);
assert.equal(session.status,'idle');assert.equal(session.cwd,'/live');
mergeEventMetadata(cache,session,event('command_started',{id:'live',command:'echo live',status:'running',cwd:'/live'}),true);
assert.equal(session.status,'running');assert.equal(cache.commands.find(c=>c.id==='live').status,'running');
mergeEventMetadata(cache,session,event('command_finished',{id:'live',status:'finished',exit_code:7,cwd:'/after'}),true);
assert.equal(session.status,'idle');assert.equal(session.cwd,'/after');assert.equal(session.exit_code,7);
mergeEventMetadata(cache,session,event('session_closed',{}),true);assert.equal(session.status,'closed');
// Reconnect snapshots repair metadata even if the corresponding event was never written.
applyAuthoritativeSnapshot(cache,session,{id:'s',status:'closed',cwd:'/restart'},[{id:'live',command:'echo live',status:'lost',exit_code:null}]);
assert.equal(cache.commands.find(c=>c.id==='live').status,'lost');assert.equal(session.cwd,'/restart');
assert.equal(cache.raw,'retained raw output');assert.equal(cache.outputs.c,'retained command output');
const app=await fs.readFile(new URL('./app.js',import.meta.url),'utf8');
assert.ok(app.includes("const detail=await api('/sessions/'+encodeURIComponent(id),{signal:controller.signal})"));
assert.ok(app.includes('event.data.session,event.data.commands'));
assert.ok(app.includes('if(c===state()&&!terminalTransport.write(text,writeContext))'),'Replay output still reaches the terminal regardless of metadata policy');
console.log('PASS: replay cannot roll back finished/lost commands or closed sessions; missing historical status remains unknown; live updates apply; reconnect snapshots repair metadata without clearing output.');

for (const finalStatus of ['exited', 'lost']) {
  const liveSession={id:'s',status:'running',cwd:'/work'};
  const history={commands:[{id:'active',status:'running'}],active:'active'};
  mergeEventMetadata(history,liveSession,event('command_finished',{id:'active',status:finalStatus,exit_code:null}),true);
  assert.equal(liveSession.status,'closed',`${finalStatus} must not make a dead shell look idle`);
  mergeEventMetadata(history,liveSession,event('command_finished',{id:'prior',status:'finished',exit_code:0}),true);
  assert.equal(liveSession.status,'closed','Later command lifecycle events must not reopen a closed session');
}
for (const status of ['unknown','lost','failed','finished','exited','interrupted','unrecognized']) {
  const view=commandPresentation({status,duration_ms:null,exit_code:null});
  assert.equal(view.duration,'时长未知');
  assert.ok(!view.label.includes('RUNNING'));
  assert.ok(!view.label.includes('EXIT —'),'Do not invent an exit result when it is unknown');
}
assert.equal(commandPresentation({status:'running',duration_ms:null}).duration,'执行中');
assert.equal(commandPresentation({status:'finished',duration_ms:0,exit_code:0}).duration,'0.00s');
assert.equal(commandPresentation({status:'finished',duration_ms:1234,exit_code:0}).duration,'1.23s');
for (const duration_ms of [-1,NaN,Infinity,'12'])assert.equal(commandPresentation({status:'lost',duration_ms}).duration,'时长未知');
assert.match(commandPresentation({status:'lost'}).label,/丢失/);
assert.match(commandPresentation({status:'failed'}).label,/失败/);
assert.match(commandPresentation({status:'exited'}).label,/Shell 已退出/);
assert.match(commandPresentation({status:'interrupted',exit_code:130}).label,/已中断.*130/);
assert.equal(commandPresentation({status:'finished',exit_code:0}).tone,'success');
assert.equal(commandPresentation({status:'finished',exit_code:1}).tone,'failed');
console.log('PASS: exited/lost completion closes sessions; null/invalid duration is unknown unless running; failure, interruption, shell exit and lost-state labels remain distinct.');

assert.equal(serverHistoryMayBeIncomplete({start_seq:10},11),true);
assert.equal(serverHistoryMayBeIncomplete({start_seq:10},10),false);
assert.equal(serverHistoryMayBeIncomplete({start_seq:10},9),false);
assert.equal(serverHistoryMayBeIncomplete({start_seq:10},0),true,'Zero cursor means no session events remain');
for(const first of [undefined,null,-1,'11',NaN,Infinity])assert.equal(serverHistoryMayBeIncomplete({start_seq:10},first),false);
for(const start_seq of [undefined,null,0,-1,'10',NaN,Infinity])assert.equal(serverHistoryMayBeIncomplete({start_seq},11),false);
const retentionCache={commands:[],outputs:{},truncatedOutputs:new Set()},retentionSession={id:'retained',status:'closed'};
applyAuthoritativeSnapshot(retentionCache,retentionSession,{id:'retained',status:'closed',first_seq:11},[{id:'old',start_seq:10,status:'finished'}]);
assert.equal(retentionCache.retainedFirstSeq,11);
assert.equal(serverHistoryMayBeIncomplete(retentionCache.commands[0],retentionCache.retainedFirstSeq),true);
assert.equal(retentionCache.truncatedOutputs.size,0,'Server retention must not be mislabeled browser truncation');
applyAuthoritativeSnapshot(retentionCache,retentionSession,{id:'retained',status:'closed'},[],{first_seq:20,replay_through:30});
assert.equal(retentionCache.retainedFirstSeq,20,'Atomic stream sync bounds supersede preliminary detail');
applyAuthoritativeSnapshot(retentionCache,retentionSession,{id:'other',status:'closed'},[],{first_seq:99});
assert.equal(retentionCache.retainedFirstSeq,20,'Only same-session bounds may affect retention warning');
assert.ok(app.includes('serverHistoryMayBeIncomplete(cmd,c.retainedFirstSeq)'));
assert.ok(app.includes('服务端较早输出可能已清理，回放可能不完整。'));
console.log('PASS: server retention warnings use valid same-session start/first sequence boundaries, handle empty retained history, stay separate from browser truncation, and use one bounded scalar per cache.');
