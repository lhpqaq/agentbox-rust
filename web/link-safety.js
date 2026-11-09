/** Terminal output is untrusted. Accept only explicit web URLs, never relative URIs. */
export function validateTerminalLink(value) {
  if (typeof value !== 'string' || !/^https?:\/\//i.test(value) || /[\s\u0000-\u001f\u007f\\]/u.test(value)) return null;
  const authority = value.match(/^https?:\/\/([^/?#]*)/i)?.[1];
  if (!authority || authority.includes('@')) return null;
  try {
    const url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol) || !url.hostname || url.username || url.password) return null;
    return url.href;
  } catch { return null; }
}

/** No navigation occurs until a valid URL has explicit confirmation. */
export function createTerminalLinkHandler({ confirmOpen, openLink, showBlocked }) {
  return {
    // Route every OSC 8 target through our validator so rejected schemes show a
    // blocked notice rather than silently disappearing. Never use xterm's opener.
    allowNonHttpProtocols: true,
    activate(event, value) {
      event?.preventDefault?.();
      const url = validateTerminalLink(value);
      if (!url) {
        showBlocked('已阻止终端链接：仅允许不含登录凭据的完整 HTTP / HTTPS 地址。');
        return;
      }
      if (!confirmOpen(`是否在新标签页打开此终端链接？\n${url}\n终端输出中的链接可能来自不可信程序，请核对地址。`)) return;
      openLink(url, '_blank', 'noopener,noreferrer');
    }
  };
}
