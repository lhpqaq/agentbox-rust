# Evaluate with: eval "$(agentbox shell-init zsh)"
# No dotfiles are changed. Startup files still load in the new shared shell.
agentbox-here() {
  local -A __agentbox_saved_options=( "${(@kv)options}" )
  builtin emulate -L zsh
  builtin setopt localtraps
  local -a __agentbox_vars=() __agentbox_functions=() __agentbox_copy_options=() __agentbox_forward=()
  builtin local __agentbox_aliases=1 __agentbox_name __agentbox_type __agentbox_file __agentbox_result
  while (( $# )); do
    case "$1" in
      --var|--function)
        if (( $# < 2 )); then builtin print -u2 -- "agentbox-here: $1 requires a name"; return 2; fi
        __agentbox_name=$2
        if [[ $__agentbox_name == __agentbox_* || $__agentbox_name == -* || -z $__agentbox_name ]]; then
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
      --no-aliases) __agentbox_aliases=0; shift ;;
      --option)
        if (( $# < 2 )) || [[ -z ${__agentbox_saved_options[$2]-} || $2 == (interactive|login|shinstdin|singlecommand|restricted|privileged) ]]; then
          builtin print -u2 -- 'agentbox-here: --option requires an editable canonical Zsh option name'; return 2
        fi
        __agentbox_copy_options+=("$2"); shift 2 ;;
      --) shift; __agentbox_forward=("$@"); break ;;
      -h|--help)
        builtin print -r -- 'agentbox-here [--var NAME] [--function NAME] [--no-aliases] [--option NAME] -- [attach creation options]'
        builtin print -r -- 'Copies aliases by default, selected global variables/functions on request, and exported environment. Jobs stay in the original shell.'
        return 0 ;;
      *) builtin print -u2 -- 'agentbox-here: pass attach options after --'; return 2 ;;
    esac
  done
  __agentbox_file=$(command mktemp "${TMPDIR:-/tmp}/agentbox-zsh-state.XXXXXXXX") || return
  {
    {
      builtin print -r -- '# AgentBox explicit state snapshot; trusted shell code.'
      for __agentbox_name in "${__agentbox_vars[@]}"; do
        builtin typeset -p -- "$__agentbox_name" || return
      done
      for __agentbox_name in "${__agentbox_functions[@]}"; do
        builtin functions -- "$__agentbox_name" || return
      done
      # Aliases go last so they cannot rewrite the serialized declarations.
      if (( __agentbox_aliases )); then builtin alias -L; fi
      for __agentbox_name in "${__agentbox_copy_options[@]}"; do
        if [[ $__agentbox_saved_options[$__agentbox_name] == on ]]; then
          builtin printf 'builtin setopt -- %q\n' "$__agentbox_name"
        else
          builtin printf 'builtin unsetopt -- %q\n' "$__agentbox_name"
        fi
      done
      builtin print -r -- 'builtin true'
    } >| "$__agentbox_file"
    command agentbox attach --shell "$(builtin whence -p zsh)" --state-file "$__agentbox_file" --consume-state-file "${__agentbox_forward[@]}"
    __agentbox_result=$?
  } always {
    command rm -f -- "$__agentbox_file"
  }
  return $__agentbox_result
}
