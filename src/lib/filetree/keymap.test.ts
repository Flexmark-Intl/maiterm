import { describe, expect, it } from 'vitest';
import { treeAction, type KeyLike } from './keymap';
import { relToAbs } from './paths';

const k = (key: string, mods: Partial<KeyLike> = {}): KeyLike => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...mods });
const mac = { mac: true, vim: false };
const linux = { mac: false, vim: false };

describe('treeAction', () => {
  it('bare letters are type-ahead, never commands, outside vim mode', () => {
    expect(treeAction(k('a'), mac)).toEqual({ type: 'typeahead', char: 'a' });
    expect(treeAction(k('j'), mac)).toEqual({ type: 'typeahead', char: 'j' });
  });

  it('vim mode maps j/k/gg/G and needs a leading quote for type-ahead', () => {
    const vim = { mac: true, vim: true };
    expect(treeAction(k('j'), vim)).toEqual({ type: 'move', to: 'down' });
    expect(treeAction(k('g'), vim)).toEqual({ type: 'pendingG' });
    expect(treeAction(k('g'), { ...vim, pendingG: true })).toEqual({ type: 'move', to: 'home' });
    expect(treeAction(k("'"), vim)).toEqual({ type: 'typeahead', char: '' });
    expect(treeAction(k('j'), { ...vim, typeaheadArmed: true })).toEqual({ type: 'typeahead', char: 'j' });
  });

  it('AltGr characters are type-ahead off macOS, though Chromium reports Ctrl+Alt', () => {
    const altGr = (key: string) => k(key, { ctrlKey: true, altKey: true, getModifierState: (m) => m === 'AltGraph' });
    expect(treeAction(altGr('@'), linux)).toEqual({ type: 'typeahead', char: '@' });
    expect(treeAction(altGr('ł'), linux)).toEqual({ type: 'typeahead', char: 'ł' });
    // A real Ctrl+Alt chord (no AltGraph) is still nobody's.
    expect(treeAction(k('q', { ctrlKey: true, altKey: true, getModifierState: () => false }), linux)).toBeNull();
    // Vim mode keeps letters as commands; an AltGr character is not one.
    expect(treeAction(altGr('@'), { ...linux, vim: true })).toBeNull();
  });

  it('leaves app chords alone (Cmd+N, Cmd+D) and the other platform modifier', () => {
    expect(treeAction(k('n', { metaKey: true }), mac)).toBeNull();
    expect(treeAction(k('d', { metaKey: true }), mac)).toBeNull();
    expect(treeAction(k('a', { metaKey: true }), linux)).toBeNull();
  });

  it('copy path chords differ per platform', () => {
    expect(treeAction(k('ç', { code: 'KeyC', metaKey: true, altKey: true }), mac)).toEqual({ type: 'copyPath', relative: false });
    expect(treeAction(k('Ç', { code: 'KeyC', metaKey: true, altKey: true, shiftKey: true }), mac)).toEqual({ type: 'copyPath', relative: true });
    expect(treeAction(k('C', { code: 'KeyC', altKey: true, shiftKey: true }), linux)).toEqual({ type: 'copyPath', relative: false });
    expect(treeAction(k('C', { code: 'KeyC', ctrlKey: true, altKey: true, shiftKey: true }), linux)).toEqual({ type: 'copyPath', relative: true });
  });

  it('Enter opens, Mod+Enter opens to the side, Alt+Enter adds to the tab, Space previews', () => {
    expect(treeAction(k('Enter'), mac)).toEqual({ type: 'open' });
    expect(treeAction(k('Enter', { metaKey: true }), mac)).toEqual({ type: 'openSide' });
    expect(treeAction(k('Enter', { ctrlKey: true }), linux)).toEqual({ type: 'openSide' });
    expect(treeAction(k('Enter', { altKey: true }), mac)).toEqual({ type: 'addToTab' });
    expect(treeAction(k(' '), mac)).toEqual({ type: 'preview' });
  });

  it('arrows move, shift extends, alt recurses', () => {
    expect(treeAction(k('ArrowDown', { shiftKey: true }), mac)).toEqual({ type: 'extend', to: 'down' });
    expect(treeAction(k('ArrowRight', { altKey: true }), mac)).toEqual({ type: 'expandRecursive' });
    expect(treeAction(k('Escape'), mac)).toEqual({ type: 'escape' });
  });
});

describe('relToAbs', () => {
  it('joins with the root native separator', () => {
    expect(relToAbs({ root_native: '/Users/d/proj', sep: '/' }, 'src/a.ts')).toBe('/Users/d/proj/src/a.ts');
    expect(relToAbs({ root_native: '/Users/d/proj', sep: '/' }, '')).toBe('/Users/d/proj');
    expect(relToAbs({ root_native: 'C:\\proj', sep: '\\' }, 'src/a.ts')).toBe('C:\\proj\\src\\a.ts');
    expect(relToAbs({ root_native: 'C:\\', sep: '\\' }, 'a.ts')).toBe('C:\\a.ts');
    expect(relToAbs({ root_native: '/', sep: '/' }, 'etc')).toBe('/etc');
  });
});
