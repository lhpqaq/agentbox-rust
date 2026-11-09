/** Refresh authoritative metadata without clearing already reconstructed output. */
export function applyAuthoritativeSnapshot(cache, currentSession, snapshotSession, commands, bounds = snapshotSession) {
  if (snapshotSession && currentSession && snapshotSession.id === currentSession.id) {
    Object.assign(currentSession, snapshotSession);
    if (Number.isSafeInteger(bounds?.first_seq) && bounds.first_seq >= 0) cache.retainedFirstSeq = bounds.first_seq;
  }
  if (!Array.isArray(commands)) return;
  const byId = new Map(cache.commands.map(command => [command.id, command]));
  const authoritative = new Set();
  for (const command of commands) {
    if (!command || typeof command.id !== 'string') continue;
    byId.set(command.id, {...command});
    authoritative.add(command.id);
  }
  cache.commands = [...byId.values()];
  cache.authoritativeCommandIds = authoritative;
  // This cursor is solely a fallback for legacy output without command_id.
  // Current command status is always read from authoritative command metadata.
  cache.active = commands.find(command => command.status === 'running')?.id || null;
}

/** Historical events reconstruct history, never roll back current metadata. */
export function mergeEventMetadata(cache, currentSession, event, isLive) {
  const data = event.data || {};
  if (event.type === 'command_started' || event.type === 'command_finished') {
    let existing = cache.commands.find(command => command.id === data.id);
    const authoritative = cache.authoritativeCommandIds?.has(data.id);
    const existingTerminal = existing && !['running', 'unknown'].includes(existing.status);
    if (isLive || (!authoritative && !existingTerminal)) {
      const update = !isLive && event.type === 'command_started' ? {...data, status: 'unknown'} : data;
      if (existing) Object.assign(existing, update);
      else cache.commands.push({...update});
    }
    if (event.type === 'command_started') cache.active = data.id;
    else if (cache.active === data.id) cache.active = null;
    if (isLive && currentSession) {
      if (event.type === 'command_finished' && ['exited', 'lost'].includes(data.status)) {
        currentSession.status = 'closed';
      } else if (currentSession.status !== 'closed') {
        currentSession.status = event.type === 'command_started' ? 'running' : 'idle';
      }
      if (data.cwd) currentSession.cwd = data.cwd;
      if (event.type === 'command_finished') currentSession.exit_code = data.exit_code;
    }
  } else if (isLive && currentSession && event.type === 'session_updated') {
    Object.assign(currentSession, data);
  } else if (isLive && currentSession && event.type === 'session_closed') {
    currentSession.status = 'closed';
  }
}

/** Display only observed status/duration; missing timing never implies activity. */
export function commandPresentation(command) {
  const knownExit = Number.isInteger(command.exit_code);
  const exitSuffix = knownExit ? ` · EXIT ${command.exit_code}` : '';
  let label, tone;
  switch (command.status) {
    case 'running': label = '● RUNNING'; tone = 'success'; break;
    case 'finished':
      label = knownExit ? `● EXIT ${command.exit_code}` : '● 已结束 · 退出码未知';
      tone = command.exit_code === 0 ? 'success' : knownExit ? 'failed' : ''; break;
    case 'failed': label = '● 执行失败' + exitSuffix; tone = 'failed'; break;
    case 'interrupted': label = '■ 已中断' + exitSuffix; tone = 'failed'; break;
    case 'exited': label = '● Shell 已退出' + exitSuffix; tone = ''; break;
    case 'lost': label = '◌ 执行状态丢失'; tone = ''; break;
    case 'unknown': label = '◌ 历史状态未知'; tone = ''; break;
    default: label = '◌ 状态未知'; tone = '';
  }
  const duration = Number.isFinite(command.duration_ms) && command.duration_ms >= 0
    ? (command.duration_ms / 1000).toFixed(2) + 's'
    : command.status === 'running' ? '执行中' : '时长未知';
  return {label, tone, duration};
}

/** A retained prefix boundary is evidence of possible loss, never a byte count. */
export function serverHistoryMayBeIncomplete(command, firstSeq) {
  if (!Number.isSafeInteger(command.start_seq) || command.start_seq <= 0 ||
      !Number.isSafeInteger(firstSeq) || firstSeq < 0) return false;
  // The broker cursor uses zero when no events remain in this session.
  return firstSeq === 0 || command.start_seq < firstSeq;
}
