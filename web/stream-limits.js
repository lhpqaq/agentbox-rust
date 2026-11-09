/** Sync metadata is byte-bounded to 4 MiB; 8 Mi characters leaves framing headroom. */
export const MAX_SSE_BUFFER_CHARS = 8 * 1024 * 1024;
export class SseResourceLimitError extends Error {
  constructor() {
    super('事件数据超过安全缓冲限制，已暂停读取。请检查本地服务后切换会话重连。');
    this.name = 'SseResourceLimitError';
  }
}
/** The retained undecoded frame text is bounded even when a delimiter never arrives. */
export function appendSseChunk(buffer, chunk, limit = MAX_SSE_BUFFER_CHARS) {
  if (buffer.length + chunk.length > limit) throw new SseResourceLimitError();
  // Normalize after joining so a CR/LF split across network chunks still works.
  return (buffer + chunk).replace(/\r\n/g, '\n');
}
