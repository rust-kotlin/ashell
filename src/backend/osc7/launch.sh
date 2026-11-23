# Runs on the terminal PTY. Only supported login shells need a startup file.
case "${SHELL##*/}" in
    bash|brush) rc=bash-init ;;
    zsh) rc=.zshenv ;;
    *) exec "${SHELL:-/bin/sh}" -l ;;
esac
dir=$(mktemp -d /tmp/ashell-osc7.XXXXXXXX) || exec "$SHELL" -l
case "$dir" in
    /tmp/ashell-osc7.????????) ;;
    *) exec "$SHELL" -l ;;
esac
case "${dir#/tmp/ashell-osc7.}" in
    *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]*) exec "$SHELL" -l ;;
esac
[ -d "$dir" ] && [ ! -L "$dir" ] || exec "$SHELL" -l
readonly dir rc
# Also clean up when a system profile skips the user's startup files.
trap 'if [ -d "$dir" ] && [ ! -L "$dir" ]; then rm -f "$dir/$rc"; rmdir "$dir"; fi' 0
trap 'exit 129' HUP
trap 'exit 143' TERM

case "$rc" in
    bash-init)
        cat > "$dir/$rc" <<'ASHELL_BASH_INIT' || { "$SHELL" -l; exit $?; }
@BASH_INIT@
ASHELL_BASH_INIT
        if [ "${ENV+x}" ]; then export ASHELL_PREVIOUS_ENV="$ENV"; fi
        if [ "${PROMPT_COMMAND+x}" ]; then export ASHELL_PREVIOUS_PROMPT_COMMAND="$PROMPT_COMMAND"; fi
        # Bash can source ENV in POSIX mode. Brush loads the same file from
        # the first prompt in its native mode, as does the older Bash fallback.
        set -- --noprofile
        if [ "${SHELL##*/}" = bash ]; then set -- "$@" --posix; fi
        ENV="$dir/$rc" PROMPT_COMMAND='__ashell_first_prompt=1; . "$ENV"' "$SHELL" "$@" -il
        ;;
    .zshenv)
        cat > "$dir/$rc" <<'ASHELL_ZSH_INIT' || { "$SHELL" -l; exit $?; }
@ZSH_INIT@
ASHELL_ZSH_INIT
        if [ "${ZDOTDIR+x}" ]; then export ASHELL_PREVIOUS_ZDOTDIR="$ZDOTDIR"; fi
        ZDOTDIR="$dir" "$SHELL" -il
        ;;
esac
