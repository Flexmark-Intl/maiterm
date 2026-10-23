import { describe, expect, it } from 'vitest';
import { localShellKind, shellQuote } from './shellQuote';

describe('shellQuote', () => {
  it.each([
    ['/Users/d/plain-file_1.ts', '/Users/d/plain-file_1.ts'],
    ['/Users/d/My Docs/a b.txt', '/Users/d/My\\ Docs/a\\ b.txt'],
    ["/tmp/it's", "/tmp/it\\'s"],
    ['/tmp/$HOME `x` (1) & *;|<>', '/tmp/\\$HOME\\ \\`x\\`\\ \\(1\\)\\ \\&\\ \\*\\;\\|\\<\\>'],
    ['/tmp/café', '/tmp/caf\\é'],
    ['/tmp/a\nb', "'/tmp/a\nb'"],
    ["/tmp/it's\nx", "'/tmp/it'\\''s\nx'"],
    ['', "''"],
  ])('posix %j', (input, want) => {
    expect(shellQuote(input, 'posix')).toBe(want);
    expect(shellQuote(input)).toBe(want);
  });

  it.each([
    ['C:\\Users\\d\\My Docs\\a.txt', "'C:\\Users\\d\\My Docs\\a.txt'"],
    ["C:\\it's", "'C:\\it''s'"],
    ['C:\\it\u2019s', "'C:\\it\u2019\u2019s'"],
    ['C:\\$env:x `y`', "'C:\\$env:x `y`'"],
  ])('pwsh %j', (input, want) => {
    expect(shellQuote(input, 'pwsh')).toBe(want);
  });

  it.each([
    ['C:\\Users\\d\\My Docs\\a.txt', '"C:\\Users\\d\\My Docs\\a.txt"'],
    ['C:\\a&b^c', '"C:\\a&b^c"'],
  ])('cmd %j', (input, want) => {
    expect(shellQuote(input, 'cmd')).toBe(want);
  });
});

describe('localShellKind', () => {
  it('is posix off Windows whatever the preference says', () => {
    expect(localShellKind(false, 'cmd')).toBe('posix');
  });
  it.each([
    ['cmd', 'cmd'],
    ['pwsh', 'pwsh'],
    ['powershell', 'pwsh'],
    ['gitbash', 'posix'],
    ['wsl', 'posix'],
    ['auto', 'pwsh'],
    [undefined, 'pwsh'],
  ] as const)('windows %s → %s', (pref, want) => {
    expect(localShellKind(true, pref)).toBe(want);
  });
});
