/**
 * Another agent's text, made safe to quote inside maiTerm's own message to a Hive's coordinator:
 * the twin of Rust's `hive::deliver::frame_escape`, for the one message that reaches it from the
 * frontend (its bridge partner, docs/hive.md §5.1). No `⟦`/`⟧` frame of its own, no line that
 * opens `[maiTerm]` (it gets `(quoted) ` in front), and no control or invisible characters (a
 * bracketed-paste end would leave the paste). Line breaks and tabs stay.
 */

const cp = (n: number) => String.fromCodePoint(n);
const span = (a: number, b: number) => `${cp(a)}-${cp(b)}`;

/** Zero-width, bidi controls, the BOM, the soft hyphen: they would hide a forged line from the
 *  checks below, or reorder it on screen. Built from code points, as a literal line or paragraph
 *  separator would end a regex literal. */
const INVISIBLE = new RegExp(`[${span(0x200b, 0x200f)}${span(0x202a, 0x202e)}${span(0x2060, 0x2069)}${cp(0xfeff)}${cp(0xad)}]`, 'g');
/** Every C0 and C1 control but the tab (line breaks are already split off). */
const CONTROL = new RegExp(`[${span(0, 8)}${span(0x0a, 0x1f)}${span(0x7f, 0x9f)}]`, 'g');
/** Every line break a reader may honour, so each gets the `[maiTerm]` check. */
const BREAK = new RegExp(`${cp(0x0d)}${cp(0x0a)}|[${cp(0x0d)}${cp(0x0b)}${cp(0x0c)}${cp(0x85)}${cp(0x2028)}${cp(0x2029)}]`, 'g');
const OPEN = new RegExp(`[${cp(0x27e6)}${cp(0x301a)}]`, 'g');
const CLOSE = new RegExp(`[${cp(0x27e7)}${cp(0x301b)}]`, 'g');

export function frameEscape(text: string): string {
  return text
    .replace(BREAK, '\n')
    .split('\n')
    .map((line) => {
      const clean = line.replace(CONTROL, '').replace(INVISIBLE, '').replace(OPEN, '[').replace(CLOSE, ']');
      return clean.trimStart().slice(0, 9).toLowerCase() === '[maiterm]' ? `(quoted) ${clean}` : clean;
    })
    .join('\n');
}
