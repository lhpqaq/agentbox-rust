#!/usr/bin/env python3
"""Opt-in real Oh My Zsh/autosuggestions/syntax-highlighting PTY acceptance test."""
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
    omz = pathlib.Path(os.environ.get('AGENTBOX_TEST_OMZ', pathlib.Path.home() / '.oh-my-zsh')).resolve()
    suggest = pathlib.Path(os.environ.get('AGENTBOX_TEST_AUTOSUGGEST', '/opt/homebrew/share/zsh-autosuggestions/zsh-autosuggestions.zsh')).resolve()
    highlight = pathlib.Path(os.environ.get('AGENTBOX_TEST_HIGHLIGHT', '/opt/homebrew/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh')).resolve()
    for path in [omz / 'oh-my-zsh.sh', suggest, highlight]:
        assert path.is_file(), f'Required plugin fixture missing: {path}; configure AGENTBOX_TEST_OMZ/AUTOSUGGEST/HIGHLIGHT'
    with tempfile.TemporaryDirectory(prefix='ab-plugins-') as temp:
        base = pathlib.Path(temp).resolve()
        user = base / 'user'
        user.mkdir()
        home = base / 'state'
        (user / '.zshrc').write_text('''
zstyle ':omz:update' mode disabled
ZSH_THEME=robbyrussell
plugins=(git)
ZSH_CACHE_DIR="$HOME/cache"
ZSH_COMPDUMP="$HOME/zcompdump"
ZSH_DISABLE_COMPFIX=true
source "$TEST_OMZ/oh-my-zsh.sh"
source "$TEST_SUGGEST"
unset ZSH_AUTOSUGGEST_USE_ASYNC
source "$TEST_HIGHLIGHT"
PROMPT='REAL_PLUGINS> '
''')
        env={'HOME':str(user),'ZDOTDIR':str(user),'SHELL':shutil.which('zsh'),
             'ZSH':str(omz),'TEST_OMZ':str(omz),'TEST_SUGGEST':str(suggest),'TEST_HIGHLIGHT':str(highlight),
             'skip_global_compinit':'1','DISABLE_AUTO_UPDATE':'true',
             'LC_ALL':'en_US.UTF-8' if os.uname().sysname=='Darwin' else 'C.UTF-8'}
        client=module.Attached(home,options=[],env=env)
        try:
            client.until(b'REAL_PLUGINS>',timeout=20)
            sid=re.search(rb'AgentBox session: ([0-9a-f]{32})',client.output).group(1).decode()
            start=len(client.output)
            client.send(b'print -r -- "LOADED:${+functions[_zsh_highlight]}:${+functions[_zsh_autosuggest_start]}:${aliases[gst]}:${_comps[git]}"\r')
            client.until(b'LOADED:1:1:git status:_git\r\n',start=start)
            # Seed a history entry, type its unique prefix, and accept the actual
            # ghost suggestion using End. Enter must execute the suggested tail.
            start=len(client.output)
            client.send(b"print -s -- 'print -r -- AUTOSUGGEST_ACCEPTED'; print SEED_DONE\r")
            client.until(b'SEED_DONE\r\n',start=start)
            start=len(client.output)
            client.send(b'print -r -- AUTO')
            client.until(b'SUGGEST_ACCEPTED',start=start)
            client.send(b'\x05\r')
            client.until(b'AUTOSUGGEST_ACCEPTED\r\n',start=start)
            # Highlighting is confirmed by the real plugin's hooked ZLE widget
            # and a color sequence on a newly typed command (not startup output).
            start=len(client.output)
            client.send(b'echo ')
            client.until(b'echo',start=start)
            deadline=time.monotonic()+3
            while b'\x1b[' not in client.output[start:] and time.monotonic()<deadline:
                import select
                if select.select([client.master],[],[],.1)[0]: client.output+=os.read(client.master,65536)
            assert re.search(rb'\x1b\[[0-9;]*m',client.output[start:]), client.output[start:]
            client.send(b'\x03')
            client.send(b'\x1d')
            client.finished()
            print('PASS: actual Oh My Zsh git plugin/theme, completion, autosuggestion acceptance and syntax colors work in native attach',flush=True)
            print('Plugins:',omz,suggest,highlight,flush=True)
        finally:
            client.dispose()
            p=subprocess.run([str(BIN),'--home',str(home),'stop','--shutdown-sessions','--broker-only'],capture_output=True,text=True,timeout=20)
            assert p.returncode==0,p.stderr

if __name__=='__main__': main()
