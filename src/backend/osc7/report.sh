__ashell_report_directory() {
    local __ashell_status=$?
    [[ ${__ashell_last_directory-} != "$PWD" ]] || return "$__ashell_status"
    __ashell_last_directory=$PWD
    local LC_ALL=C rest="$PWD" char encoded='' octet
    while [[ -n $rest ]]; do
        char=${rest:0:1}
        rest=${rest:1}
        case "$char" in
            [a-zA-Z0-9/._~-]) encoded+="$char" ;;
            *) builtin printf -v octet '%02X' "'$char"; encoded+="%${octet: -2}" ;;
        esac
    done
    builtin printf '\033]7;file://%s\007' "$encoded"
    return "$__ashell_status"
}
