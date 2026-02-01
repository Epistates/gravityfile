# Shell integration for gravityfile (cd on exit)
# Add this file to your fish functions directory:
#   cp grav.fish ~/.config/fish/functions/grav.fish
#
# Then use `grav` command - when you quit, your shell will cd to your last location.

function grav
    set -l tmp (mktemp -t "gravityfile-cwd.XXXXXX")
    command gravityfile --cwd-file="$tmp" $argv
    set -l exit_code $status
    if test -f "$tmp"
        set -l cwd (cat "$tmp")
        if test -d "$cwd"; and test "$cwd" != "$PWD"
            cd "$cwd"
        end
        rm -f "$tmp"
    end
    return $exit_code
end
