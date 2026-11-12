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
import time

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
        (user / '.zshrc').write_text("RC_ORDER+=:rc\nPROMPT='CHILD> '\nalias local_alias='print rc-alias'\nsetopt autocd\nunsetopt extendedglob\nexport RESTORE_ME=changed_by_rc\n")
        (user / '.zlogin').write_text('RC_ORDER+=:login\n')
        env = {**os.environ, 'HOME':str(user),'ZDOTDIR':str(user),'PATH':str(BIN.parent)+os.pathsep+os.environ['PATH'], 'SHELL':zsh,
               'PROMPT':'PARENT> ', 'skip_global_compinit':'1', 'LC_ALL': os.environ.get('AGENTBOX_TEST_LOCALE', 'en_US.UTF-8' if os.uname().sysname == 'Darwin' else 'C.UTF-8')}
        parent = module.Attached(home, env=env, argv=[zsh, '-f', '-i'])
        def cli(*args):
            p = subprocess.run([str(BIN),'--home',str(home),*args],text=True,capture_output=True,timeout=20)
            assert p.returncode==0,(p.stdout,p.stderr)
            return json.loads(p.stdout)
        try:
            parent.until(b'PARENT>')
            setup = f'''source {ROOT / 'shell/integration.zsh'}; SNAP_TEXT='space 中文 $quoted'; export RESTORE_ME='from parent'; typeset -a SNAP_ARRAY=('one two' three); typeset -A SNAP_MAP=(key 'value with spaces'); snap_function() {{ print -r -- "FUNC:$SNAP_TEXT"; }}; alias local_alias='print LOCAL_ALIAS'; alias -g AB_GLOBAL='| cat'; alias -s abtxt=cat; unsetopt autocd; setopt extendedglob; print SETUP_DONE'''
            parent.send(setup.encode()+b'\r')
            parent.until(b'SETUP_DONE\r\n')
            start=len(parent.output)
            parent.send(f'agentbox-here --var SNAP_TEXT --var SNAP_ARRAY --var SNAP_MAP --function snap_function --export RESTORE_ME --option autocd --option extendedglob -- --login --home {home}\r'.encode())
            parent.until(b'AgentBox session:',start=start)
            parent.until(b'CHILD>',start=start)
            sid=re.search(rb'AgentBox session: ([0-9a-f]{32})',parent.output[start:]).group(1).decode()
            start=len(parent.output)
            parent.send(b'print -r -- "COPY:$SNAP_TEXT:$SNAP_ARRAY[1]:$SNAP_MAP[key]:$RC_ORDER:$options[autocd]:$options[extendedglob]:$ZDOTDIR:$RESTORE_ME"; snap_function; local_alias; print -r -- "ALIASES:$galiases[AB_GLOBAL]:$saliases[abtxt]"\r')
            parent.until(f'COPY:space 中文 $quoted:one two:value with spaces:env:profile:rc:login:off:on:{user}:from parent'.encode(),start=start)
            parent.until('FUNC:space 中文 $quoted'.encode(),start=start)
            parent.until(b'LOCAL_ALIAS\r\n',start=start)
            parent.until(b'ALIASES:| cat:cat\r\n',start=start)
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
            def create_case(label, state_source, config, expected=None, zdot=True):
                case_user = base / label
                case_user.mkdir()
                for name, source in config.items():
                    (case_user/name).write_text(source)
                state_file = base / (label + '.zsh')
                state_file.write_text(state_source)
                state_file.chmod(0o600)
                case_env = {**env, 'HOME':str(case_user)}
                if zdot: case_env['ZDOTDIR']=str(case_user)
                else: case_env.pop('ZDOTDIR',None)
                result=subprocess.run([str(BIN),'--home',str(home),'create',label,'--shell',zsh,
                    '--inherit-env','--state-file',str(state_file)],env=case_env,capture_output=True,text=True,timeout=15)
                if expected is None:
                    assert result.returncode != 0,(label,result.stdout,result.stderr)
                    failed=next(s for s in cli('sessions') if s['name']==label)
                    assert failed['status']=='closed' and failed['shell']['state_imported'] is False
                else:
                    assert result.returncode==0,(label,result.stdout,result.stderr)
                    case_id=result.stdout.strip()
                    cli('input',case_id,'print -r -- "CASE:$CASE_VALUE:$RC_ORDER:${ZDOTDIR-unset}"','--newline')
                    deadline=time.monotonic()+5
                    while True:
                        page=cli('read',case_id,'--max-bytes','1048576')
                        if expected in page['output']: break
                        assert time.monotonic()<deadline,(label,page['output'])
                        time.sleep(.03)
                    cli('close',case_id)
                assert not list((home/'runtime').glob('.zsh-state-*'))
            create_case('nonlogin', 'CASE_VALUE=loaded\n',
                {'.zshenv':'RC_ORDER=env\n','.zshrc':'RC_ORDER+=:rc\nreturn 7\n'},
                f'CASE:loaded:env:rc:{base / "nonlogin"}')
            create_case('unset-zdot', 'CASE_VALUE=loaded\n',
                {'.zshenv':'RC_ORDER=env\n','.zshrc':'RC_ORDER+=:rc\n'},
                'CASE:loaded:env:rc:unset',zdot=False)
            relocated=base/'relocated'
            relocated.mkdir()
            (relocated/'.zshrc').write_text('RC_ORDER+=:relocated\n')
            create_case('redirect-zdot','CASE_VALUE=loaded\n',
                {'.zshenv':f'RC_ORDER=env\nZDOTDIR={relocated}\n'},
                f'CASE:loaded:env:relocated:{relocated}')
            create_case('disable-rcs','CASE_VALUE=loaded\n',
                {'.zshenv':'RC_ORDER=env\nunsetopt rcs\n','.zshrc':'RC_ORDER=wrong\n'},
                f'CASE:loaded:env:{base / "disable-rcs"}')
            create_case('import-fails','false\n',{},None)
            create_case('import-syntax','typeset broken=(\n',{},None)
            create_case('readonly-conflict','typeset CASE_VALUE=new || return\nbuiltin true\n',
                {'.zshrc':'typeset -r CASE_VALUE=old\n'},None)
            print('PASS: non-login/returning startup, unset/redirected ZDOTDIR and disabled RCS preserve intended state',flush=True)
            print('PASS: failed, malformed and readonly-conflicting imports close the new shell and clean private files',flush=True)
        finally:
            parent.dispose()
            if home.exists(): cli('stop','--shutdown-sessions','--broker-only')

if __name__=='__main__': main()
