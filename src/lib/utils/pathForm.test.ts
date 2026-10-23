import { describe, expect, it } from 'vitest';
import { basenameOf, pathKey } from './pathForm';

describe('basenameOf', () => {
  it('keeps a backslash in a macOS/Linux file name', () => {
    // The uploaded file is `report\v2.txt`; typing `v2.txt` names a file that isn't there.
    expect(basenameOf('/Users/d/report\\v2.txt', false)).toBe('report\\v2.txt');
  });

  it('splits on either separator on Windows', () => {
    expect(basenameOf('C:\\Users\\d\\report.txt', true)).toBe('report.txt');
    expect(basenameOf('C:\\repo/src/a.ts', true)).toBe('a.ts');
  });
});

describe('pathKey', () => {
  it('folds . and .. segments', () => {
    expect(pathKey('/Users/d/repo/./src/a.ts', false)).toBe('/Users/d/repo/src/a.ts');
    expect(pathKey('/Users/d/repo/lib/../src//a.ts', false)).toBe('/Users/d/repo/src/a.ts');
    expect(pathKey('/../a', false)).toBe('/a');
  });

  it('keeps .. when asked not to fold it, still dropping . and empty segments', () => {
    expect(pathKey('/home/me/proj/../shared//./x.ts', false, false)).toBe('/home/me/proj/../shared/x.ts');
  });

  it('keeps case and backslashes on macOS/Linux', () => {
    expect(pathKey('/A/b\\c', false)).toBe('/A/b\\c');
  });

  it('ignores separator style and case on Windows', () => {
    expect(pathKey('C:\\repo/src/a.ts', true)).toBe(pathKey('c:\\Repo\\src\\A.ts', true));
  });
});
