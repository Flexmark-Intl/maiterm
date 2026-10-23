import { describe, expect, it } from 'vitest';
import { revealEarly } from './reveal';

describe('revealEarly', () => {
  it('answers a path that is not on disk before anything raises a chip', () => {
    // The human on a lens or find view, or using the tree, or the column closed: a typo'd
    // path used to answer `noted` / `tree_closed` with a chip for a file that never existed.
    for (const humanBusy of [false, true]) {
      const open = revealEarly({ open: true, onDisk: false, inRoot: true, humanBusy });
      expect(open).toMatchObject({ result: 'not_found', chip: false });
    }
    expect(revealEarly({ open: false, onDisk: false, inRoot: null, humanBusy: false })).toMatchObject({ result: 'not_found', chip: false });
  });

  it('chips only a path that is there or that nothing could check', () => {
    expect(revealEarly({ open: false, onDisk: true, inRoot: null, humanBusy: false })).toMatchObject({ result: 'tree_closed', chip: true });
    // A caller on an ssh host with the column closed can't be checked from here.
    expect(revealEarly({ open: false, onDisk: null, inRoot: null, humanBusy: false })).toMatchObject({ result: 'tree_closed', chip: true });
    expect(revealEarly({ open: true, onDisk: true, inRoot: true, humanBusy: true })).toMatchObject({ result: 'noted', chip: true });
  });

  it('leaves outside the root to the root, and the rest to the reveal', () => {
    expect(revealEarly({ open: true, onDisk: null, inRoot: false, humanBusy: true })).toMatchObject({ result: 'outside_root', chip: false });
    expect(revealEarly({ open: true, onDisk: true, inRoot: true, humanBusy: false })).toBeNull();
    // Unknown is not absent: the reveal's own outcome decides.
    expect(revealEarly({ open: true, onDisk: null, inRoot: true, humanBusy: false })).toBeNull();
  });
});
