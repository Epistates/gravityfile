# gravityfile shell integration: `grav` changes to the explorer's last
# directory when it exits. Add to ~/.zshrc:
#   eval "$(command grav init zsh)"

grav() {
    local tmp
    tmp="$(mktemp -t "gravityfile-cwd.XXXXXX")" || return $?
    command grav --cwd-file="$tmp" "$@"
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
