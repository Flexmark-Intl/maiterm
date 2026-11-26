import { quoteWord } from '$lib/utils/shellWords';

/** The background SSH setup installs launch.sh. Resolve it when the human runs
 * Codex, since the first shell can open before setup finishes (or if setup fails
 * on the remote). Uses a POSIX function so a missing launch.sh falls back cleanly
 * to the real `codex` binary rather than failing with ENOENT. */
const launch = '/bin/sh "$HOME/.maiterm/codex/launch.sh"';
const launchOrFallback = `if [ -r "$HOME/.maiterm/codex/launch.sh" ]; then ${launch} "$@"; else command codex "$@"; fi`;
export const remoteCodexAlias = `unalias codex 2>/dev/null; codex() { ${launchOrFallback}; }`;

/** A generated SSH command starts a new login shell. An alias set before exec
 * disappears there, and /etc/profile can reset PATH. Use each shell's startup
 * mechanism, preserving the normal login files. No change to the user's rc. */
export function remoteCodexLogin(): string {
  const body = launchOrFallback;
  const zshenv = [
    'ZDOTDIR=$MAITERM_ORIGINAL_ZDOTDIR',
    'unset MAITERM_ORIGINAL_ZDOTDIR',
    `function codex { ${body}; }`,
    '[[ ! -r "$ZDOTDIR/.zshenv" ]] || source "$ZDOTDIR/.zshenv"',
  ].join('; ');
  // zsh reads our one-line .zshenv, then the user's own startup files in order.
  // Use mktemp + rename since two tabs can log in at once.
  const startup = [
    'case "${SHELL##*/}" in',
    `bash) exec env ${quoteWord(`BASH_FUNC_codex%%=() { ${body}; }`)} "$SHELL" -l ;;`,
    `fish) exec "$SHELL" -l -C ${quoteWord('function codex; if test -r "$HOME/.maiterm/codex/launch.sh"; ' + launch + ' $argv; else; command codex $argv; end; end')} ;;`,
    'zsh) dir="$HOME/.maiterm/codex/zsh";',
    'mkdir -p "$dir" && tmp=$(mktemp "$dir/.zshenv.XXXXXX") &&',
    `printf "%s\\n" ${quoteWord(zshenv)} > "$tmp" && mv -f "$tmp" "$dir/.zshenv" &&`,
    'export MAITERM_ORIGINAL_ZDOTDIR="${ZDOTDIR:-$HOME}" ZDOTDIR="$dir" ;;',
    'esac; exec "$@"',
  ].join(' ');
  return 'exec /bin/sh -c ' + quoteWord(startup) + ' maiterm-codex-login $SHELL -l';
}
