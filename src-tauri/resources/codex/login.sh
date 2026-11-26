#!/bin/sh
# maiTerm remote Codex login wrapper (installed to ~/.maiterm/codex/login.sh).
# Preserves the user's normal login shell and startup files while defining a
# `codex` function that routes interactive launches through launch.sh.
shell_bin="${SHELL:-/bin/sh}"
body='if [ -r "$HOME/.maiterm/codex/launch.sh" ]; then /bin/sh "$HOME/.maiterm/codex/launch.sh" "$@"; else command codex "$@"; fi'
case "${shell_bin##*/}" in
    bash)
        exec env "BASH_FUNC_codex%%=() { ${body}; }" "$shell_bin" -l
        ;;
    fish)
        exec "$shell_bin" -l -C 'function codex; if test -r "$HOME/.maiterm/codex/launch.sh"; /bin/sh "$HOME/.maiterm/codex/launch.sh" $argv; else; command codex $argv; end; end'
        ;;
    zsh)
        dir="$HOME/.maiterm/codex/zsh"
        if mkdir -p "$dir" 2>/dev/null && tmp=$(mktemp "$dir/.zshenv.XXXXXX" 2>/dev/null); then
            cat > "$tmp" <<'MAITERM_ZSHENV_EOF'
ZDOTDIR="$MAITERM_ORIGINAL_ZDOTDIR"
unset MAITERM_ORIGINAL_ZDOTDIR
function codex {
  if [[ -r "$HOME/.maiterm/codex/launch.sh" ]]; then
    /bin/sh "$HOME/.maiterm/codex/launch.sh" "$@"
  else
    command codex "$@"
  fi
}
[[ ! -r "$ZDOTDIR/.zshenv" ]] || source "$ZDOTDIR/.zshenv"
MAITERM_ZSHENV_EOF
            if mv -f "$tmp" "$dir/.zshenv" 2>/dev/null; then
                export MAITERM_ORIGINAL_ZDOTDIR="${ZDOTDIR:-$HOME}"
                export ZDOTDIR="$dir"
            else
                rm -f "$tmp" 2>/dev/null || true
            fi
        fi
        exec "$shell_bin" -l
        ;;
    *)
        exec "$shell_bin" -l
        ;;
esac
