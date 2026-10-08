# Shell integration for gravityfile (cd on exit)
# Add this file to your fish functions directory:
#   cp grav.fish ~/.config/fish/functions/grav.fish
#
# Then use `grav` command - when you quit, your shell will cd to your last location.

function grav
    set -l tmp (mktemp -t "gravityfile-cwd.XXXXXX")
    or return $status
    command gravityfile --cwd-file="$tmp" $argv
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
