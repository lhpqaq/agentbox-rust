#!/usr/bin/env python3
"""Real concurrent clients: keyed submissions execute once, reads never mutate."""
import concurrent.futures
import json
import os
import pathlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get('AGENTBOX_TEST_BIN', ROOT / 'target/debug/agentbox')).resolve()


def main():
    with tempfile.TemporaryDirectory(prefix='ab-concurrent-') as temp:
        base = pathlib.Path(temp)
        home, work = base / 'state', base / 'work'
        work.mkdir()

        def cli(*args, success=True):
            p = subprocess.run([str(BIN), '--home', str(home), *args],
                               capture_output=True, text=True, timeout=30)
            assert (p.returncode == 0) == success, (args[0], p.stderr, p.stdout[-1000:])
            return p.stdout

        cli('start')
        try:
            sid = cli('create', 'concurrent', '--cwd', str(work)).strip()
            command = "printf 'once\\n' >> once.txt; printf 'CONCURRENT_DONE\\n'"

            def submit(_):
                return json.loads(cli('run', sid, command, '--request-id', 'shared-request', '--no-wait'))

            with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
                submissions = list(pool.map(submit, range(16)))
            assert len({r['id'] for r in submissions}) == 1
            assert sum(r.get('replayed') is True for r in submissions) == 15
            result = json.loads(cli('wait', sid, submissions[0]['id']))
            assert result['status'] == 'finished' and result['exit_code'] == 0
            assert (work / 'once.txt').read_text() == 'once\n'

            def observe(_):
                return json.loads(cli('read', sid))

            with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
                pages = list(pool.map(observe, range(32)))
            assert all('CONCURRENT_DONE' in p['output'] for p in pages)
            assert len(json.loads(cli('history', sid))) == 1
            assert (work / 'once.txt').read_text() == 'once\n'
            cli('run', sid, 'touch must-not-run', '--request-id', 'shared-request', '--no-wait', success=False)
            assert not (work / 'must-not-run').exists()
            print('PASS: 16 concurrent keyed submissions execute once; 32 concurrent reads preserve state; conflicting replay is refused', flush=True)
        finally:
            cli('stop', '--shutdown-sessions', '--broker-only')
            deadline = time.monotonic() + 5
            while (home / 'broker.sock').exists():
                assert time.monotonic() < deadline, 'Disposable broker did not stop'
                time.sleep(.05)


if __name__ == '__main__':
    main()
