# AgentBox Rust browser UI

The existing buildless Chinese UI is retained as HTML/CSS/ES modules with locally vendored xterm. The Rust HTTP module embeds an explicit asset allowlist into the executable. No CDN or frontend dependency installation is needed.

## Rust-specific behavior

- Automatic terminal device replies are disabled in app.js and rejected by the broker. The reusable terminal-input module still contains and tests its live/replay gating, but its live reply callback is a no-op in this edition.
- Ordinary raw input permanently prevents subsequent structured runs in that session. Continue interacting or create another session; do not expect returning to a visible prompt to re-enable structured execution.
- A changed/missing managed prompt may similarly leave the session interactive after the current command completes.
- This package has not been compiled, served or tested in a real browser. See ../docs/VERIFICATION.md.

## Retained safety behavior

Token is tab-local sessionStorage and sent in a Bearer header, never in URLs. Unlock/lock, 401 handling, per-session replay cursors, authoritative snapshots, bounded caches, generation guards, delayed parser backpressure and explicit truncation notices remain. Names/commands/structured output render as text. Historical command replay fills the composer and requires another explicit execution action. Failed command submission never triggers automatic retry; manual retry preserves its page-memory request ID until confirmed.

Terminal links permit explicit HTTP(S) URLs only, reject credentials/control characters and ask before opening a new window. Terminal content remains untrusted, and these checks are not a security audit.

Browser caches have separate limits from SQLite history: eight sessions, 500 commands, 250,000 output characters per command, and 2,000,000 structured/raw output characters per session. Queued input is bounded to 64 requests/256 KiB and uses UTF-8-safe chunks. Parser writes and SSE text are bounded by the modules. Search covers retained local cache, not an indefinite archive.

## Tests actually run

From the project root:

```sh
for test in web/test-*.mjs; do node "$test"; done
```

All six files passed, including actual vendored parser tests and controlled mock transports. These are not browser/rendering, HTTP/server or end-to-end tests. The reusable adapter's allowed-live-device-response tests do not claim that automatic responses are enabled in the Rust application.

Manual browser checks remain: unlock/lock, creation interrupted by locking, repeated clicks, rename/close, stateful commands, failure/interrupt, raw input, resize, high-volume output, session switching during output, stream disconnect/reconnect, refresh, unsafe links and narrow-screen navigation. See vendor/NOTICE.txt and associated licenses for dependency provenance.
