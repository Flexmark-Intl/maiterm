/** The background SSH setup installs ~/.maiterm/codex/launch.sh. Resolve it
 * when the human runs Codex, since the first shell can open before setup finishes
 * (or if setup fails on the remote).
 *
 * Uses `alias codex='...'` with zero nested single quotes so:
 * - bash/zsh shells that ALREADY define `alias codex=...` overwrite it cleanly
 *   at runtime instead of failing to parse `codex() { ... }`,
 * - fish accepts the same syntax (`alias name='...'` with `\"` / `\$` inside
 *   double quotes), and
 * - a missing launch.sh falls back cleanly to `exec codex "$@"`.
 *
 * Callers MUST write this on its own line AFTER `export MAITERM_*` / account
 * handoff so any shell-specific issue can never abort the env export line. */
export const remoteCodexAlias =
  'alias codex=\'/bin/sh -c "if [ -r ~/.maiterm/codex/launch.sh ]; then exec /bin/sh ~/.maiterm/codex/launch.sh \\"\\$@\\"; else exec codex \\"\\$@\\"; fi" codex\'';

/** A generated SSH command starts a new login shell. An alias set before exec
 * disappears there, and /etc/profile can reset PATH. Delegate to
 * ~/.maiterm/codex/login.sh (installed by background SSH setup) when present,
 * falling back to `exec $SHELL -l`.
 *
 * Deliberately contains ZERO single quotes (`'`) and ZERO backslashes (`\`) so
 * `buildSshCommand` can splice it straight inside `'...'` without nested
 * `'\''` escapes — which break local `fish` (where `\'` inside `'...'` is an
 * escaped apostrophe and flips quote parity). */
export function remoteCodexLogin(): string {
  return 'exec /bin/sh -c "test -r ~/.maiterm/codex/login.sh && exec /bin/sh ~/.maiterm/codex/login.sh; exec $SHELL -l" maiterm-codex-login $SHELL -l';
}
