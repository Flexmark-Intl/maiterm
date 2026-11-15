/**
 * An ssh command as maiTerm stores and passes it: one string of shell words.
 *
 * The TypeScript twin of `src-tauri/src/util/ssh_words.rs`, and the two must read and write
 * alike: a key built here (`connectArgs`) is compared with one Rust built (`watch::connect_args`).
 * The command comes off the ssh's argv joined as shell words (`pty::manager::ssh_command_line`),
 * so `-i '/Users/me/My Keys/id'` is two words. **Never split a stored command on whitespace:
 * `ssh -i "/Users/me/My Keys/id" ews@nova` split that way connects to the host `Keys/id`.**
 *
 * Compatible with what was stored before: a plain word stays bare (`-o Port=2222`, `~/.ssh/k`),
 * a string a quote leaves unbalanced (a lossy `ps` line naming `Bob's Keys`) is read on
 * whitespace as it always was, and a backslash outside quotes escapes only whitespace and
 * quote chars, so a bare Windows path (`C:\Users\me\.ssh\id`) still reads as itself.
 */

/** One word and where it sits in the string it was read from (`raw` is `s.slice(start, end)`). */
export interface WordSpan {
  value: string;
  start: number;
  end: number;
}

const BLANK = new Set([' ', '\t', '\n', '\r']);

/** The shell-words reading of `s` with each word's span, or null when a quote is left open. */
export function parseWordSpans(s: string): WordSpan[] | null {
  const out: WordSpan[] = [];
  // Walk by code point, as Rust's `chars()` does: an index is still a UTF-16 offset.
  const cps = Array.from(s);
  let word = '';
  let start = -1;
  let state: 'between' | 'word' | 'single' | 'double' = 'between';
  let at = 0;
  const push = (end: number) => {
    out.push({ value: word, start, end });
    word = '';
    start = -1;
  };
  for (let i = 0; i < cps.length; i++) {
    const c = cps[i];
    const here = at;
    at += c.length;
    if (state === 'between' || state === 'word') {
      if (BLANK.has(c)) {
        if (state === 'word') {
          push(here);
          state = 'between';
        }
        continue;
      }
      if (start < 0) start = here;
      if (c === "'") state = 'single';
      else if (c === '"') state = 'double';
      else if (c === '\\') {
        state = 'word';
        const n = cps[i + 1];
        if (n === ' ' || n === '\t' || n === "'" || n === '"') {
          word += n;
          i++;
          at += n.length;
        } else if (n === '\n') {
          i++;
          at += 1;
        } else {
          word += '\\';
        }
      } else {
        word += c;
        state = 'word';
      }
    } else if (state === 'single') {
      if (c === "'") state = 'word';
      else word += c;
    } else {
      if (c === '"') state = 'word';
      else if (c === '\\') {
        const n = cps[i + 1];
        if (n === '$' || n === '`' || n === '"' || n === '\\') {
          word += n;
          i++;
          at += 1;
        } else if (n === '\n') {
          i++;
          at += 1;
        } else {
          word += '\\';
        }
      } else word += c;
    }
  }
  if (state === 'single' || state === 'double') return null;
  if (state === 'word') push(at);
  return out;
}

/** The words of a stored or typed ssh command. Never fails: an unbalanced quote is read on
 *  whitespace, as everything was before quoting. */
export function splitWords(s: string): string[] {
  return parseWordSpans(s)?.map(w => w.value) ?? s.split(/\s+/).filter(Boolean);
}

/** Characters a word may hold and still go bare (Rust's `is_bare`: `char::is_alphanumeric` is
 *  Unicode Alphabetic or Numeric). */
const BARE = /^[\p{Alphabetic}\p{N}\-_./:@%+=,~\\]+$/u;

/** One word, quoted only if it needs it: `'…'`, with a `'` inside written `'\''`. A backslash
 *  goes bare unless it ends the word (Rust's `quote` says why): a Windows path keeps the spelling
 *  cmd.exe, where `'` is not a quote, needs. */
export function quoteWord(w: string): string {
  return BARE.test(w) && !w.endsWith('\\') ? w : `'${w.replace(/'/g, "'\\''")}'`;
}

/** Words joined so `splitWords` (and Rust, and a POSIX shell) reads them back exactly. */
export function joinWords(words: readonly string[]): string {
  return words.map(quoteWord).join(' ');
}
