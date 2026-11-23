# Bash entered POSIX mode to load ENV. Brush is already in its native mode.
if [[ ${SHELL##*/} == bash ]]; then set +o posix; fi
if [[ ${ASHELL_PREVIOUS_PROMPT_COMMAND+x} ]]; then
    PROMPT_COMMAND="$ASHELL_PREVIOUS_PROMPT_COMMAND"
    unset ASHELL_PREVIOUS_PROMPT_COMMAND
else
    unset PROMPT_COMMAND
fi
if [[ ${ASHELL_PREVIOUS_ENV+x} ]]; then
    export ENV="$ASHELL_PREVIOUS_ENV"
    unset ASHELL_PREVIOUS_ENV
else
    unset ENV
fi
if [[ -r /etc/profile ]]; then source /etc/profile; fi
for __ashell_profile in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [[ -r $__ashell_profile ]]; then source "$__ashell_profile"; break; fi
done
unset __ashell_profile

@REPORT@
# Prepend the hook: it restores the exit status seen by the user's first hook.
__ashell_report_directory
case $(declare -p PROMPT_COMMAND 2>/dev/null) in
    'declare -a'*) PROMPT_COMMAND=(__ashell_report_directory "${PROMPT_COMMAND[@]}") ;;
    *) PROMPT_COMMAND="__ashell_report_directory"${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND} ;;
esac
# Brush and older Bash first enter this file from the prompt command. Run the user's
# configured prompt commands for that initial prompt as well.
if [[ ${__ashell_first_prompt-} == 1 ]]; then
    unset __ashell_first_prompt
    for __ashell_hook in "${PROMPT_COMMAND[@]}"; do
        builtin eval -- "$__ashell_hook"
    done
    unset __ashell_hook
fi
