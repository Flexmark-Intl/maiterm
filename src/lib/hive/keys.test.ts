import { describe, expect, it } from 'vitest';
import { escapeAction, hiveKeyAction, isHiveAppShortcut } from './keys';

const ev = (key: string, mods: { meta?: boolean; ctrl?: boolean; shift?: boolean; alt?: boolean; code?: string } = {}) => ({
  key,
  code: mods.code ?? '',
  metaKey: !!mods.meta,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
});

describe('hiveKeyAction', () => {
  it('maps the §4.6 keys on a Mac (Cmd)', () => {
    expect(hiveKeyAction(ev('1', { meta: true }), true)).toEqual({ kind: 'focus-coordinator' });
    expect(hiveKeyAction(ev('2', { meta: true }), true)).toEqual({ kind: 'open-badge', badge: 1 });
    expect(hiveKeyAction(ev('9', { meta: true }), true)).toEqual({ kind: 'open-badge', badge: 8 });
    expect(hiveKeyAction(ev('w', { meta: true }), true)).toEqual({ kind: 'close-focused' });
    expect(hiveKeyAction(ev('W', { meta: true }), true)).toEqual({ kind: 'close-focused' });
    expect(hiveKeyAction(ev('Y', { meta: true, shift: true }), true)).toEqual({ kind: 'cycle-view' });
  });

  it('cycles on Shift+[ / ] whichever character the layout reports', () => {
    expect(hiveKeyAction(ev('[', { meta: true, shift: true }), true)).toEqual({ kind: 'cycle', dir: -1 });
    expect(hiveKeyAction(ev('{', { meta: true, shift: true }), true)).toEqual({ kind: 'cycle', dir: -1 });
    expect(hiveKeyAction(ev('}', { meta: true, shift: true }), true)).toEqual({ kind: 'cycle', dir: 1 });
    expect(hiveKeyAction(ev('ü', { meta: true, shift: true, code: 'BracketRight' }), true)).toEqual({ kind: 'cycle', dir: 1 });
  });

  it('uses Ctrl off a Mac, and Cmd is nothing there', () => {
    expect(hiveKeyAction(ev('1', { ctrl: true }), false)).toEqual({ kind: 'focus-coordinator' });
    expect(hiveKeyAction(ev('[', { ctrl: true, shift: true }), false)).toEqual({ kind: 'cycle', dir: -1 });
    expect(hiveKeyAction(ev('1', { meta: true }), false)).toBeNull();
    // Ctrl on a Mac is the terminal's (Ctrl+W erases a word).
    expect(hiveKeyAction(ev('w', { ctrl: true }), true)).toBeNull();
  });

  it('returns null for a Shift+digit, a bare key, Option, and unbound keys', () => {
    expect(hiveKeyAction(ev('!', { meta: true, shift: true, code: 'Digit1' }), true)).toBeNull();
    expect(hiveKeyAction(ev('2', { meta: true, shift: true, code: 'Digit2' }), true)).toBeNull();
    expect(hiveKeyAction(ev('1'), true)).toBeNull();
    expect(hiveKeyAction(ev('1', { meta: true, alt: true }), true)).toBeNull();
    expect(hiveKeyAction(ev('w', { meta: true, shift: true }), true)).toBeNull();
    expect(hiveKeyAction(ev('0', { meta: true }), true)).toBeNull();
    expect(hiveKeyAction(ev('[', { meta: true }), true)).toBeNull();
    expect(hiveKeyAction(ev('y', { meta: true }), true)).toBeNull();
    expect(hiveKeyAction(ev('g', { meta: true, shift: true }), true)).toBeNull();
  });

  it('never makes Escape an action, with any modifier', () => {
    for (const mods of [{}, { meta: true }, { ctrl: true }, { meta: true, shift: true }]) {
      expect(hiveKeyAction(ev('Escape', mods), true)).toBeNull();
      expect(hiveKeyAction(ev('Escape', mods), false)).toBeNull();
    }
  });

  it('isHiveAppShortcut is exactly the keys with an action', () => {
    expect(isHiveAppShortcut(ev('y', { meta: true, shift: true }), true)).toBe(true);
    expect(isHiveAppShortcut(ev('5', { meta: true }), true)).toBe(true);
    expect(isHiveAppShortcut(ev('y', { meta: true }), true)).toBe(false);
    expect(isHiveAppShortcut(ev('Escape'), true)).toBe(false);
  });
});

describe('escapeAction', () => {
  it('leaves Escape to a raw terminal, wherever it sits', () => {
    expect(escapeAction({ terminal: true, editable: true, workerColumn: 't1' })).toBeNull();
    expect(escapeAction({ terminal: true, editable: false, workerColumn: null })).toBeNull();
  });

  it('blurs a composer before it closes anything', () => {
    expect(escapeAction({ terminal: false, editable: true, workerColumn: 't1' })).toEqual({ kind: 'blur' });
    expect(escapeAction({ terminal: false, editable: true, workerColumn: null })).toEqual({ kind: 'blur' });
  });

  it("closes a worker's column, never the coordinator's or the board", () => {
    expect(escapeAction({ terminal: false, editable: false, workerColumn: 't1' })).toEqual({ kind: 'close', taskId: 't1' });
    expect(escapeAction({ terminal: false, editable: false, workerColumn: null })).toBeNull();
  });
});
