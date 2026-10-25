import { describe, expect, it } from 'vitest';
import {
  abSwitch,
  appendToDraft,
  attachFor,
  compareSet,
  iterateText,
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
