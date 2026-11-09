/** Pinned xterm input provenance; generic onData includes parser-generated replies. */
function observeProvenance(terminal, handler) {
  const core = terminal?._core?.coreService;
  if (!core || typeof core.triggerDataEvent !== 'function' || typeof terminal.onData !== 'function') {
    throw new Error('终端输入安全接口不可用；请检查随应用提供的 xterm 版本。');
  }
  const original = core.triggerDataEvent;
  let provenance = null;
  function guardedTrigger(data, wasUserInput = false) {
    const previous = provenance;
    provenance = wasUserInput === true ? 'user' : 'device';
    try { return original.call(this, data, wasUserInput); }
    finally { provenance = previous; }
  }
  core.triggerDataEvent = guardedTrigger;
  const subscription = terminal.onData(data => {
    if (provenance !== null) handler(data, provenance);
  });
  return { dispose() {
    subscription.dispose();
    if (core.triggerDataEvent === guardedTrigger) core.triggerDataEvent = original;
  } };
}

/** Backward-compatible fail-closed user-input-only subscription. */
export function observeUserTerminalInput(terminal, handler) {
  return observeProvenance(terminal, (data, kind) => { if (kind === 'user') handler(data); });
}

/**
 * Exactly one asynchronous xterm write owns parser reply permission at a time.
 * Reset immediately revokes the current write's permissions, drops pending old
 * writes, then resets only after the current parser callback completes. New
 * writes wait behind that reset, so stale output cannot contaminate a new shell.
 * User keyboard/paste/IME input does not inherit replay context or wait for it.
 */
export const TERMINAL_LIMITS = Object.freeze({writeChars: 2 * 1024 * 1024, pendingChars: 4 * 1024 * 1024, pendingWrites: 64});
export class TerminalResourceLimitError extends Error {
  constructor(message) { super(message); this.name = 'TerminalResourceLimitError'; }
}
export function createTerminalTransport(terminal, {
  sendUserInput, sendDeviceReply, isContextCurrent, onError = () => {}
}) {
  let epoch = 0, busy = false, disposed = false, active = null, repliesRevoked = false;
  let pendingChars = 0, pendingWrites = 0;
  const queue = [], idleWaiters = new Set();
  const subscription = observeProvenance(terminal, (data, kind) => {
    if (disposed) return;
    if (kind === 'user') { sendUserInput(data); return; }
    if (!repliesRevoked && active && active.epoch === epoch && active.context.allowDeviceReplies === true &&
        isContextCurrent(active.context)) sendDeviceReply(data, active.context);
  });
  function settleWaiter(waiter, failure) {
    idleWaiters.delete(waiter);
    waiter.signal?.removeEventListener('abort', waiter.abort);
    if (failure) waiter.reject(failure); else waiter.resolve();
  }
  function notifyIdle() {
    if (!busy && queue.length === 0) for (const waiter of [...idleWaiters]) settleWaiter(waiter);
  }
  function release(item) { pendingChars -= item.text.length; pendingWrites--; }
  function drain() {
    if (busy || disposed) { notifyIdle(); return; }
    const item = queue.shift();
    if (!item) { notifyIdle(); return; }
    if (item.kind === 'reset') {
      try { terminal.reset(); } catch (error) { onError(error); }
      drain(); return;
    }
    if (item.epoch !== epoch || !isContextCurrent(item.context)) { release(item); drain(); return; }
    busy = true; active = item;
    let completed = false;
    const complete = () => {
      if (completed) return;
      completed = true; release(item); active = null; busy = false; drain();
    };
    try { terminal.write(item.text, complete); }
    catch (error) { complete(); onError(error); }
  }
  function requestReset(revokeReplies) {
    epoch++; active = null; repliesRevoked = revokeReplies;
    for (const item of queue) if (item.kind === 'write') release(item);
    queue.length = 0; queue.push({kind: 'reset'}); drain();
  }
  return {
    write(text, context) {
      if (disposed || !text) return true;
      if (typeof text !== 'string' || text.length > TERMINAL_LIMITS.writeChars ||
          pendingChars + text.length > TERMINAL_LIMITS.pendingChars || pendingWrites >= TERMINAL_LIMITS.pendingWrites) {
        requestReset(true);
        onError(new TerminalResourceLimitError('终端显示超出安全缓冲限制，待显示输出已清理，自动终端应答已停用。请切换会话重新载入保留的输出。'));
        return false;
      }
      pendingChars += text.length; pendingWrites++;
      queue.push({kind: 'write', text, context: {...context}, epoch}); drain(); return true;
    },
    reset() { if (!disposed) requestReset(false); },
    whenIdle({signal} = {}) {
      if (signal?.aborted) return Promise.reject(new DOMException('Terminal wait cancelled', 'AbortError'));
      if (!busy && queue.length === 0) return Promise.resolve();
      return new Promise((resolve, reject) => {
        const waiter = {resolve, reject, signal};
        waiter.abort = () => settleWaiter(waiter, new DOMException('Terminal wait cancelled', 'AbortError'));
        idleWaiters.add(waiter); signal?.addEventListener('abort', waiter.abort, {once: true});
      });
    },
    stats() { return {pendingChars, pendingWrites, repliesRevoked}; },
    dispose() {
      disposed = true; epoch++; active = null;
      for (const item of queue) if (item.kind === 'write') release(item);
      queue.length = 0; subscription.dispose();
      for (const waiter of [...idleWaiters]) settleWaiter(waiter);
    }
  };
}

/** Missing/invalid sync metadata always fails closed, including on reconnect. */
export function isLiveOutput(sequence, replayThrough) {
  return Number.isSafeInteger(replayThrough) && replayThrough >= 0 &&
    Number.isSafeInteger(sequence) && sequence > replayThrough;
}

/** Explicit null means unassociated shell output; only old servers omit the key. */
export function outputCommandId(data, activeCommandId) {
  return Object.prototype.hasOwnProperty.call(data, 'command_id') ? data.command_id : activeCommandId;
}
