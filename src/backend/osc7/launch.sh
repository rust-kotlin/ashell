# Runs on the terminal PTY. Only supported login shells need a startup file.
case "${SHELL##*/}" in
    bash) rc=bash-init ;;
    zsh) rc=.zshenv ;;
    *) exec "${SHELL:-/bin/sh}" -l ;;
esac
dir=$(mktemp -d /tmp/ashell-osc7.XXXXXXXX) || exec "$SHELL" -l
# Also clean up when a system profile skips the user's startup files.
trap 'rm -f "$dir/$rc"; rmdir "$dir"' 0
trap 'exit 129' HUP
trap 'exit 143' TERM

case "$rc" in
    bash-init)
        cat > "$dir/$rc" <<'ASHELL_BASH_INIT' || { "$SHELL" -l; exit $?; }
@BASH_INIT@
ASHELL_BASH_INIT
        if [ "${ENV+x}" ]; then export ASHELL_PREVIOUS_ENV="$ENV"; fi
        if [ "${PROMPT_COMMAND+x}" ]; then export ASHELL_PREVIOUS_PROMPT_COMMAND="$PROMPT_COMMAND"; fi
        # ENV is the POSIX interactive startup file. Bash 3.2 needs the
        # first-prompt fallback. Both routes discard the temporary hook.
        ENV="$dir/$rc" PROMPT_COMMAND='__ashell_first_prompt=1; . "$ENV"' "$SHELL" --noprofile --posix -il
        ;;
    .zshenv)
        cat > "$dir/$rc" <<'ASHELL_ZSH_INIT' || { "$SHELL" -l; exit $?; }
@ZSH_INIT@
ASHELL_ZSH_INIT
        if [ "${ZDOTDIR+x}" ]; then export ASHELL_PREVIOUS_ZDOTDIR="$ZDOTDIR"; fi
        ZDOTDIR="$dir" "$SHELL" -il
        ;;
esac
