/**
 * Name rules for the tree's inline New and Rename (docs/file-tree.md §10.4).
 *
 * Pure, so the table of what is refused is tested on its own (names.test.ts). Rust refuses
 * the same things where they would do harm (a `..`, a separator in a rename, a Windows device
 * name on Windows); this is the wider check, said in words under the input BEFORE anything is
 * sent, and the only place the case-folding-disk rules live:
 * - Windows-illegal names are refused on Windows and on any root whose disk folds case
 *   (APFS, NTFS, exFAT): a repo made there is one checkout away from a Windows machine, and
 *   `aux.ts` is a file nobody there can open.
 * - A name that exists already is refused, compared case-folded where the disk folds, so
 *   `Readme.md` beside `README.md` is caught here rather than as a vague failure.
 */

/** Characters Windows refuses in a name (besides control characters). */
const WINDOWS_ILLEGAL = /[<>:"\\|?*]/;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f]/;
/** Device names Windows reserves, with or without an extension (`con.txt` too). */
const RESERVED = /^(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(\..*)?$/i;

export interface NameRules {
  /** 'create' allows `a/b/c.ts` (folders made on the way); 'rename' takes one name. */
  mode: 'create' | 'rename';
  /** Apply the Windows rules: on Windows, or on a disk that folds case. */
  strictWindows: boolean;
  /** The disk folds case: `Foo` and `foo` are one name. */
  caseInsensitive: boolean;
  /** NFC and NFD spell one name (macOS). */
  nfc?: boolean;
  /** What is in the folder now: a name → 'dir' | 'file', or null when nothing is. Asked with
   *  the name as typed; this function does the folding. */
  sibling: (folded: string) => 'dir' | 'file' | null;
  /** Rename: the item's current name, which may be re-cased (or kept). */
  current?: string;
}

/** How a name is compared on this disk. */
export function foldName(name: string, caseInsensitive: boolean, nfc = false): string {
  let n = nfc ? name.normalize('NFC') : name;
  if (caseInsensitive) n = n.toLowerCase();
  return n;
}

/** One component's problem, or null. */
function componentProblem(part: string, strictWindows: boolean): string | null {
  if (part === '') return 'A name can’t be empty between slashes';
  if (part === '.' || part === '..') return `“${part}” isn’t a name`;
  if (CONTROL.test(part)) return 'A name can’t contain control characters';
  if (strictWindows) {
    const bad = part.match(WINDOWS_ILLEGAL);
    if (bad) return `“${bad[0]}” isn’t allowed in a name on Windows`;
    if (/[. ]$/.test(part)) return 'A name can’t end with a dot or a space on Windows';
    if (RESERVED.test(part)) return `“${part.split('.')[0].toUpperCase()}” is a reserved name on Windows`;
  }
  return null;
}

/**
 * What is wrong with `name`, in words, or null when it can be used. `null` for a rename to
 * the current name too: committing that is a no-op, not an error.
 */
export function nameProblem(raw: string, r: NameRules): string | null {
  const name = raw;
  if (name.trim() === '') return 'A name is needed';
  if (r.mode === 'rename') {
    if (name.includes('/')) return 'A name can’t contain “/”';
    const p = componentProblem(name, r.strictWindows);
    if (p) return p;
    if (r.current !== undefined) {
      if (name === r.current) return null;
      // A re-cased name on a folding disk "exists" because it is this item.
      if (foldName(name, r.caseInsensitive, r.nfc) === foldName(r.current, r.caseInsensitive, r.nfc)) return null;
    }
    return r.sibling(name) ? `“${name}” is already here` : null;
  }
  if (name.startsWith('/')) return 'A name can’t start with “/”';
  if (name.endsWith('/')) return 'A name can’t end with “/”';
  const parts = name.split('/');
  for (const part of parts) {
    const p = componentProblem(part, r.strictWindows);
    if (p) return p;
  }
  // Only the first component is in this folder; deeper ones are checked by Rust, which uses
  // a folder already on the way and refuses a file in the way.
  const first = r.sibling(parts[0]);
  if (parts.length === 1) return first ? `“${parts[0]}” is already here` : null;
  return first === 'file' ? `“${parts[0]}” is a file, not a folder` : null;
}

/** The part of a name a rename preselects: everything before the last extension, so typing
 *  replaces the stem (`a.test.ts` → `a.test`). A dotfile and a folder select whole. */
export function renameSelection(name: string, folder: boolean): [number, number] {
  if (folder) return [0, name.length];
  const dot = name.lastIndexOf('.');
  return dot > 0 ? [0, dot] : [0, name.length];
}

/** A sibling lookup over a folder's entries, folded the way the disk folds. */
export function siblingLookup(
  names: Iterable<[string, 'dir' | 'file']>,
  caseInsensitive: boolean,
  nfc = false,
  exclude?: string,
): (name: string) => 'dir' | 'file' | null {
  const map = new Map<string, 'dir' | 'file'>();
  for (const [n, k] of names) if (n !== exclude) map.set(foldName(n, caseInsensitive, nfc), k);
  return (name) => map.get(foldName(name, caseInsensitive, nfc)) ?? null;
}
