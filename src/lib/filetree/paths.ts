/**
 * The tree's only native-path code (docs/file-tree.md §5.1).
 *
 * Everything between Rust and the tree is root-relative POSIX (`src/lib/x.ts`, '' for the
 * root). A native path is needed in exactly two directions:
 * - rel → abs, to open a file or hand a path to a terminal: `relToAbs`, pure string work on
 *   the root Rust canonicalised (`OpenAck.root_native`) and its separator;
 * - abs → rel, to reveal an editor's file or a hook path: that one is Rust's (`ftLocate`),
 *   because it needs canonicalisation and case folding. Never strip a prefix here.
 */
import type { OpenAck } from '$lib/tauri/types';

type RootInfo = Pick<OpenAck, 'root_native' | 'sep'>;

/** The native absolute path of a rel under the subscription's root. */
export function relToAbs(ack: RootInfo, rel: string): string {
  if (!rel) return ack.root_native;
  const root = ack.root_native.endsWith(ack.sep) ? ack.root_native.slice(0, -ack.sep.length) : ack.root_native;
  const tail = ack.sep === '/' ? rel : rel.split('/').join(ack.sep);
  return `${root}${ack.sep}${tail}`;
}

/** The folder holding a rel ('' = the root itself), still root-relative. */
export function relDir(rel: string): string {
  const i = rel.lastIndexOf('/');
  return i < 0 ? '' : rel.slice(0, i);
}

/** The last segment of a native path or a rel, for headers and menu labels. */
export function lastSegment(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, '');
  const i = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return (i < 0 ? trimmed : trimmed.slice(i + 1)) || path;
}

/** A root for a human: `~/x/y` under home, else as is. `home` is the native home folder. */
export function tildePath(abs: string, home: string | null): string {
  if (!home) return abs;
  const h = home.replace(/[\\/]+$/, '');
  if (abs === h) return '~';
  if (abs.startsWith(h + '/') || abs.startsWith(h + '\\')) return '~' + abs.slice(h.length);
  return abs;
}
