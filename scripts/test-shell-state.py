#!/usr/bin/env python3
"""Real parent Zsh -> optional integration -> native shared Zsh state handoff."""
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get('AGENTBOX_TEST_BIN', ROOT / 'target/debug/agentbox')).resolve()
spec = importlib.util.spec_from_file_location('attach_test', ROOT / 'scripts/test-attach.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def main():
    with tempfile.TemporaryDirectory(prefix='ab-state-') as temp:
        base = pathlib.Path(temp).resolve()
        user = base / 'user'
        user.mkdir()
        home = base / 'state'
        zsh = shutil.which('zsh')
        assert zsh
        (user / '.zshenv').write_text('RC_ORDER=env\n')
        (user / '.zprofile').write_text('RC_ORDER+=:profile\n')
        (user / '.zshrc').write_text("RC_ORDER+=:rc\nPROMPT='CHILD> '\nalias local_alias='print rc-alias'\nsetopt autocd\nunsetopt extendedglob\n")
        (user / '.zlogin').write_text('RC_ORDER+=:login\n')
        env = {**os.environ, 'HOME':str(user),'ZDOTDIR':str(user),'PATH':str(BIN.parent)+os.pathsep+os.environ['PATH'], 'SHELL':zsh,
               'PROMPT':'PARENT> ', 'skip_global_compinit':'1', 'LC_ALL': 'en_US.UTF-8' if os.uname().sysname == 'Darwin' else 'C.UTF-8'}
        parent = module.Attached(home, env=env, argv=[zsh, '-f', '-i'])
        def cli(*args):
            p = subprocess.run([str(BIN),'--home',str(home),*args],text=True,capture_output=True,timeout=20)
            assert p.returncode==0,(p.stdout,p.stderr)
            return json.loads(p.stdout)
        try:
            parent.until(b'PARENT>')
            setup = f'''source {ROOT / 'shell/integration.zsh'}; SNAP_TEXT='space 中文 $quoted'; typeset -a SNAP_ARRAY=('one two' three); typeset -A SNAP_MAP=(key 'value with spaces'); snap_function() {{ print -r -- "FUNC:$SNAP_TEXT"; }}; alias local_alias='print LOCAL_ALIAS'; unsetopt autocd; setopt extendedglob; print SETUP_DONE'''
            parent.send(setup.encode()+b'\r')
            parent.until(b'SETUP_DONE\r\n')
            start=len(parent.output)
            parent.send(f'agentbox-here --var SNAP_TEXT --var SNAP_ARRAY --var SNAP_MAP --function snap_function --option autocd --option extendedglob -- --login --home {home}\r'.encode())
            parent.until(b'AgentBox session:',start=start)
            parent.until(b'CHILD>',start=start)
            sid=re.search(rb'AgentBox session: ([0-9a-f]{32})',parent.output[start:]).group(1).decode()
            start=len(parent.output)
            parent.send(b'print -r -- "COPY:$SNAP_TEXT:$SNAP_ARRAY[1]:$SNAP_MAP[key]:$RC_ORDER:$options[autocd]:$options[extendedglob]:$ZDOTDIR"; snap_function; local_alias\r')
            parent.until(f'COPY:space 中文 $quoted:one two:value with spaces:env:profile:rc:login:off:on:{user}'.encode(),start=start)
            parent.until('FUNC:space 中文 $quoted'.encode(),start=start)
            parent.until(b'LOCAL_ALIAS\r\n',start=start)
            assert not list((home/'runtime').glob('.zsh-state-*')), 'private import files should be removed after import'
            sessions=cli('sessions')
            session=next(s for s in sessions if s['id']==sid)
            assert session['shell']['state_imported'] is True
            assert 'space 中文 $quoted' not in json.dumps(sessions,ensure_ascii=False)
            parent.send(b'\x1d')
            start=len(parent.output)
            parent.until(b'PARENT>',start=start)
            parent.send(b'print -r -- "PARENT_STILL:$SNAP_TEXT"\r')
            parent.until('PARENT_STILL:space 中文 $quoted'.encode(),start=start)
            cli('close',sid)
            print('PASS: selected scalar/array/map/function, aliases and on/off options transfer from the live parent Zsh',flush=True)
            print('PASS: login startup order and ZDOTDIR are preserved; private import files are removed; parent remains usable',flush=True)
        finally:
            parent.dispose()
            if home.exists(): cli('stop','--shutdown-sessions','--broker-only')

if __name__=='__main__': main()
