#!/usr/bin/env python3
"""Native Bash/Zsh startup, plugins, caller environment and shared-process state."""
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get('AGENTBOX_TEST_BIN', ROOT / 'target/debug/agentbox')).resolve()
spec = importlib.util.spec_from_file_location('attach_test', ROOT / 'scripts/test-attach.py')
attach_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attach_test)
Attached = attach_test.Attached


def main():
    with tempfile.TemporaryDirectory(prefix='ab-shell-') as temp:
        base = pathlib.Path(temp).resolve()
        home, user, work = base / 'state', base / 'user', base / 'work'
        user.mkdir()
        work.mkdir()
        (work / 'child').mkdir()
        clients = []

        def cli(*args, env=None, ok=True):
            p = subprocess.run([str(BIN), '--home', str(home), *args], capture_output=True,
                               text=True, timeout=20, env={**os.environ, **(env or {})})
            assert (p.returncode == 0) == ok, (args[0], p.returncode, p.stdout, p.stderr)
            return p

        def data(*args, **kw):
            return json.loads(cli(*args, **kw).stdout)

        def command(client, source, marker):
            start = len(client.output)
            client.send(source.encode() + b'\r')
            client.until(marker.encode(), start=start)

        try:
            # The daemon deliberately gets different, stale values.
            data('start', env={'CALLER_VALUE': 'stale', 'BROKER_ONLY': 'must-not-leak', 'TERM': 'vt100'})
            for kind in ['bash', 'zsh']:
                shell = shutil.which(kind)
                if not shell:
                    raise AssertionError(f'{kind} is required for the native-shell regression suite')
                if kind == 'zsh':
                    (user / '.zshrc').write_text('''
PROMPT='ZSH_FIXTURE> '
autoload -Uz compinit
compinit -D -i
source "$ZDOTDIR/plugin.zsh"
''')
                    (user / 'plugin.zsh').write_text('''
export PLUGIN_LOADED=yes
alias plugin_alias='printf "PLUGIN_ALIAS_OK\\n"'
plugin_function() { print -r -- "PLUGIN_FUNCTION:$PLUGIN_LOADED"; }
_plugin_complete() { compadd alpha beta; }
compdef _plugin_complete plugin_function
_plugin_widget() { LBUFFER+="widget"; }
zle -N plugin-widget _plugin_widget
bindkey '^X^P' plugin-widget
_plugin_precmd() { PLUGIN_PROMPTS=$(( ${PLUGIN_PROMPTS:-0} + 1 )); }
precmd_functions+=(_plugin_precmd)
''')
                    ready = b'ZSH_FIXTURE>'
                else:
                    (user / '.bashrc').write_text('''
PS1='BASH_FIXTURE> '
export PLUGIN_LOADED=yes
alias plugin_alias='printf "PLUGIN_ALIAS_OK\\n"'
plugin_function() { printf 'PLUGIN_FUNCTION:%s\\n' "$PLUGIN_LOADED"; }
''')
                    ready = b'BASH_FIXTURE>'
                env = {'HOME': str(user), 'ZDOTDIR': str(user), 'SHELL': shell,
                       'CALLER_VALUE': 'fresh from caller', 'TERM': 'xterm-256color', 'skip_global_compinit': '1'}
                one = Attached(home, options=['--cwd', str(work), '--env', 'OVERRIDE=space 中文 = value'], env=env)
                clients.append(one)
                one.until(ready)
                sid = re.search(rb'AgentBox session: ([0-9a-f]{32})', one.output).group(1).decode()
                session = next(s for s in data('sessions') if s['id'] == sid)
                assert session['shell']['mode'] == 'native'
                assert session['shell']['environment_source'] == 'client_snapshot'
                assert session['status'] == 'interactive'
                command(one, 'printf "ENV:%s|%s|%s|%s\\n" "$CALLER_VALUE" "$OVERRIDE" "${BROKER_ONLY-unset}" "$TERM"',
                        'ENV:fresh from caller|space 中文 = value|unset|xterm-256color')
                command(one, 'plugin_alias; plugin_function', 'PLUGIN_FUNCTION:yes')
                assert b'PLUGIN_ALIAS_OK\r\n' in one.output
                if kind == 'zsh':
                    command(one, 'print -r -- "ZSH_PLUGIN:${_comps[plugin_function]}:${widgets[plugin-widget]}:${precmd_functions[(Ie)_plugin_precmd]}"',
                            'ZSH_PLUGIN:_plugin_complete:user:_plugin_widget:1')
                    command(one, 'print -r -- "EDIT:\x18\x10"', 'EDIT:widget')
                print(f'PASS: native {kind} loads startup, aliases/functions and current caller environment', flush=True)
                # Interrupt actual foreground work without replacing the shell.
                start = len(one.output)
                one.send(b'printf "INTERRUPT_%s\\n" ready; sleep 30\r')
                one.until(b'INTERRUPT_ready\r\n', start=start)
                data('interrupt', sid)
                command(one, 'printf "AFTER_INTERRUPT:%s\\n" "$PLUGIN_LOADED"', 'AFTER_INTERRUPT:yes')

                command(one, 'LOCAL_VALUE=kept; export EXPORTED_VALUE=shared; live_function(){ printf "LIVE:%s:%s\\n" "$LOCAL_VALUE" "$EXPORTED_VALUE"; }; cd child; sleep 60 & printf \"STATE_%s\\n\" ready', 'STATE_ready')
                # Prove state with independent client input before detaching.
                start = len(one.output)
                data('input', sid, 'live_function; printf "STATE_PID:%s:%s\\n" "$$" "$PWD"', '--newline')
                one.until(('LIVE:kept:shared').encode(), start=start)
                one.until((':' + str(work / 'child')).encode(), start=start)
                shell_pid = session['pid']
                one.send(b'\x1d')
                one.finished()
                two = Attached(home, session=sid, env={'CALLER_VALUE': 'wrong-on-reattach'})
                clients.append(two)
                two.until(b'Ctrl-] detaches')
                command(two, 'live_function; printf "REATTACH:%s:%s:%s\\n" "$$" "$CALLER_VALUE" "$PWD"; jobs',
                        f'REATTACH:{shell_pid}:fresh from caller:{work / "child"}')
                two.until(b'sleep 60')
                # Structured execution must never inject wrappers into interactive shells.
                cli('run', sid, 'touch should-not-run', ok=False)
                assert not (work / 'child' / 'should-not-run').exists()
                cli('attach', sid, '--shell', shell, ok=False)
                page = data('read', sid, '--max-bytes', '1048576')
                assert 'LIVE:kept:shared' in page['output']
                command(two, 'kill %1; wait; exit', '')
                two.finished()
                print(f'PASS: {kind} reconnect keeps PID, unexported/exported variables, functions, cwd and job', flush=True)
            # Explicit clean mode skips user configuration for both supported shells.
            for kind in ['bash', 'zsh']:
                shell = shutil.which(kind)
                sid = cli('create', 'clean-'+kind, '--shell', shell, '--no-rc', '--inherit-env',
                    env={'HOME':str(user),'ZDOTDIR':str(user),'skip_global_compinit':'1'}).stdout.strip()
                data('input',sid,'printf "CLEAN:%s\\n" "${PLUGIN_LOADED-unset}"','--newline')
                deadline=time.monotonic()+5
                while True:
                    page=data('read',sid,'--max-bytes','1048576')
                    if 'CLEAN:unset' in page['output']: break
                    assert time.monotonic()<deadline,page['output']
                    time.sleep(.03)
                data('close',sid)
            print('PASS: foreground interrupts preserve native shells and clean mode skips user startup files',flush=True)
            broker_sid=cli('create','broker-environment','--shell',shutil.which('bash'),'--no-rc','--broker-env').stdout.strip()
            data('input',broker_sid,'printf "BROKER_ENV:%s:%s\\n" "$TERM" "$CALLER_VALUE"','--newline')
            deadline=time.monotonic()+5
            while True:
                page=data('read',broker_sid)
                if 'BROKER_ENV:vt100:stale' in page['output']: break
                assert time.monotonic()<deadline,page['output']
                time.sleep(.03)
            data('close',broker_sid)
            print('PASS: explicit broker environment preserves its TERM and values instead of silently overriding them',flush=True)
            # No environment values should be placed in durable session metadata.
            assert 'fresh from caller' not in json.dumps(data('sessions'))
            print('PASS: environment values are absent from session metadata', flush=True)
        finally:
            for client in clients:
                client.dispose()
            cli('stop', '--shutdown-sessions', '--broker-only')


if __name__ == '__main__':
    main()
