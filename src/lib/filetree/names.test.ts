import { describe, expect, it } from 'vitest';
import { foldName, nameProblem, renameSelection, siblingLookup, type NameRules } from './names';

// Accented names are spelled with char codes: an editor or a tool would normalize a literal.
const E_ACUTE = String.fromCharCode(0xe9);
const COMBINING_ACUTE = String.fromCharCode(0x301);
const NFC_CAFE = `caf${E_ACUTE}.ts`;
const NFD_CAFE = `cafe${COMBINING_ACUTE}.ts`;

const here = siblingLookup([['README.md', 'file'], ['src', 'dir'], [NFC_CAFE, 'file']], true, true);
const rules = (o: Partial<NameRules> = {}): NameRules => ({ mode: 'create', strictWindows: false, caseInsensitive: true, nfc: true, sibling: here, ...o });

describe('nameProblem', () => {
  it('refuses an empty name', () => {
    expect(nameProblem('', rules())).toMatch(/needed/);
    expect(nameProblem('   ', rules())).toMatch(/needed/);
  });

  it('refuses a slash in a rename but allows nested create', () => {
    expect(nameProblem('a/b', rules({ mode: 'rename', current: 'x' }))).toMatch(/“\/”/);
    expect(nameProblem('a/b/c.ts', rules())).toBeNull();
  });

  it('refuses empty, dot and dot-dot components in a create', () => {
    expect(nameProblem('a//b', rules())).toMatch(/empty/);
    expect(nameProblem('/a', rules())).toMatch(/start/);
    expect(nameProblem('a/', rules())).toMatch(/end/);
    expect(nameProblem('a/../b', rules())).toMatch(/isn’t a name/);
    expect(nameProblem('.', rules())).toMatch(/isn’t a name/);
  });

  it('applies the Windows rules only when strict', () => {
    for (const bad of ['a<b', 'a>b', 'a:b', 'a"b', 'a\\b', 'a|b', 'a?b', 'a*b']) {
      expect(nameProblem(bad, rules({ strictWindows: true }))).toMatch(/isn’t allowed/);
      expect(nameProblem(bad, rules({ strictWindows: false }))).toBeNull();
    }
    expect(nameProblem('trail.', rules({ strictWindows: true }))).toMatch(/dot or a space/);
    expect(nameProblem('trail ', rules({ strictWindows: true }))).toMatch(/dot or a space/);
    expect(nameProblem('trail.', rules())).toBeNull();
  });

  it('words the Windows rules for where it runs', () => {
    // On a Mac's case-folding disk the name works here: the message says what it would break.
    const mac = rules({ strictWindows: true, windows: false });
    expect(nameProblem('a:b', mac)).toBe('“:” isn’t allowed: the name would break on Windows');
    expect(nameProblem('trail.', mac)).toBe('A name ending with a dot or a space would break on Windows');
    expect(nameProblem('aux.ts', mac)).toBe('“AUX” is reserved on Windows, so the name would break there');
    // On Windows the disk refuses it outright, so "on Windows" adds nothing.
    const win = rules({ strictWindows: true, windows: true });
    expect(nameProblem('a:b', win)).toBe('“:” isn’t allowed in a name');
    expect(nameProblem('trail ', win)).toBe('A name can’t end with a dot or a space');
    expect(nameProblem('aux.ts', win)).toBe('“AUX” is a reserved name');
    for (const r of [mac, win, rules()]) expect(nameProblem('a\u0007b', r)).toMatch(/Control characters/);
  });

  it('refuses Windows reserved names, with or without an extension, in any case', () => {
    for (const n of ['CON', 'prn', 'Aux', 'nul.txt', 'COM1', 'com9.log', 'LPT3', 'lpt1.c']) {
      expect(nameProblem(n, rules({ strictWindows: true }))).toMatch(/reserved/);
    }
    for (const n of ['console', 'com10', 'lpt0', 'auxiliary.ts', 'nullable']) {
      expect(nameProblem(n, rules({ strictWindows: true }))).toBeNull();
    }
    // A reserved name deep in a nested create is caught too.
    expect(nameProblem('a/con/b.ts', rules({ strictWindows: true }))).toMatch(/reserved/);
  });

  it('catches a collision case-folded where the disk folds, exactly where it does not', () => {
    expect(nameProblem('readme.md', rules())).toMatch(/already here/);
    const exact = siblingLookup([['README.md', 'file']], false);
    expect(nameProblem('readme.md', rules({ caseInsensitive: false, sibling: exact }))).toBeNull();
    expect(nameProblem('README.md', rules({ caseInsensitive: false, sibling: exact }))).toMatch(/already here/);
  });

  it('folds NFC and NFD as one name only when asked', () => {
    expect(NFD_CAFE).not.toBe(NFC_CAFE);
    expect(nameProblem(NFD_CAFE, rules())).toMatch(/already here/);
    const plain = siblingLookup([[NFC_CAFE, 'file']], true, false);
    expect(nameProblem(NFD_CAFE, rules({ nfc: false, sibling: plain }))).toBeNull();
  });

  it('uses an existing folder on the way of a nested create, refuses a file in the way', () => {
    expect(nameProblem('SRC/new.ts', rules())).toBeNull();
    expect(nameProblem('readme.md/x.ts', rules())).toMatch(/is a file/);
    expect(nameProblem('src', rules())).toMatch(/already here/);
  });

  it('lets a rename keep or re-case its own name', () => {
    const own = siblingLookup([['README.md', 'file'], ['Foo.ts', 'file']], true, false, 'Foo.ts');
    const r = rules({ mode: 'rename', current: 'Foo.ts', sibling: own });
    expect(nameProblem('Foo.ts', r)).toBeNull();
    expect(nameProblem('foo.ts', r)).toBeNull();
    expect(nameProblem('readme.MD', r)).toMatch(/already here/);
  });
});

describe('renameSelection', () => {
  it('selects the stem before the last extension', () => {
    expect(renameSelection('a.test.ts', false)).toEqual([0, 6]);
    expect(renameSelection('Makefile', false)).toEqual([0, 8]);
    expect(renameSelection('.env', false)).toEqual([0, 4]);
    expect(renameSelection('my.folder', true)).toEqual([0, 9]);
  });
});

describe('foldName', () => {
  it('folds only what it is asked to', () => {
    expect(foldName('ABC', false)).toBe('ABC');
    expect(foldName('ABC', true)).toBe('abc');
    expect(foldName(`e${COMBINING_ACUTE}`, false, true)).toBe(E_ACUTE);
  });
});
