#!/usr/bin/env python3
"""Opt-in real broker stress: large output, bounded capture and retention loss."""
import json
import pathlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/agentbox'


def main():
    with tempfile.TemporaryDirectory(prefix='ab-bulk-') as temp:
        home = pathlib.Path(temp) / 'state'

        def cli(*args):
            result = subprocess.run([str(BIN), '--home', str(home), *args],
                                    capture_output=True, text=True, timeout=90)
            assert result.returncode == 0, (result.stderr, result.stdout[-2000:])
            return result.stdout

        cli('start')
        try:
            sid = cli('create', 'bulk output').strip()
            start = time.monotonic()
            result = json.loads(cli('run', sid,
                                    "head -c 9437184 /dev/zero | tr '\\000' x",
                                    '--timeout', '60'))
            assert result['status'] == 'finished' and result['exit_code'] == 0
            assert result['timed_out'] is False
            assert result['output_bytes'] == 9437184
            assert result['output'] == 'x' * (1024 * 1024)
            assert result['output_truncated'] is True
            assert result['output_may_be_truncated'] is True
            page = json.loads(cli('read', sid))
            assert page['output_may_be_truncated'] is True
            assert page['output_bytes'] <= 65536 and page['has_more']
            print(f'PASS: 9 MiB output completes, bounded capture and retention flags agree ({time.monotonic()-start:.2f}s)', flush=True)
        finally:
            cli('stop', '--shutdown-sessions', '--broker-only')
            deadline = time.monotonic() + 5
            while (home / 'broker.sock').exists():
                assert time.monotonic() < deadline, 'Disposable broker did not stop'
                time.sleep(.05)


if __name__ == '__main__':
    main()
