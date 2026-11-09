export const INPUT_LIMITS = Object.freeze({requests:64, bytes:256*1024, userRequestBytes:60*1024, deviceRequestBytes:4096, timeoutMs:8000});
const encoder=new TextEncoder(),decoder=new TextDecoder('utf-8',{fatal:true,ignoreBOM:true});
function splitUtf8(bytes,limit){
  const chunks=[];
  for(let start=0;start<bytes.length;){
    let end=Math.min(start+limit,bytes.length);
    while(end<bytes.length&&(bytes[end]&0xc0)===0x80)end--;
    chunks.push({data:decoder.decode(bytes.subarray(start,end)),bytes:end-start});start=end;
  }return chunks;
}
/** Serialized and bounded; every accepted byte is tied to a session generation. */
export function createInputTransport({send,isCurrent,onError=()=>{},onDeviceDrop=()=>{},setTimer=setTimeout,clearTimer=clearTimeout}){
  const queue=[];let active=null,pendingBytes=0,epoch=0,deviceDropNotified=false;
  function dropDevice(message){if(!deviceDropNotified){deviceDropNotified=true;onDeviceDrop(message);}}
  function discardQueued(){const user=queue.some(item=>item.userInput);for(const item of queue)pendingBytes-=item.bytes;queue.length=0;return user;}
  function release(job){if(active===job){pendingBytes-=job.item.bytes;active=null;}}
  function fail(job,failure){
    const queuedUser=discardQueued();release(job);
    if(job.item.userInput||queuedUser){onError('终端输入未能确认，后续排队输入已取消。部分字节可能已写入，请检查终端状态；不会自动重试。'+(failure?.message?' '+failure.message:''));}
    else dropDevice('自动终端应答发送失败，已停止本次排队应答；不会自动重试。');
  }
  function pump(){
    if(active)return;
    const item=queue.shift();if(!item)return;
    if(!isCurrent(item.context)){pendingBytes-=item.bytes;if(item.userInput)onError('会话已变化，尚未发送的终端输入已取消。');pump();return;}
    const controller=new AbortController();const job={item,controller,epoch};active=job;
    let timer;const abortFailure=new Promise((_,reject)=>controller.signal.addEventListener('abort',()=>reject(controller.signal.reason||new DOMException('Input cancelled','AbortError')),{once:true}));
    timer=setTimer(()=>controller.abort(new Error('终端输入请求超过 8 秒，写入结果未知。')),INPUT_LIMITS.timeoutMs);
    // Promise.race lets a new generation proceed even if an old mock/transport
    // ignores abort; the real fetch receives the same AbortSignal.
    const request=Promise.resolve().then(()=>{
      if(controller.signal.aborted||job.epoch!==epoch)throw new DOMException('Input cancelled','AbortError');
      return send(item,{signal:controller.signal});
    });
    Promise.race([request,abortFailure]).then(()=>{
      if(active!==job||job.epoch!==epoch)return;release(job);pump();
    },failure=>{
      if(active!==job||job.epoch!==epoch)return;
      if(!item.userInput&&failure?.status===409){release(job);pump();return;}
      fail(job,failure);
    }).finally(()=>clearTimer(timer));
  }
  return{
    enqueue(data,context,userInput=true){
      if(!isCurrent(context))return false;
      const reject=()=>{if(userInput)onError('终端输入队列已满或输入过大，本次输入未加入队列。请等待已接受输入完成，再检查终端后手动重试。');else dropDevice('自动终端应答超过安全队列限制，超出部分已丢弃。');return false;};
      if(typeof data!=='string'||data.length>INPUT_LIMITS.bytes)return reject();
      if(data.includes('\0')){if(userInput)onError('输入包含服务不接受的 NUL 字符，本次输入未加入队列。');else dropDevice('自动终端应答包含无效字符，已丢弃。');return false;}
      if(!data)return true;
      const bytes=encoder.encode(data);
      if(bytes.length>INPUT_LIMITS.bytes||(!userInput&&bytes.length>INPUT_LIMITS.deviceRequestBytes))return reject();
      const chunks=splitUtf8(bytes,userInput?INPUT_LIMITS.userRequestBytes:INPUT_LIMITS.deviceRequestBytes);
      if(pendingBytes+bytes.length>INPUT_LIMITS.bytes||queue.length+(active?1:0)+chunks.length>INPUT_LIMITS.requests)return reject();
      for(const chunk of chunks)queue.push({...chunk,context:{...context},userInput});
      pendingBytes+=bytes.length;pump();return true;
    },
    cancel(reason='会话已变化'){
      epoch++;const queuedUser=discardQueued(),job=active;
      if(job){release(job);job.controller.abort(new DOMException(reason,'AbortError'));}
      deviceDropNotified=false;
      if(queuedUser||job?.item.userInput)onError(reason+'，未发送输入已取消；在途输入可能已写入，请检查终端。不会自动重试。');
    },
    stats(){return{bytes:pendingBytes,requests:queue.length+(active?1:0)};}
  };
}
