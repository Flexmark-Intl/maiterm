import { describe, expect, it } from 'vitest';
import { joinWords, parseWordSpans, quoteWord, splitWords } from './shellWords';

/** Words a whitespace split mangles, as `ssh_words::HARD_WORDS` has them in Rust. */
export const HARD_WORDS = [
  '/Users/me/My Keys/id',
  '/Users/me/Bob\'s "Keys"/id',
  '/Users/me/Clés privées/id_ed25519',
  '/Users/me/鍵/id',
  'IdentityFile=/Users/me/a b=c/id',
  'ProxyCommand=ssh -W %h:%p bastion',
  'two  spaces\tand a tab',
  'C:\\Users\\Me\\My Keys\\id',
  '$HOME/`x`/\\',
  '',
];

/** The spelling both sides must give: the same table is asserted in `ssh_words.rs`. */
export const JOINED: [string[], string][] = [
  [['-i', '/Users/me/My Keys/id', 'ews@nova'], "-i '/Users/me/My Keys/id' ews@nova"],
  [['/Users/me/Bob\'s "Keys"/id'], `'/Users/me/Bob'\\''s "Keys"/id'`],
  [['-o', 'Port=2222', '-i', '~/.ssh/k', 'ews@nova'], '-o Port=2222 -i ~/.ssh/k ews@nova'],
  [['/Users/me/Clés/id', '-W', '%h:%p'], '/Users/me/Clés/id -W %h:%p'],
  [['C:\\Users\\me\\id', ''], "'C:\\Users\\me\\id' ''"],
  [['a#b', '$x', 'ews@[::1]'], "'a#b' '$x' 'ews@[::1]'"],
];

describe('shell words', () => {
  it('round-trips every hard word', () => {
    for (const w of HARD_WORDS) {
      const joined = joinWords(['-i', w, 'ews@nova']);
      expect(splitWords(joined), joined).toEqual(['-i', w, 'ews@nova']);
    }
    expect(splitWords(joinWords(HARD_WORDS))).toEqual(HARD_WORDS);
  });

  it('spells every word as Rust does', () => {
    for (const [words, joined] of JOINED) {
      expect(joinWords(words)).toBe(joined);
      expect(splitWords(joined)).toEqual(words);
    }
  });

  it('keeps the old spelling of plain words', () => {
    for (const s of ['-o Port=2222 ews@nova', '-p 2222 -i ~/.ssh/k -J bastion ews@nova', '-x -C ews@nova', '-oUser=ews nova']) {
      expect(joinWords(splitWords(s))).toBe(s);
    }
    expect(quoteWord('/Users/me/Clés/id')).toBe('/Users/me/Clés/id');
  });

  it('reads what a human typed as their shell would', () => {
    expect(splitWords('-i "/Users/me/My Keys/id" ews@nova')).toEqual(['-i', '/Users/me/My Keys/id', 'ews@nova']);
    expect(splitWords('-i /Users/me/My\\ Keys/id ews@nova')).toEqual(['-i', '/Users/me/My Keys/id', 'ews@nova']);
    expect(splitWords('-o "ProxyCommand=ssh -W %h:%p \\"b\\"" h')).toEqual(['-o', 'ProxyCommand=ssh -W %h:%p "b"', 'h']);
    expect(splitWords("a'b c'd")).toEqual(['ab cd']);
    expect(splitWords("ews@nova ''")).toEqual(['ews@nova', '']);
  });

  it('still reads old stored strings', () => {
    expect(splitWords("-i /Users/me/Bob's/id ews@nova")).toEqual(['-i', "/Users/me/Bob's/id", 'ews@nova']);
    expect(parseWordSpans("-i /Users/me/Bob's/id ews@nova")).toBeNull();
    expect(splitWords('-i C:\\Users\\me\\.ssh\\id ews@nova')).toEqual(['-i', 'C:\\Users\\me\\.ssh\\id', 'ews@nova']);
  });

  it('gives each word its span in the original string', () => {
    const s = `ssh  -i "/a b/鍵" 'x'y z`;
    const spans = parseWordSpans(s)!;
    expect(spans.map(w => s.slice(w.start, w.end))).toEqual(['ssh', '-i', '"/a b/鍵"', "'x'y", 'z']);
    expect(spans.map(w => w.value)).toEqual(['ssh', '-i', '/a b/鍵', 'xy', 'z']);
    // A code point outside the BMP is two UTF-16 units: the offsets stay right after it.
    const e = "'😀 x' b";
    expect(parseWordSpans(e)!.map(w => e.slice(w.start, w.end))).toEqual(["'😀 x'", 'b']);
  });
});
