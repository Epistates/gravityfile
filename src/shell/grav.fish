# gravityfile shell integration: `grav` changes to the explorer's last
# directory when it exits. Add to ~/.config/fish/config.fish:
#   command grav init fish | source

function grav
    set -l tmp (mktemp -t "gravityfile-cwd.XXXXXX")
    or return $status
    command grav --cwd-file="$tmp" $argv
    set -l exit_code $status
    if test $exit_code -eq 0; and test -f "$tmp"
        set -l cwd
        read --null cwd < "$tmp"
        if test -d "$cwd"; and test "$cwd" != "$PWD"
            cd "$cwd"
        end
    end
    rm -f -- "$tmp"
    return $exit_code
end
