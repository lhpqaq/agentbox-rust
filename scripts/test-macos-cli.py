#!/usr/bin/env python3
"""Exercise the built CLI against a disposable broker; no browser or HTTP server."""
import json
import pathlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/agentbox'


def main():
    with tempfile.TemporaryDirectory(prefix='agentbox-cli-') as temp:
        base = pathlib.Path(temp)
        home = base / 'state'
        work = base / 'work'
        work.mkdir()
        (work / 'child').mkdir()
        checks = []
        started = False

        def cli(*args, success=True):
            p = subprocess.run([str(BIN), '--home', str(home), '--port', '0', *args],
                               capture_output=True, text=True, timeout=40)
            if success and p.returncode != 0:
                raise AssertionError(f'{args[0]} failed: {p.stderr}\n{p.stdout}')
            if not success and p.returncode == 0:
                raise AssertionError(f'{args[0]} unexpectedly succeeded: {p.stdout}')
            return p

        def data(*args, success=True):
            return json.loads(cli(*args, success=success).stdout)

        def check(name):
            checks.append(name)
            print(f'PASS: {name}', flush=True)

        try:
            data('start')
            started = True
            sid = cli('create', 'CLI test', '--cwd', str(work)).stdout.strip()
            data('run', sid, "export CLI_VALUE=kept; cli_fn(){ printf 'function:%s' \"$CLI_VALUE\"; }; cd child", '--request-id', 'state')
            r = data('run', sid, 'pwd; cli_fn', '--request-id', 'observe-state')
            assert str(work / 'child') in r['output'] and 'function:kept' in r['output']
            check('cwd, exported variable and function survive separate CLI invocations')

            command = 'CLI_COUNTER=$(( ${CLI_COUNTER:-0} + 1 )); printf "%s" "$CLI_COUNTER"'
            first = data('run', sid, command, '--request-id', 'once')
            again = data('run', sid, command, '--request-id', 'once', '--no-wait')
            assert first['id'] == again['id'] and again['replayed'] is True
            assert data('run', sid, 'printf "%s" "$CLI_COUNTER"')['output'] == '1'
            cli('run', sid, 'echo conflicting-command', '--request-id', 'once', success=False)
            check('request_id replays the original command and rejects conflicting text')

            failed = data('run', sid, '(exit 7)', success=False)
            assert failed['status'] == 'finished' and failed['exit_code'] == 7
            check('failed commands return a nonzero CLI exit code and preserve exit status')

            large = data('run', sid, "printf '%131072s' '' | tr ' ' x")
            assert large['output'].count('x') == 131072 and not large['output_truncated']
            check('128 KiB output crosses the Unix socket without truncation or disconnection')

            running = data('run', sid, 'sleep 30', '--no-wait', '--request-id', 'interrupt')
            snapshot = data('wait', sid, running['id'], '--timeout', '0', success=False)
            assert snapshot['status'] == 'running' and snapshot['timed_out'] is True
            data('interrupt', sid)
            interrupted = data('wait', sid, running['id'], success=False)
            assert interrupted['status'] == 'interrupted' and interrupted['exit_code'] == 130
            assert data('run', sid, 'true')['exit_code'] == 0
            check('zero-time wait observes only; interrupt ends foreground work and preserves the shell')

            reading = data('run', sid, "read -r answer; printf 'reply:%s' \"$answer\"", '--no-wait')
            data('input', sid, 'hello', '--newline')
            assert 'reply:hello' in data('wait', sid, reading['id'])['output']
            cli('run', sid, 'echo should-not-run', success=False)
            assert data('close', sid)['status'] == 'closed'
            check('raw input answers a prompt; later structured runs are conservatively blocked')

            pending_sid = cli('create', 'shutdown test', '--cwd', str(work)).stdout.strip()
            data('run', pending_sid, 'sleep 30', '--no-wait')
            cli('stop', '--shutdown-sessions')
            started = False
            deadline = time.monotonic() + 5
            while (home / 'broker.sock').exists():
                assert time.monotonic() < deadline, 'Broker did not stop'
                time.sleep(.05)
            data('start')
            started = True
            sessions = data('sessions')
            assert all(s['status'] == 'closed' for s in sessions)
            replay = data('run', sid, command, '--request-id', 'once', '--no-wait')
            assert replay['id'] == first['id'] and replay['replayed'] is True
            check('broker shutdown/restart preserves history and deduplication without reviving closed shells')
        finally:
            if started:
                cli('stop', '--shutdown-sessions')
                deadline = time.monotonic() + 5
                while (home / 'broker.sock').exists() and time.monotonic() < deadline:
                    time.sleep(.05)
        print(f'{len(checks)} CLI scenarios passed; disposable instance shut down.', flush=True)


if __name__ == '__main__':
    main()
