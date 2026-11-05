/** A leading status glyph (1–3 symbols that are not letters, digits or spaces), a space, and
 *  the rest of the title. Claude Code titles itself `✳ <topic>` at rest and animates the glyph
 *  (`◐`/`◑`) while it works. */
const LEADING_GLYPH = /^([^\p{L}\p{N}\s]{1,3})\s+(.+)$/su;

/**
 * True when `next` differs from `prev` ONLY in its leading status glyph: the program that set
 * the title is still the one drawing it, animating a spinner. Never true for an identical
 * repeat, or for any change to the title's text.
 */
export function isSpinnerTick(prev: string, next: string): boolean {
  if (!prev || prev === next) return false;
  const a = LEADING_GLYPH.exec(prev);
  const b = LEADING_GLYPH.exec(next);
  return !!a && !!b && a[1] !== b[1] && a[2] === b[2];
}
