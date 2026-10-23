/**
 * `revealInTree`'s answers that come before the tree is touched (docs/file-tree.md §9.6).
 *
 * Whether the path is there is asked FIRST: every answer that raises the pointer chip
 * ("‹tab› points at x · Show", or the titlebar dot with the column closed) offers the human a
 * path, and a path that isn't on disk must never be offered. The filtered-view check (find or
 * a lens leaving the path out) ran before it, so a typo answered `noted` with a chip for a
 * file that never existed.
 */

export interface RevealFacts {
  /** The column is open, with a subscription. */
  open: boolean;
  /** On disk on the root's machine (or this computer with the column closed); null when
   *  nothing could say (refused, a host not answering, a closed column and a caller on a host). */
  onDisk: boolean | null;
  /** Under the root the tree shows; null when not asked (the column is closed). */
  inRoot: boolean | null;
  /** The human used the tree in the last few seconds, or is naming an item. */
  humanBusy: boolean;
}

export interface RevealEarly {
  result: 'outside_root' | 'not_found' | 'tree_closed' | 'noted';
  /** Raise the pointer chip. */
  chip: boolean;
  note?: string;
}

/** The answer when it is settled before the tree reveals anything; null to go on and reveal. */
export function revealEarly(f: RevealFacts): RevealEarly | null {
  // Outside the root the tree has nothing to show, whatever is on disk there.
  if (f.open && f.inRoot === false) return { result: 'outside_root', chip: false };
  if (f.onDisk === false) {
    return { result: 'not_found', chip: false, note: f.open ? 'Nothing by that name is on disk in the folder Files shows.' : 'Nothing by that name is on disk.' };
  }
  if (!f.open) return { result: 'tree_closed', chip: true };
  if (f.humanBusy) return { result: 'noted', chip: true };
  return null;
}
