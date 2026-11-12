# Evaluate with: eval "$(agentbox shell-init zsh)"
# No dotfiles are changed. Startup files still load in the new shared shell.
agentbox-here() {
  local -A __agentbox_saved_options=( "${(@kv)options}" )
  builtin emulate -L zsh
  builtin setopt localtraps extendedglob
  local -a __agentbox_vars=() __agentbox_functions=() __agentbox_exports=() __agentbox_copy_options=() __agentbox_forward=()
  builtin local __agentbox_aliases=1 __agentbox_name __agentbox_type __agentbox_file __agentbox_result __agentbox_declaration
  while (( $# )); do
    case "$1" in
      --var|--function)
        if (( $# < 2 )); then builtin print -u2 -- "agentbox-here: $1 requires a name"; return 2; fi
        __agentbox_name=$2
        if [[ $__agentbox_name == (__agentbox_*|AGENTBOX_TOKEN|AGENTBOX_SESSION) || $__agentbox_name == -* || -z $__agentbox_name ]]; then
          builtin print -u2 -- 'agentbox-here: invalid or reserved state name'; return 2
        fi
        if [[ $1 == --var ]]; then
          __agentbox_type=${parameters[$__agentbox_name]-}
          if [[ -z $__agentbox_type || $__agentbox_type == *special* || $__agentbox_type == *local* ]]; then
            builtin print -u2 -- 'agentbox-here: variable must be a defined, non-special global parameter'; return 2
          fi
          __agentbox_vars+=("$__agentbox_name")
        else
          if (( ! ${+functions[$__agentbox_name]} )); then builtin print -u2 -- 'agentbox-here: function is not defined'; return 2; fi
          __agentbox_functions+=("$__agentbox_name")
        fi
        shift 2 ;;
      --export)
        if (( $# < 2 )); then builtin print -u2 -- 'agentbox-here: --export requires a name'; return 2; fi
        __agentbox_name=$2
        __agentbox_type=${parameters[$__agentbox_name]-}
        if [[ $__agentbox_name == (__agentbox_*|AGENTBOX_TOKEN|AGENTBOX_SESSION|PWD|OLDPWD|SHLVL|_) || $__agentbox_name != [a-zA-Z_][a-zA-Z0-9_]# || $__agentbox_type != *scalar* || $__agentbox_type != *export* || $__agentbox_type == *local* ]]; then
          builtin print -u2 -- 'agentbox-here: --export requires an exported scalar environment variable'; return 2
        fi
        __agentbox_exports+=("$__agentbox_name"); shift 2 ;;
      --no-aliases) __agentbox_aliases=0; shift ;;
      --option)
        if (( $# < 2 )) || [[ -z ${__agentbox_saved_options[$2]-} || $2 == (interactive|login|shinstdin|singlecommand|restricted|privileged) ]]; then
          builtin print -u2 -- 'agentbox-here: --option requires an editable canonical Zsh option name'; return 2
        fi
        __agentbox_copy_options+=("$2"); shift 2 ;;
      --) shift; __agentbox_forward=("$@"); break ;;
      -h|--help)
        builtin print -r -- 'agentbox-here [--var NAME] [--export NAME] [--function NAME] [--no-aliases] [--option NAME] -- [attach creation options]'
        builtin print -r -- 'Copies aliases by default, selected global variables/functions on request, and exported environment. Jobs stay in the original shell.'
        return 0 ;;
      *) builtin print -u2 -- 'agentbox-here: pass attach options after --'; return 2 ;;
    esac
  done
  __agentbox_file=$(command mktemp "${TMPDIR:-/tmp}/agentbox-zsh-state.XXXXXXXX") || return
  {
    (
      # Zsh's C-locale typeset printer can emit mixed raw bytes and escapes.
      # Use UTF-8 only while serializing; the launched shell keeps caller locale.
      builtin local __agentbox_utf8 __agentbox_candidate
      for __agentbox_candidate in C.UTF-8 C.utf8 en_US.UTF-8; do
        if [[ $(LC_ALL=$__agentbox_candidate command locale charmap 2>/dev/null) == UTF-8 ]]; then
          __agentbox_utf8=$__agentbox_candidate; break
        fi
      done
      if [[ -z $__agentbox_utf8 ]]; then
        builtin print -u2 -- 'agentbox-here: a UTF-8 locale is required to serialize shell state'; return 2
      fi
      builtin local -x LC_ALL=$__agentbox_utf8
      builtin print -r -- '# AgentBox explicit state snapshot; trusted shell code.'
      for __agentbox_name in "${__agentbox_vars[@]}"; do
        __agentbox_declaration=$(builtin typeset -p -- "$__agentbox_name") || return
        builtin printf '%s || return\n' "$__agentbox_declaration"
      done
      for __agentbox_name in "${__agentbox_exports[@]}"; do
        builtin printf 'builtin export %q=%q || return\n' "$__agentbox_name" "${(P)__agentbox_name}"
      done
      for __agentbox_name in "${__agentbox_functions[@]}"; do
        __agentbox_declaration=$(builtin functions -- "$__agentbox_name") || return
        builtin printf '%s || return\n' "$__agentbox_declaration"
      done
      # Aliases go last so they cannot rewrite the serialized declarations.
      if (( __agentbox_aliases )); then
        for __agentbox_name in "${(@k)aliases}"; do
          builtin printf 'builtin alias -- %q=%q || return\n' "$__agentbox_name" "$aliases[$__agentbox_name]"
        done
        for __agentbox_name in "${(@k)galiases}"; do
          builtin printf 'builtin alias -g -- %q=%q || return\n' "$__agentbox_name" "$galiases[$__agentbox_name]"
        done
        for __agentbox_name in "${(@k)saliases}"; do
          builtin printf 'builtin alias -s -- %q=%q || return\n' "$__agentbox_name" "$saliases[$__agentbox_name]"
        done
      fi
      for __agentbox_name in "${__agentbox_copy_options[@]}"; do
        if [[ $__agentbox_saved_options[$__agentbox_name] == on ]]; then
          builtin printf 'builtin setopt -- %q || return\n' "$__agentbox_name"
        else
          builtin printf 'builtin unsetopt -- %q || return\n' "$__agentbox_name"
        fi
      done
      builtin print -r -- 'builtin true'
    ) >| "$__agentbox_file" || return
    command agentbox attach --shell "$(builtin whence -p zsh)" --state-file "$__agentbox_file" --consume-state-file "${__agentbox_forward[@]}"
    __agentbox_result=$?
  } always {
    command rm -f -- "$__agentbox_file"
  }
  return $__agentbox_result
}
