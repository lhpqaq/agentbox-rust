/** Exact submitted bytes determine retry identity; do not trim inside this helper. */
export function prepareSubmission(cache, command, createId = () => crypto.randomUUID()) {
  cache.pendingRequests ??= new Map();
  const previous = cache.pendingRequests.get(command);
  if (previous) return previous;
  const attempt = Object.freeze({command, request_id: createId()});
  if (typeof attempt.request_id !== 'string' || !attempt.request_id) throw new Error('无法生成请求 ID，命令未提交。');
  cache.pendingRequests.set(command, attempt);
  return attempt;
}

/** Clear only the exact attempt for which the HTTP result has been confirmed. */
export function confirmSubmission(cache, attempt) {
  if (cache.pendingRequests?.get(attempt.command)?.request_id === attempt.request_id) {
    cache.pendingRequests.delete(attempt.command);
  }
}

export function readSubmissionResult(result, attempt) {
  const command = result?.command && typeof result.command === 'object' ? result.command : result;
  if (!command || typeof command.id !== 'string' || !command.id ||
      typeof command.status !== 'string' || !command.status ||
      command.command !== attempt.command || command.request_id !== attempt.request_id) {
    throw new Error('服务返回的命令确认不完整或不匹配。');
  }
  return command;
}

/** A late running HTTP response must not overwrite a newer terminal SSE result. */
export function mergeSubmissionResult(commands, command) {
  const existing = commands.find(item => item.id === command.id);
  if (!existing) { commands.push(command); return command; }
  if (existing.status !== 'running' && command.status === 'running') return existing;
  Object.assign(existing, command);
  return existing;
}

export function uncertainSubmissionMessage(error, attempt) {
  const detail = typeof error?.message === 'string' && error.message.trim() ? error.message.trim() : '请求未获得有效确认。';
  return `${detail}\n未能确认提交结果，命令可能已执行。请先检查执行记录。\n请求 ID：${attempt.request_id}\n本页手动重试相同命令将复用此 ID；不会自动重试。刷新或关闭页面前请保留此 ID。`;
}
