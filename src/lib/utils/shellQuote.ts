/**
 * Quote a path so a shell reads it back as exactly one word (docs/file-tree.md §9.5).
 *
 * Used where maiTerm TYPES a path into a terminal's command line (a file dropped on a tab,
 * a pasted Finder copy, "Add to tab" from Files). The human presses Enter, so the path has
 * to survive their shell's parser unchanged.
 *
 * - `posix` (sh/bash/zsh/fish): backslash before every character that isn't plainly safe.
 *   A newline can't be backslash-escaped (that's a line continuation), so a path holding
 *   one is single-quoted instead.
 * - `pwsh` (PowerShell 5/7): `'…'`, the only literal string form, with `'` doubled.
 *   PowerShell also reads the typographic quotes ‘ ’ ‚ ‛ as single quotes, so they are
 *   doubled too.
 * - `cmd`: `"…"`. Windows file names can't contain `"`, so nothing inside needs escaping;
 *   cmd still expands `%VAR%` inside quotes and there is no way to stop that interactively.
 */
export type ShellKind = 'posix' | 'pwsh' | 'cmd';

const POSIX_UNSAFE = /([^a-zA-Z0-9_\-.,/:@+])/g;

export function shellQuote(path: string, kind: ShellKind = 'posix'): string {
  switch (kind) {
    case 'pwsh':
      return `'${path.replace(/['\u2018\u2019\u201A\u201B]/g, (q) => q + q)}'`;
    case 'cmd':
      return `"${path}"`;
    case 'posix':
    default:
      if (path === '') return "''";
      if (/[\n\r]/.test(path)) return `'${path.replace(/'/g, `'\\''`)}'`;
      return path.replace(POSIX_UNSAFE, '\\$1');
  }
}

/**
 * Which quoting a LOCAL terminal's shell needs, from the platform and the
 * `windows_shell` preference (`pty/manager.rs::resolve_windows_shell`).
 *
 * `auto` on Windows starts Git Bash when it is installed and PowerShell otherwise, and the
 * frontend can't tell which one this tab got. It gets PowerShell quoting: `'C:\a b'` reads
 * the same in bash, whereas bash's backslashes would mangle a PowerShell path. The two part
 * only on a path containing `'`.
 */
export function localShellKind(windows: boolean, windowsShell: string | null | undefined): ShellKind {
  if (!windows) return 'posix';
  switch (windowsShell) {
    case 'cmd':
      return 'cmd';
    case 'gitbash':
    case 'wsl':
      return 'posix';
    default:
      return 'pwsh';
  }
}
