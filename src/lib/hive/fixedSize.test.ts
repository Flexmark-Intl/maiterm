import { describe, expect, it } from 'vitest';
import { clampTerminalHeight, fitAndSyncAllowed, TERMINAL_HEIGHT_DEFAULT, wheelScrollsSlot } from './fixedSize';

describe('fitAndSyncAllowed', () => {
  it('never lets a fixed-size pane fit or resize, whatever else holds', () => {
    for (const visible of [true, false]) {
      for (const connected of [true, false]) {
        expect(fitAndSyncAllowed(true, visible, connected)).toBe(false);
      }
    }
  });

  it("keeps an ordinary pane's gate: visible and in the document", () => {
    expect(fitAndSyncAllowed(false, true, true)).toBe(true);
    expect(fitAndSyncAllowed(false, false, true)).toBe(false);
    expect(fitAndSyncAllowed(false, true, false)).toBe(false);
    expect(fitAndSyncAllowed(false, false, false)).toBe(false);
  });
});

describe('wheelScrollsSlot', () => {
  it('gives a sideways swipe or Shift+wheel over a fixed-size pane to its slot', () => {
    expect(wheelScrollsSlot(true, 30, 0, false)).toBe(true);
    expect(wheelScrollsSlot(true, -12, 4, false)).toBe(true);
    expect(wheelScrollsSlot(true, 0, 40, true)).toBe(true);
  });

  it('keeps a vertical wheel on the scrollback', () => {
    expect(wheelScrollsSlot(true, 0, 40, false)).toBe(false);
    expect(wheelScrollsSlot(true, 3, -20, false)).toBe(false);
    expect(wheelScrollsSlot(true, 0, 0, false)).toBe(false);
  });

  it('never takes a wheel from an ordinary pane', () => {
    expect(wheelScrollsSlot(false, 30, 0, false)).toBe(false);
    expect(wheelScrollsSlot(false, 0, 40, true)).toBe(false);
  });
});

describe('clampTerminalHeight', () => {
  it('keeps the split between 15 and 85 percent', () => {
    expect(clampTerminalHeight(5)).toBe(15);
    expect(clampTerminalHeight(99)).toBe(85);
    expect(clampTerminalHeight(40.4)).toBe(40);
  });

  it('falls back to the default on garbage', () => {
    expect(clampTerminalHeight(Number('nope'))).toBe(TERMINAL_HEIGHT_DEFAULT);
    expect(clampTerminalHeight(Infinity)).toBe(TERMINAL_HEIGHT_DEFAULT);
  });
});
