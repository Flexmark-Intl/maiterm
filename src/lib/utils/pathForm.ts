/**
 * Local path forms that depend on the platform, kept free of stores so Vitest can run them.
 *
 * A backslash is a separator only on Windows. On macOS and Linux it is a legal character in a
 * file name (`report\v2.txt`), so splitting on it there names a file that doesn't exist.
 */

/** The last component of a local path. */
export function basenameOf(path: string, windows: boolean): string {
  const parts = path.split(windows ? /[\\/]/ : '/');
  return parts.pop() || path;
}

/**
 * The form two spellings of one path compare equal in, without touching the disk: `.` and
 * empty segments dropped, `..` folded into its parent. On Windows separators become `/` and
 * case is ignored, because the file system ignores it. Symlinks are not resolved here — the
 * opener canonicalizes local paths through Rust before they reach a tab (`canonicalEditorPath`).
 */
export function pathKey(path: string, windows: boolean): string {
  let p = windows ? path.replace(/\\/g, '/') : path;
  const absolute = p.startsWith('/');
  const out: string[] = [];
  for (const seg of p.split('/')) {
    if (seg === '' || seg === '.') continue;
    if (seg === '..' && out.length > 0 && out[out.length - 1] !== '..') {
      out.pop();
      continue;
    }
    // `..` above the root is the root itself.
    if (seg === '..' && absolute) continue;
    out.push(seg);
  }
  p = (absolute ? '/' : '') + out.join('/');
  return windows ? p.toLowerCase() : p;
}
