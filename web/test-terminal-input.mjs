import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source = await fs.readFile(new URL('./terminal-input.js', import.meta.url), 'utf8');
const { observeUserTerminalInput, outputCommandId, createTerminalTransport, isLiveOutput, TERMINAL_LIMITS, TerminalResourceLimitError } = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
const listeners = new Set();
const emit = data => { for (const listener of listeners) listener(data); };
const core = { triggerDataEvent(data) { emit(data); } };
const terminal = {
  _core: { coreService: core },
  onData(listener) { listeners.add(listener); return { dispose() { listeners.delete(listener); } }; }
};
const forwarded = [];
const original = core.triggerDataEvent;
const subscription = observeUserTerminalInput(terminal, data => forwarded.push(data));
// Historical output can cause these synchronous or asynchronous device replies.
for (const reply of ['\x1b[1;1R', '\x1b[?1;2c', '\x1b[>0;276;0c', '\x1b]10;rgb:ffff/ffff/ffff\x1b\\']) {
  core.triggerDataEvent(reply);
  await Promise.resolve().then(() => core.triggerDataEvent(reply, false));
}
assert.deepEqual(forwarded, [], 'Historical queries must never send new shell input');
// Real user input can contain identical bytes; provenance, not regex, distinguishes it.
for (const data of ['ls\r', '粘贴文本', '\x03', '\x1b[1;1R']) core.triggerDataEvent(data, true);
assert.deepEqual(forwarded, ['ls\r', '粘贴文本', '\x03', '\x1b[1;1R']);
emit('unmarked');
assert.equal(forwarded.length, 4, 'Unmarked generic onData must be ignored');
subscription.dispose();
assert.equal(core.triggerDataEvent, original);
assert.throws(() => observeUserTerminalInput({ onData() {} }, () => {}), /安全接口/);
// Retention can remove command_started; retained output still has authoritative association.
assert.equal(outputCommandId({ text: 'retained', command_id: 'cmd-42' }, null), 'cmd-42');
assert.equal(outputCommandId({ text: 'old command', command_id: 'cmd-41' }, 'cmd-42'), 'cmd-41');
assert.equal(outputCommandId({ text: 'prompt', command_id: null }, 'cmd-42'), null);
assert.equal(outputCommandId({ text: 'legacy' }, 'cmd-42'), 'cmd-42');
// Pin contract checks to the actual local bundle, so vendor updates require review.
const vendor = await fs.readFile(new URL('./vendor/xterm.js', import.meta.url), 'utf8');
assert.ok(vendor.includes('triggerDataEvent(e,t=!1)'), 'Pinned xterm provenance signature changed');
assert.ok(vendor.includes('t&&this._onUserInput.fire()'), 'Pinned xterm user-input marker changed');
assert.ok(vendor.includes('this._onData.fire(e)'), 'Pinned xterm synchronous dispatch changed');
console.log('PASS: replay replies blocked; genuine keyboard/paste/IME provenance accepted; explicit output associations survive retention; pinned vendor contract verified.');
// Exercise the real vendored parser without a browser, DOM, network or sockets.
globalThis.self = globalThis;
const { default: Xterm } = await import('data:text/javascript;base64,' + Buffer.from(vendor).toString('base64'));
const realTerminal = new Xterm.Terminal();
const rawReplies = [], safeInput = [];
realTerminal.onData(data => rawReplies.push(data));
const realSubscription = observeUserTerminalInput(realTerminal, data => safeInput.push(data));
await new Promise(resolve => realTerminal.write('\x1b[6n\x1b[c\x1b[>c\x1b[5n', resolve));
assert.ok(rawReplies.length >= 4, 'Real xterm must actually generate query replies for this test');
assert.deepEqual(safeInput, [], 'Real parser-generated replies must never reach shell input');
realTerminal._core.coreService.triggerDataEvent('user input\r', true);
assert.deepEqual(safeInput, ['user input\r']);
realSubscription.dispose();
realTerminal.dispose();
console.log('PASS: actual vendored xterm asynchronously emitted CPR/DA/status replies; all were blocked while explicit user input passed.');

// Serialized mixed replay/live writes, using the actual asynchronous parser.
let currentSession = 'one', currentGeneration = 1;
const queuedTerminal = new Xterm.Terminal();
const automatic = [], human = [], allParserReplies = [];
queuedTerminal.onData(data => allParserReplies.push(data));
const transport = createTerminalTransport(queuedTerminal, {
  sendUserInput: data => human.push(data),
  sendDeviceReply: (data, context) => automatic.push({data, ...context}),
  isContextCurrent: context => context.session_id === currentSession && context.generation === currentGeneration,
  onError: error => { throw error; }
});
const context = live => ({session_id: currentSession, generation: currentGeneration, event_seq: 42, allowDeviceReplies: live});
transport.write('\x1b[6n', context(false));
transport.write('\x1b[c', context(true));
transport.write('\x1b[5n', context(false));
transport.write('\x1b[>c', context(true));
queuedTerminal._core.coreService.triggerDataEvent('keyboard while replay parses', true);
assert.deepEqual(human, ['keyboard while replay parses']);
await transport.whenIdle();
assert.equal(automatic.length, 2, 'Only the two live writes can respond');
assert.ok(automatic[0].data.endsWith('c') && automatic[1].data.startsWith('\x1b[>'), 'Live responses preserve FIFO order');
assert.ok(allParserReplies.length >= 5, 'Both replay and live queries were actually parsed');
queuedTerminal._core.coreService.triggerDataEvent('unsolicited automatic reply');
assert.equal(automatic.length, 2, 'Automatic replies outside a write have no permission');

// Consume xterm's user-input fast path so the next write parses asynchronously.
transport.write(' ', context(false));
await transport.whenIdle();
// Reset invalidates in-flight permission before its async parser gets a turn.
automatic.length = 0;
transport.write('OLD\x1b[6n', context(true));
transport.write('STALE QUEUED OUTPUT\x1b[c', context(true));
currentSession = 'two'; currentGeneration++;
transport.reset();
transport.write('NEW\x1b[6n', context(true));
queuedTerminal._core.coreService.triggerDataEvent('new session user input', true);
await transport.whenIdle();
assert.equal(automatic.length, 1, 'Old in-flight and queued device responses must be discarded');
assert.equal(automatic[0].session_id, 'two');
assert.equal(automatic[0].generation, 2);
assert.equal(automatic[0].event_seq, 42, 'Associated output sequence must survive the asynchronous write');
assert.equal(queuedTerminal.buffer.active.getLine(0).translateToString(true), 'NEW', 'Old output must not survive the serialized reset');
assert.equal(human.at(-1), 'new session user input');

// Consume the last explicit user input fast path before the invalidation check.
transport.write(' ', context(false));
await transport.whenIdle();
// A generation change alone revokes both in-flight and queued reply contexts.
automatic.length = 0;
transport.write('\x1b[6n', context(true));
transport.write('\x1b[c', context(true));
currentGeneration++;
await transport.whenIdle();
assert.equal(automatic.length, 0);
assert.equal(isLiveOutput(11, 10), true);
for (const watermark of [null, undefined, -1, '10', NaN]) assert.equal(isLiveOutput(11, watermark), false);
assert.equal(isLiveOutput(10, 10), false);
assert.equal(isLiveOutput(9, 10), false);
transport.write('\x1b[6n', {...context(true), allowDeviceReplies: isLiveOutput(11, null)});
await transport.whenIdle();
assert.equal(automatic.length, 0, 'Missing stream_sync must fail closed');
transport.dispose();
queuedTerminal.dispose();
console.log('PASS: live replies allowed; replay denied; mixed FIFO parsing, session reset, generation invalidation, missing watermark and independent user input verified against real xterm.');

// A deliberately stalled parser proves hard queue limits independently of browsers.
function delayedTerminal() {
  const callbacks=[],listeners=new Set(),writes=[];
  const terminal={
    _core:{coreService:{triggerDataEvent(data){for(const listener of listeners)listener(data);}}},
    onData(listener){listeners.add(listener);return{dispose(){listeners.delete(listener);}};},
    write(text,done){writes.push(text);callbacks.push(done);},reset(){terminal.resets++;},resets:0,
    flush(){assert.ok(callbacks.length);callbacks.shift()();},callbacks,writes
  };return terminal;
}
const stalled=delayedTerminal(),notices=[],allowed=[],typed=[];
const bounded=createTerminalTransport(stalled,{
 sendUserInput:data=>typed.push(data),sendDeviceReply:data=>allowed.push(data),
 isContextCurrent:()=>true,onError:error=>notices.push(error)
});
const liveContext={session_id:'bounded',generation:1,allowDeviceReplies:true};
for(let index=0;index<TERMINAL_LIMITS.pendingWrites;index++)assert.equal(bounded.write('x',liveContext),true);
assert.equal(bounded.stats().pendingWrites,64);
assert.equal(bounded.write('overflow',liveContext),false);
assert.ok(notices[0] instanceof TerminalResourceLimitError);
assert.equal(bounded.stats().pendingWrites,1,'Overflow releases queued strings but counts the parser-owned string');
assert.equal(bounded.stats().repliesRevoked,true);
stalled._core.coreService.triggerDataEvent('old reply');
stalled._core.coreService.triggerDataEvent('typed during stall',true);
assert.deepEqual(allowed,[]);assert.deepEqual(typed,['typed during stall']);
stalled.flush();assert.equal(stalled.resets,1);assert.equal(bounded.stats().pendingChars,0);
bounded.write('next live output',liveContext);stalled._core.coreService.triggerDataEvent('reply after truncation');
assert.deepEqual(allowed,[],'Reply permission stays revoked after dropping display context');stalled.flush();
bounded.reset();bounded.write('fresh context',liveContext);stalled._core.coreService.triggerDataEvent('fresh reply');
assert.deepEqual(allowed,['fresh reply']);stalled.flush();
const maximum='x'.repeat(TERMINAL_LIMITS.writeChars);
assert.equal(bounded.write(maximum,liveContext),true);assert.equal(bounded.write(maximum,liveContext),true);
assert.equal(bounded.stats().pendingChars,TERMINAL_LIMITS.pendingChars);
assert.equal(bounded.write('x',liveContext),false);
assert.ok(bounded.stats().pendingChars<=TERMINAL_LIMITS.pendingChars);stalled.flush();
assert.equal(bounded.write(maximum+'x',liveContext),false,'Oversized single writes are never handed to the parser');
assert.equal(bounded.stats().pendingWrites,0);bounded.dispose();

// Parser-aware producer advances only after callbacks, so flood pace cannot grow queue.
const slow=delayedTerminal();
const backpressured=createTerminalTransport(slow,{sendUserInput(){},sendDeviceReply(){},isContextCurrent:()=>true});
const signalController=new AbortController();
const producer=(async()=>{for(let i=0;i<20;i++){
 assert.equal(backpressured.write('x'.repeat(65536),liveContext),true);
 await backpressured.whenIdle({signal:signalController.signal});
}})();
for(let i=0;i<20;i++){
 assert.equal(backpressured.stats().pendingWrites,1);assert.equal(slow.callbacks.length,1);
 slow.flush();await Promise.resolve();
}
await producer;assert.equal(backpressured.stats().pendingChars,0);
backpressured.write('still parsing',liveContext);
const waiting=backpressured.whenIdle({signal:signalController.signal});
backpressured.reset();signalController.abort();
await assert.rejects(waiting,{name:'AbortError'});
assert.equal(slow.callbacks.length,1,'Cancel/reset does not wait for a stalled parser callback');
backpressured.write('new session display',{...liveContext,generation:2});
assert.ok(backpressured.stats().pendingWrites<=2);
slow.flush();slow.flush();await backpressured.whenIdle();backpressured.dispose();

const limitsSource=await fs.readFile(new URL('./stream-limits.js',import.meta.url),'utf8');
const {appendSseChunk,MAX_SSE_BUFFER_CHARS,SseResourceLimitError}=await import('data:text/javascript;base64,'+Buffer.from(limitsSource).toString('base64'));
assert.equal(MAX_SSE_BUFFER_CHARS,8*1024*1024);
assert.equal(appendSseChunk('data: x\r','\n\r\n'),'data: x\n\n');
let incomplete='';for(let i=0;i<8;i++)incomplete=appendSseChunk(incomplete,'01234567',64);
assert.equal(incomplete.length,64);assert.throws(()=>appendSseChunk(incomplete,'x',64),SseResourceLimitError);
assert.equal(appendSseChunk('','x'.repeat(4*1024*1024+4096)).length,4*1024*1024+4096,'Valid maximum sync metadata plus framing has margin');
const appSource=await fs.readFile(new URL('./app.js',import.meta.url),'utf8');
assert.ok(appSource.includes('await terminalTransport.whenIdle({signal:controller.signal})'));
assert.ok(appSource.includes('buffer=appendSseChunk(buffer,decoder.decode(value,{stream:true}))'));
console.log('PASS: delayed parser proves 2 Mi/write, 4 Mi total and 64-write bounds; overflow visibly resets/revokes replies; per-event backpressure stays at one write; abort/reset remains responsive; SSE buffer is capped at 8 Mi characters.');
