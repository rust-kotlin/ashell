__ashell_report_directory() {
    local __ashell_status=$?
    [[ ${__ashell_last_directory-} != "$PWD" ]] || return "$__ashell_status"
    local LC_ALL=C rest="$PWD" char encoded='' octet
    if [[ $rest != *[!a-zA-Z0-9/._~-]* ]]; then
        encoded=$rest
    elif [[ -n ${BRUSH_VERSION-} ]]; then
        # Brush strings are UTF-8. Percent-encode the path's bytes.
        encoded=$(
            set -o pipefail
            builtin printf '%s' "$rest" | command od -An -v -tu1 | LC_ALL=C command awk '{
                for (i = 1; i <= NF; i++) {
                    c = sprintf("%c", $i)
                    if (c ~ /^[a-zA-Z0-9\/._~-]$/) printf "%s", c
                    else printf "%%%02X", $i
                }
            }'
        ) || return "$__ashell_status"
    else
        while [[ -n $rest ]]; do
            char=${rest:0:1}
            rest=${rest:1}
            case "$char" in
                [a-zA-Z0-9/._~-]) encoded+="$char" ;;
                *) builtin printf -v octet '%02X' "'$char"; encoded+="%${octet: -2}" ;;
            esac
        done
    fi
    builtin printf '\033]7;file://%s\007' "$encoded"
    __ashell_last_directory=$PWD
    return "$__ashell_status"
}
