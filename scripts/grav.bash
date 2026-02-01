# Shell integration for gravityfile (cd on exit)
# Source this file in your .bashrc or .bash_profile:
#   source /path/to/scripts/grav.bash
#
# Then use `grav` command - when you quit, your shell will cd to your last location.

grav() {
    local tmp="$(mktemp -t "gravityfile-cwd.XXXXXX")"
    command gravityfile --cwd-file="$tmp" "$@"
    local exit_code=$?
    if [[ -f "$tmp" ]]; then
        local cwd="$(cat "$tmp")"
        if [[ -d "$cwd" ]] && [[ "$cwd" != "$PWD" ]]; then
            cd "$cwd" || true
        fi
        rm -f "$tmp"
    fi
    return $exit_code
}
