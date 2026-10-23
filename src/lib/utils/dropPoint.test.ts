import { describe, expect, it } from 'vitest';
import { dropPointToCss } from './dropPoint';

describe('dropPointToCss', () => {
  it('scales Windows’ physical pixels down to CSS pixels', () => {
    expect(dropPointToCss({ x: 300, y: 450 }, true, 1.5)).toEqual({ x: 200, y: 300 });
  });

  it('leaves macOS points and Linux logical pixels alone, whatever the ratio', () => {
    expect(dropPointToCss({ x: 300, y: 450 }, false, 2)).toEqual({ x: 300, y: 450 });
    expect(dropPointToCss({ x: 300, y: 450 }, true, 1)).toEqual({ x: 300, y: 450 });
  });
});
