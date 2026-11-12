#!/usr/bin/env python3
"""Real native PTY + CLI shared-input test; no browser or HTTP server."""
import fcntl
import json
import os
import pathlib
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/agentbox'


class Attached:
    def __init__(self, home, session=None, read_only=False):
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        args = [str(BIN), '--home', str(home), 'attach']
        if session:
            args += [session, '--no-replay']
        if read_only:
            args += ["--read-only"]
        self.process = subprocess.Popen(args, stdin=self.slave, stdout=self.slave,
                                        stderr=self.slave, env={**os.environ, 'TERM': 'xterm-256color'})
        self.output = b''

    def until(self, needle, timeout=10, start=0):
        deadline = time.monotonic() + timeout
        while needle not in self.output[start:]:
            assert time.monotonic() < deadline, f'Missing {needle!r}; output={self.output[-3000:]!r}'
            ready, _, _ = select.select([self.master], [], [], .1)
            if ready:
                self.output += os.read(self.master, 65536)
            elif self.process.poll() is not None:
                raise AssertionError(f'attach exited {self.process.returncode}: {self.output!r}')
        return self.output

    def send(self, data):
        os.write(self.master, data)

    def finished(self):
        assert self.process.wait(timeout=10) == 0
        restored = termios.tcgetattr(self.slave)
        expected = self.original.copy()
        # Darwin sets this kernel-managed flag on raw -> canonical transition.
        restored[3] &= ~termios.PENDIN
        expected[3] &= ~termios.PENDIN
        assert restored == expected, f'Local tty settings were not restored: before={expected!r}; after={restored!r}'

    def dispose(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        os.close(self.master)
        os.close(self.slave)


def main():
    with tempfile.TemporaryDirectory(prefix='ab-attach-') as temp:
        home = pathlib.Path(temp) / 'state'
        attached = []

        def cli(*args):
            result = subprocess.run([str(BIN), '--home', str(home), '--port', '0', *args],
                                    capture_output=True, text=True, timeout=15)
            assert result.returncode == 0, (args[0], result.stderr, result.stdout)
            return json.loads(result.stdout)

        def remote(sid, command):
            cli('input', sid, command, '--newline')

        try:
            one = Attached(home)
            attached.append(one)
            one.until(b'AgentBox session: ')
            # The ID can arrive in a later read than the prefix.
            one.until(b'Ctrl-] detaches')
            sid = re.search(rb'AgentBox session: ([0-9a-f]{32})', one.output).group(1).decode()
            one.until(b'agentbox ')
            one.send(b'export SHARED_TTY=from_local; printf "LOCAL_OK:%s\\n" "$SHARED_TTY"\r')
            one.until(b'LOCAL_OK:from_local')
            prompt_start = one.output.index(b'LOCAL_OK:from_local') + len(b'LOCAL_OK:from_local')
            one.until(b'agentbox ', start=prompt_start)
            for number in range(2):
                marker = f'CONTINUOUS_{number}'.encode()
                one.send(b'printf "CONTINUOUS_%s\\n" ' + str(number).encode() + b'\r')
                one.until(marker + b'\r\n')
                prompt_start = one.output.index(marker + b'\r\n') + len(marker + b'\r\n')
                one.until(b'agentbox ', start=prompt_start)
            print('PASS: prompt returns after each consecutive command with readline enabled', flush=True)
            print('PASS: ordinary terminal keyboard executes commands in the shared shell', flush=True)

            remote(sid, 'printf "REMOTE_OK:%s\\n" "$SHARED_TTY"')
            one.until(b'REMOTE_OK:from_local')
            # logs is NDJSON rather than a single JSON document.
            logs = subprocess.check_output([str(BIN), '--home', str(home), 'logs', sid], text=True)
            transcript = ''.join(json.loads(line).get('data', {}).get('text', '') for line in logs.splitlines())
            assert 'LOCAL_OK:from_local' in transcript and 'REMOTE_OK:from_local' in transcript
            page = cli('read', sid)
            assert 'LOCAL_OK:from_local' in page['output'] and 'REMOTE_OK:from_local' in page['output']
            assert not page['output_may_be_truncated']
            print('PASS: independent CLI reads local output and types into the same shell and environment', flush=True)

            fcntl.ioctl(one.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 38, 112, 0, 0))
            deadline = time.monotonic() + 5
            while True:
                session = next(s for s in cli('sessions') if s['id'] == sid)
                if (session['rows'], session['cols']) == (38, 112):
                    break
                assert time.monotonic() < deadline, session
                time.sleep(.05)
            print('PASS: local terminal resize reaches the broker PTY', flush=True)

            observer = Attached(home, sid, read_only=True)
            attached.append(observer)
            observer.until(b'READ ONLY:')
            observer.send(b'export OBSERVER_INJECTED=yes\r')
            remote(sid, 'printf "OBSERVER_CHECK:%s\n" "${OBSERVER_INJECTED-unset}"')
            observer.until(b'OBSERVER_CHECK:unset')
            session = next(s for s in cli('sessions') if s['id'] == sid)
            assert (session['rows'], session['cols']) == (38, 112)
            observer.send(b'\x1d')
            observer.finished()
            print('PASS: read-only observer receives output without typing or changing PTY size', flush=True)


            remote(sid, "printf '\\033[6n\\033]52;c;ignored\\007'; printf 'QUERY_FILTERED\\n'")
            one.until(b'QUERY_FILTERED\r\n')
            assert b'\x1b[6n' not in one.output and b'\x1b]52' not in one.output
            one.send(b'sleep 30\r')
            one.until(b'sleep 30')
            one.send(b'\x03')
            one.send(b'printf "AFTER_CTRL_C\\n"\r')
            one.until(b'AFTER_CTRL_C\r\n')
            print('PASS: control queries are suppressed and Ctrl-C reaches foreground work', flush=True)

            one.send(b'\x1d')
            one.finished()
            assert next(s for s in cli('sessions') if s['id'] == sid)['status'] != 'closed'
            print('PASS: Ctrl-] restores the local tty while preserving the remote shell', flush=True)

            two = Attached(home, sid)
            attached.append(two)
            two.until(b'Ctrl-] detaches')
            remote(sid, 'printf "REATTACH_OK:%s\\n" "$SHARED_TTY"')
            two.until(b'REATTACH_OK:from_local')
            two.send(b'exit\r')
            two.finished()
            assert next(s for s in cli('sessions') if s['id'] == sid)['status'] == 'closed'
            print('PASS: reattach preserves state; shell exit closes the session and restores the tty', flush=True)
        finally:
            for instance in attached:
                instance.dispose()
            subprocess.run([str(BIN), '--home', str(home), '--port', '0', 'stop', '--shutdown-sessions', '--broker-only'],
                           capture_output=True, timeout=15)
            deadline = time.monotonic() + 5
            while (home / 'broker.sock').exists() and time.monotonic() < deadline:
                time.sleep(.05)
    print('All native attach scenarios passed; disposable broker shut down.', flush=True)


if __name__ == '__main__':
    main()
