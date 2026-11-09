import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source=await fs.readFile(new URL('./input-transport.js',import.meta.url),'utf8');
const {createInputTransport,INPUT_LIMITS}=await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
const tick=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
function harness(){
 let generation=1,nextTimer=0;const timers=new Map(),calls=[],errors=[],drops=[];
 const transport=createInputTransport({
  isCurrent:context=>context.generation===generation,
  send:(item,{signal})=>new Promise((resolve,reject)=>calls.push({item,signal,resolve,reject})),
  onError:message=>errors.push(message),onDeviceDrop:message=>drops.push(message),
  setTimer:(callback,ms)=>{assert.equal(ms,8000);timers.set(++nextTimer,callback);return nextTimer;},clearTimer:id=>timers.delete(id)
 });
 return{transport,calls,errors,drops,timers,context:()=>({session_id:'s'+generation,generation}),switch(){generation++;transport.cancel('测试切换');}};
}
const ordered=harness();
ordered.transport.enqueue('a',ordered.context());ordered.transport.enqueue('device',ordered.context(),false);ordered.transport.enqueue('b',ordered.context());
await tick();assert.equal(ordered.calls.length,1);assert.equal(ordered.calls[0].item.data,'a');
ordered.calls[0].resolve();await tick();assert.equal(ordered.calls[1].item.data,'device');
ordered.calls[1].resolve();await tick();assert.equal(ordered.calls[2].item.data,'b');
ordered.calls[2].resolve();await tick();assert.deepEqual(ordered.transport.stats(),{bytes:0,requests:0});
const unicode=harness(),paste='\uFEFF'+'🙂中文'.repeat(12000);
assert.equal(unicode.transport.enqueue(paste,unicode.context()),true);await tick();
let received='';for(let i=0;i<unicode.calls.length;i++){
 const call=unicode.calls[i];assert.ok(new TextEncoder().encode(call.item.data).length<=INPUT_LIMITS.userRequestBytes);
 received+=call.item.data;call.resolve();await tick();
}assert.equal(received,paste,'Splitting preserves Unicode text and accepted byte order');
const bounded=harness();
for(let i=0;i<64;i++)assert.equal(bounded.transport.enqueue('x',bounded.context()),true);
assert.equal(bounded.transport.enqueue('rejected',bounded.context()),false);await tick();
assert.equal(bounded.calls.length,1);assert.equal(bounded.transport.stats().requests,64);assert.match(bounded.errors[0],/未加入队列/);
assert.equal(bounded.transport.enqueue('reply',bounded.context(),false),false);assert.equal(bounded.drops.length,1);
bounded.transport.cancel();await tick();assert.equal(bounded.calls[0].signal.aborted,true);assert.equal(bounded.transport.stats().requests,0);
const byteBound=harness();
assert.equal(byteBound.transport.enqueue('x'.repeat(INPUT_LIMITS.bytes),byteBound.context()),true);
assert.equal(byteBound.transport.stats().bytes,INPUT_LIMITS.bytes);
assert.equal(byteBound.transport.enqueue('x',byteBound.context()),false);
assert.equal(byteBound.transport.stats().bytes,INPUT_LIMITS.bytes,'Rejected input cannot be partly enqueued');
byteBound.transport.cancel();await tick();
const oversized=harness();
assert.equal(oversized.transport.enqueue('x'.repeat(INPUT_LIMITS.bytes+1),oversized.context()),false);
assert.equal(oversized.transport.enqueue('x'.repeat(4097),oversized.context(),false),false);await tick();assert.equal(oversized.calls.length,0);
const invalid=harness();
assert.equal(invalid.transport.enqueue('x'.repeat(100000)+'\0',invalid.context()),false);
await tick();assert.equal(invalid.calls.length,0,'A NUL anywhere in a paste rejects all chunks before sending');
assert.deepEqual(invalid.transport.stats(),{bytes:0,requests:0});assert.match(invalid.errors[0],/NUL/);
const switched=harness();
switched.transport.enqueue('old',switched.context());switched.transport.enqueue('old queued',switched.context());await tick();
switched.switch();assert.equal(switched.calls[0].signal.aborted,true);
switched.transport.enqueue('new',switched.context());await tick();
assert.equal(switched.calls.length,2,'New input must not wait behind an old unresolved fetch');assert.equal(switched.calls[1].item.data,'new');
switched.calls[0].resolve();await tick();assert.equal(switched.transport.stats().requests,1,'Late old completion must not release new active input');
switched.calls[1].resolve();await tick();assert.equal(switched.transport.stats().requests,0);
const timeout=harness();timeout.transport.enqueue('uncertain',timeout.context());timeout.transport.enqueue('must not follow',timeout.context());await tick();
[...timeout.timers.values()][0]();await tick();assert.equal(timeout.calls[0].signal.aborted,true);
assert.equal(timeout.calls.length,1,'Timeout never retries or sends queued input after uncertain bytes');assert.equal(timeout.transport.stats().requests,0);assert.match(timeout.errors[0],/可能已写入.*不会自动重试/);
timeout.calls[0].resolve();await tick();assert.equal(timeout.calls.length,1);
const conflict=harness();conflict.transport.enqueue('report',conflict.context(),false);conflict.transport.enqueue('user',conflict.context());await tick();
conflict.calls[0].reject(Object.assign(new Error('stale reply'),{status:409}));await tick();
assert.equal(conflict.calls[1].item.data,'user');assert.equal(conflict.errors.length,0);conflict.calls[1].resolve();await tick();
const app=await fs.readFile(new URL('./app.js',import.meta.url),'utf8');
assert.ok(!app.includes('inputChain'));assert.ok(app.includes("inputTransport.cancel('工作台已锁定')"));assert.ok(app.includes("inputTransport.cancel('会话已切换')"));
console.log('PASS: bounded 64-request/256-KiB input, UTF-8-safe 60-KiB chunks, atomic overflow rejection, FIFO user/device ordering, 8-second abort, immediate generation switch, late-completion isolation, device409 and no uncertain-write retries. Mock-only; no sockets.');
