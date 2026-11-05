import { describe, expect, it } from 'vitest';
import {
  abSwitch,
  afterRemoval,
  appendToDraft,
  attachFor,
  compareSet,
  extendTo,
  iterateText,
  listedSelection,
  selectionMenu,
  selectionTags,
  nextPurpose,
  nextStar,
  nextStatus,
  selectMode,
  selectWith,
  syncActions,
  type PlayerState,
} from './galleryCurate';

describe('selection', () => {
  const order = ['a', 'b', 'c', 'd', 'e'];

  it('reads a click as one, a toggle or a range', () => {
    expect(selectMode({ metaKey: false, ctrlKey: false, shiftKey: false })).toBe('only');
    expect(selectMode({ metaKey: true, ctrlKey: false, shiftKey: false })).toBe('toggle');
    expect(selectMode({ metaKey: false, ctrlKey: true, shiftKey: false })).toBe('toggle');
    expect(selectMode({ metaKey: true, ctrlKey: false, shiftKey: true })).toBe('range');
  });

  it('a plain click selects one; Cmd-click adds and removes, the last added being the cursor', () => {
    expect(selectWith(order, ['a', 'b'], 'c', 'only')).toEqual(['c']);
    expect(selectWith(order, ['a'], 'c', 'toggle')).toEqual(['a', 'c']);
    expect(selectWith(order, ['a', 'c'], 'a', 'toggle')).toEqual(['c']);
  });

  it('Shift-click runs the grid order from the cursor to the click, either way, and adds to it', () => {
    expect(selectWith(order, ['b'], 'd', 'range')).toEqual(['b', 'c', 'd']);
    expect(selectWith(order, ['d'], 'b', 'range')).toEqual(['d', 'c', 'b']);
    // Kept, with the range ending on the click.
    expect(selectWith(order, ['e', 'a'], 'c', 'range')).toEqual(['e', 'a', 'b', 'c']);
    // No cursor: just the click; a cursor no longer listed starts nothing.
    expect(selectWith(order, [], 'c', 'range')).toEqual(['c']);
    expect(selectWith(order, ['gone'], 'c', 'range')).toEqual(['gone', 'c']);
  });

  it('F stars unless all are starred; 1/2/3 set a status unless all have it', () => {
    expect(nextStar([{ starred: true }, { starred: false }])).toBe(true);
    expect(nextStar([{ starred: true }, { starred: true }])).toBe(false);
    expect(nextStatus([{ status: 'final' }, { status: '' }], 'final')).toBe('final');
    expect(nextStatus([{ status: 'final' }, { status: 'final' }], 'final')).toBe('');
    expect(nextStatus([], 'picked')).toBe('picked');
  });
});

describe('iterateText', () => {
  it('names the asset and its recipe, then quotes the prompt line by line', () => {
    const t = iterateText({ title: 'Hero\n v2', kind: 'image', recipe: { tool: 'fal', model: 'flux-pro', seed: 42, prompt: 'a warm sunrise\nover hills' } });
    expect(t).toBe('Iterate on "Hero v2" (image, flux-pro via fal, seed 42):\n> a warm sunrise\n> over hills');
  });

  it('leaves out what the recipe lacks', () => {
    expect(iterateText({ title: 'Ref', kind: 'image' })).toBe('Iterate on "Ref" (image):');
    expect(iterateText({ title: 'Cut', kind: 'video', recipe: { tool: 'ffmpeg' } })).toBe('Iterate on "Cut" (video, ffmpeg):');
    expect(iterateText({ title: 'S', kind: 'audio', recipe: { tool: 't', seed: 0 } })).toBe('Iterate on "S" (audio, t, seed 0):');
  });

  it('goes into an empty draft, or on a new line after one with text', () => {
    expect(appendToDraft('', 'Iterate')).toBe('Iterate');
    expect(appendToDraft('   ', 'Iterate')).toBe('Iterate');
    expect(appendToDraft('three more  \n', 'Iterate')).toBe('three more\nIterate');
    expect(appendToDraft('keep', '')).toBe('keep');
  });
});

describe('compare', () => {
  const players = (...t: [boolean, number][]): PlayerState[] => t.map(([paused, currentTime]) => ({ paused, currentTime }));

  it('takes two to four of the selection, the newest four', () => {
    expect(compareSet(['a'])).toBeNull();
    expect(compareSet(['a', 'b'])).toEqual(['a', 'b']);
    expect(compareSet(['a', 'b', 'c', 'd', 'e'])).toEqual(['b', 'c', 'd', 'e']);
  });

  it('mirrors the leader: play, pause and seek reach the others', () => {
    const p = players([false, 3], [true, 3], [true, 1]);
    expect(syncActions(0, 0, { kind: 'play', time: 3 }, p)).toEqual([{ index: 1, play: true }, { index: 2, play: true, seek: 3 }]);
    expect(syncActions(0, 0, { kind: 'pause', time: 5 }, players([true, 5], [false, 5.05], [false, 4]))).toEqual([
      { index: 1, play: false },
      { index: 2, play: false, seek: 5 },
    ]);
    expect(syncActions(1, 1, { kind: 'seek', time: 10 }, players([true, 2], [true, 10]))).toEqual([{ index: 0, seek: 10 }]);
  });

  it('corrects drift past 0.1 s only, and ignores a follower echoing its correction', () => {
    expect(syncActions(0, 0, { kind: 'tick', time: 4 }, players([false, 4], [false, 4.08]))).toEqual([]);
    expect(syncActions(0, 0, { kind: 'tick', time: 4 }, players([false, 4], [false, 4.3]))).toEqual([{ index: 1, seek: 4 }]);
    // Only the leader drives.
    expect(syncActions(0, 1, { kind: 'seek', time: 9 }, players([false, 4], [false, 9]))).toEqual([]);
  });

  it('switches audio A/B at the shared position', () => {
    expect(abSwitch(0, 2, 12.5)).toEqual({ stop: 0, play: 1, at: 12.5 });
    expect(abSwitch(2, 3, 1)).toEqual({ stop: 2, play: 0, at: 1 });
    expect(abSwitch(-1, 3, 0)).toEqual({ stop: 0, play: 1, at: 0 });
    expect(abSwitch(0, 1, 0)).toBeNull();
  });
});

describe('a drop onto a composer', () => {
  it('reaches only the chat it landed on, with absolute paths only', () => {
    const d = { chatTabId: 'tab-1', paths: ['/g/files/a-1.png', 'relative.png', 'C:\\g\\a-2.png'] };
    expect(attachFor(d, 'tab-1')).toEqual(['/g/files/a-1.png', 'C:\\g\\a-2.png']);
    expect(attachFor(d, 'tab-2')).toBeNull();
    expect(attachFor(d, null)).toBeNull();
    expect(attachFor({ chatTabId: 'tab-1', paths: ['x'] }, 'tab-1')).toBeNull();
    expect(attachFor(undefined, 'tab-1')).toBeNull();
  });
});

describe('the selection menu', () => {
  const row = (o: Partial<{ purpose: string; starred: boolean; status: string; tags: string[]; deleted_at: string | null }> = {}) => ({ starred: false, status: '', tags: [], deleted_at: null, ...o });
  const actions = (m: ReturnType<typeof selectionMenu>) => m.filter((e) => !e.separator).map((e) => e.action);

  it('is empty with nothing selected', () => {
    expect(selectionMenu([], 'Reveal')).toEqual([]);
  });

  it('offers one asset its own actions and no Compare', () => {
    const m = selectionMenu([row()], 'Reveal in Finder');
    expect(actions(m)).toEqual(['open', 'star', 'picked', 'final', 'rejected', 'purpose', 'iterate', 'send', 'collect', 'tag', 'export', 'reveal', 'copyPath', 'trash']);
    expect(m.find((e) => e.action === 'reveal')?.label).toBe('Reveal in Finder');
  });

  it('moves a selection to Proof, and back to For you once all of it is proof', () => {
    const label = (rows: { purpose?: string }[]) => selectionMenu(rows.map((r) => row(r)), 'R').find((e) => e.action === 'purpose')?.label;
    expect(label([{ purpose: 'deliverable' }])).toBe('Move to Proof');
    expect(label([{ purpose: 'proof' }, { purpose: '' }])).toBe('Move to Proof');
    expect(label([{ purpose: 'proof' }, { purpose: 'proof' }])).toBe('Move to For you');
    expect(nextPurpose([{ purpose: 'proof' }])).toBe('deliverable');
    expect(nextPurpose([])).toBe('proof');
  });

  it('offers several the bulk actions and Compare up to four', () => {
    const two = selectionMenu([row(), row({ tags: ['warm'] })], 'R');
    expect(actions(two)).not.toContain('open');
    expect(actions(two)).not.toContain('iterate');
    expect(two.find((e) => e.action === 'reveal')?.label).toBe('R (the first)');
    expect(actions(two)).toContain('untag');
    expect(two[0]).toMatchObject({ action: 'compare', disabled: false });
    expect(two.find((e) => e.action === 'copyPath')?.label).toBe('Copy 2 paths');
    const five = selectionMenu([row(), row(), row(), row(), row()], 'R');
    expect(five[0]).toMatchObject({ action: 'compare', disabled: true });
  });

  it('says what a toggle will do', () => {
    const m = selectionMenu([row({ starred: true, status: 'picked' }), row({ starred: true, status: 'picked' })], 'R');
    expect(m.find((e) => e.action === 'star')?.label).toBe('Unstar');
    expect(m.find((e) => e.action === 'picked')?.label).toBe('Unpick');
    expect(m.find((e) => e.action === 'final')?.label).toBe('Mark final');
    expect(actions(m)).toContain('clearStatus');
  });

  it('restores what is all in the trash and disables what cannot apply to it', () => {
    const m = selectionMenu([row({ deleted_at: 'x' })], 'R');
    expect(actions(m)).toContain('restore');
    expect(actions(m)).not.toContain('trash');
    for (const a of ['open', 'iterate', 'send', 'export', 'reveal', 'copyPath'] as const) expect(m.find((e) => e.action === a)?.disabled).toBe(true);
    // A mix restores the trashed ones and moves the rest to the trash.
    const mix = selectionMenu([row({ deleted_at: 'x' }), row(), row()], 'R');
    expect(mix.find((e) => e.action === 'restore')?.label).toBe('Restore the 1 in the trash');
    expect(mix.find((e) => e.action === 'trash')?.label).toBe('Move the other 2 to trash');
  });

  it('acts only on what the grid lists', () => {
    expect(listedSelection(['v', 'a', 'gone', 'b'], ['a', 'b', 'c'])).toEqual(['a', 'b']);
    expect(listedSelection([], ['a'])).toEqual([]);
  });

  it('steps to the next asset when the viewed one leaves the list', () => {
    // Was third of five, now four: the asset now third is the next one.
    expect(afterRemoval(2, 4)).toBe(2);
    // Was last: the new last, the previous one.
    expect(afterRemoval(4, 4)).toBe(3);
    expect(afterRemoval(0, 0)).toBeNull();
    expect(afterRemoval(-1, 3)).toBeNull();
  });

  it('lists the selection’s tags, most common first', () => {
    expect(selectionTags([{ tags: ['b', 'a'] }, { tags: ['a'] }, { tags: [] }])).toEqual(['a', 'b']);
  });

  it('extends onto an asset as the new cursor', () => {
    expect(extendTo(['a', 'b'], 'c')).toEqual(['a', 'b', 'c']);
    expect(extendTo(['a', 'b'], 'a')).toEqual(['b', 'a']);
  });
});
