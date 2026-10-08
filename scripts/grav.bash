# Shell integration for gravityfile (cd on exit)
# Source this file in your .bashrc or .bash_profile:
#   source /path/to/scripts/grav.bash
#
# Then use `grav` command - when you quit, your shell will cd to your last location.

grav() {
    local tmp
    tmp="$(mktemp -t "gravityfile-cwd.XXXXXX")" || return $?
    command gravityfile --cwd-file="$tmp" "$@"
    local exit_code=$?
    if [[ $exit_code -eq 0 && -f "$tmp" ]]; then
        # A sentinel prevents command substitution from stripping path newlines.
        local cwd
        if cwd="$(cat -- "$tmp" && printf .)"; then
            cwd="${cwd%.}"
            if [[ -d "$cwd" ]] && [[ "$cwd" != "$PWD" ]]; then
                cd -- "$cwd" || true
            fi
        fi
    fi
    rm -f -- "$tmp"
    return $exit_code
}
