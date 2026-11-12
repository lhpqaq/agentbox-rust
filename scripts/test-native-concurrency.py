#!/usr/bin/env python3
"""Concurrent native startup must isolate environments/import files and stay interactive."""
import concurrent.futures
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get('AGENTBOX_TEST_BIN', ROOT / 'target/debug/agentbox')).resolve()


def main():
    with tempfile.TemporaryDirectory(prefix='ab-native-many-') as temp:
        base = pathlib.Path(temp).resolve()
        home, user = base / 'state', base / 'user'
        user.mkdir()
        (user / '.zshrc').write_text("read -r ANSWER\nPROMPT='NATIVE> '\n")
        state = base / 'state.zsh'
        state.write_text('NATIVE_VALUE=$CLIENT_VALUE\n')
        state.chmod(0o600)
        shell = shutil.which('zsh')
        assert shell

        def cli(*args, env=None):
            result = subprocess.run([str(BIN), '--home', str(home), *args],
                                    env={**os.environ, **(env or {})},
                                    capture_output=True, text=True, timeout=20)
            assert result.returncode == 0, (args[0], result.stderr)
            return result.stdout.strip()

        def data(*args):
            return json.loads(cli(*args))

        def create(number):
            sid = cli('create', f'native-{number}', '--shell', shell, '--state-file', str(state),
                      '--inherit-env', env={'HOME': str(user), 'ZDOTDIR': str(user),
                                           'CLIENT_VALUE': str(number), 'skip_global_compinit': '1'})
            return number, sid

        try:
            data('start')
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                sessions = list(pool.map(create, range(16)))
            assert len({sid for _, sid in sessions}) == 16
            before = data('sessions')
            assert all(s['shell']['state_imported'] is False for s in before)
            assert len(list((home / 'runtime').glob('.zsh-state-*'))) == 16
            # All 16 shells are waiting for startup input, yet creation/listing
            # completed and each client can answer only its own session.
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                futures = [pool.submit(data, 'input', sid, f'answer-{n}', '--newline') for n, sid in sessions]
                for future in futures:
                    future.result()
            deadline = time.monotonic() + 10
            while not all(s['shell']['state_imported'] is True for s in data('sessions')):
                assert time.monotonic() < deadline, 'native imports did not finish'
                time.sleep(.03)
            assert not list((home / 'runtime').glob('.zsh-state-*'))

            def observe(pair):
                number, sid = pair
                data('input', sid, 'print -r -- "ISOLATED:$CLIENT_VALUE:$NATIVE_VALUE:$ANSWER"', '--newline')
                expected = f'ISOLATED:{number}:{number}:answer-{number}'
                deadline = time.monotonic() + 5
                while True:
                    page = data('read', sid)
                    if expected in page['output']:
                        return
                    assert time.monotonic() < deadline, (number, page['output'])
                    time.sleep(.03)

            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                list(pool.map(observe, sessions))
            for _, sid in sessions:
                data('close', sid)
            assert not data('sessions', '--active')
            print('PASS: 16 concurrent native shells isolate caller environment, startup answers and imported state; pending startup never blocks the broker; all private files are cleaned', flush=True)
        finally:
            if home.exists():
                cli('stop', '--shutdown-sessions', '--broker-only')


if __name__ == '__main__':
    main()
