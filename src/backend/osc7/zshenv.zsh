if (( ${+ASHELL_PREVIOUS_ZDOTDIR} )); then
    export ZDOTDIR="$ASHELL_PREVIOUS_ZDOTDIR"
    unset ASHELL_PREVIOUS_ZDOTDIR
else
    unset ZDOTDIR
fi
if [[ -r ${ZDOTDIR-$HOME}/.zshenv ]]; then source "${ZDOTDIR-$HOME}/.zshenv"; fi

@REPORT@
__ashell_zsh_report() {
    __ashell_report_directory
    # Zsh preserves the command status across precmd hooks, but a hook that
    # returns a failure prevents later hooks from running.
    return 0
}
__ashell_install_directory_hook() {
    typeset -ga precmd_functions
    precmd_functions=(__ashell_zsh_report "${precmd_functions[@]}")
    __ashell_report_directory
    unfunction __ashell_install_directory_hook
}
# Install after the user's theme has finished setting up its prompt hooks.
if zmodload zsh/sched 2>/dev/null; then
    sched +0 __ashell_install_directory_hook
else
    typeset -ga precmd_functions
    precmd_functions=(__ashell_zsh_report "${precmd_functions[@]}")
    unfunction __ashell_install_directory_hook
fi
