import { describe, expect, it } from 'vitest';
import { isSpinnerTick } from './titleSpinner';

describe('isSpinnerTick', () => {
  it('is a tick when only the leading glyph changes', () => {
    expect(isSpinnerTick('◐ Counting and lighthouse story', '◑ Counting and lighthouse story')).toBe(true);
    expect(isSpinnerTick('✳ Claude Code', '◐ Claude Code')).toBe(true);
    expect(isSpinnerTick('⠂ Fix the bug', '⠐ Fix the bug')).toBe(true);
  });

  it('is not a tick for an identical repeat (a prompt redraw retries the probe)', () => {
    expect(isSpinnerTick('◐ Claude Code', '◐ Claude Code')).toBe(false);
    expect(isSpinnerTick('dprusak@mac:~/x', 'dprusak@mac:~/x')).toBe(false);
  });

  it('is not a tick when the text changes', () => {
    expect(isSpinnerTick('◐ Claude Code', '◑ Fix the bug')).toBe(false);
    expect(isSpinnerTick('✳ Claude Code', 'dprusak@mac:~/x')).toBe(false);
    expect(isSpinnerTick('dprusak@mac:~/x', 'root@nova:~')).toBe(false);
  });

  it('is not a tick for titles without a leading glyph', () => {
    expect(isSpinnerTick('', '◐ Claude Code')).toBe(false);
    expect(isSpinnerTick('vim foo', 'nvim foo')).toBe(false);
    expect(isSpinnerTick('[1] build', '[2] build')).toBe(false);
  });
});
