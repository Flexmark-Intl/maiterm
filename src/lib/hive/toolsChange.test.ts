import { describe, expect, it } from 'vitest';
import { confirmLine, labelOf, nextTools, seedPicked, tickedHosts, toolsChanged, undetected } from './toolsChange';

const detected = [
  { id: 'mcp:fal', label: 'fal' },
  { id: 'cli:ffmpeg', label: 'ffmpeg' },
  { id: 'skill:browse', label: 'browse' },
];

describe('seedPicked', () => {
  it('starts from the Hive tools without a setup-only program', () => {
    expect([...seedPicked(['cli:ffmpeg', 'cli:brew', 'mcp:gone'])]).toEqual(['cli:ffmpeg', 'mcp:gone']);
  });
});

describe('undetected', () => {
  it('lists the Hive tools this Mac no longer has, never brew', () => {
    expect(undetected(['cli:ffmpeg', 'mcp:gone', 'cli:brew'], detected)).toEqual(['mcp:gone']);
    expect(undetected([], detected)).toEqual([]);
  });
});

describe('nextTools', () => {
  it('keeps the Hive order, then adds new picks in the picker order', () => {
    const picked = new Set(['skill:browse', 'mcp:gone', 'cli:ffmpeg', 'mcp:fal']);
    expect(nextTools(['mcp:gone', 'cli:ffmpeg'], picked, detected)).toEqual(['mcp:gone', 'cli:ffmpeg', 'mcp:fal', 'skill:browse']);
  });
  it('drops what is unticked and never sends brew', () => {
    expect(nextTools(['cli:ffmpeg', 'cli:brew'], new Set(['cli:brew', 'mcp:fal']), detected)).toEqual(['mcp:fal']);
  });
});

describe('toolsChanged', () => {
  it('ignores order', () => {
    expect(toolsChanged(['a', 'b'], ['b', 'a'])).toBe(false);
    expect(toolsChanged(['a', 'b'], ['a'])).toBe(true);
    expect(toolsChanged(['a'], ['a', 'c'])).toBe(true);
  });
  it('counts a saved setup-only program as something to drop', () => {
    expect(toolsChanged(['a', 'cli:brew'], ['a'])).toBe(true);
  });
});

describe('labelOf', () => {
  it('uses the detected label, else the id without its kind', () => {
    expect(labelOf('mcp:fal', [{ id: 'mcp:fal', label: 'fal studio' }])).toBe('fal studio');
    expect(labelOf('mcp:gone', detected)).toBe('gone');
    expect(labelOf('comfyui', detected)).toBe('comfyui');
  });
});

describe('tickedHosts', () => {
  it('ticks every offered host but the ones unticked', () => {
    expect(tickedHosts(['fal.run', 'fal.media'], new Set())).toEqual(['fal.run', 'fal.media']);
    expect(tickedHosts(['fal.run', 'fal.media'], new Set(['fal.media', 'gone.example']))).toEqual(['fal.run']);
  });
});

describe('confirmLine', () => {
  const label = (id: string) => labelOf(id, detected);
  it('names what is added and removed, by label', () => {
    expect(confirmLine(['mcp:fal', 'skill:browse'], ['cli:ffmpeg'], 2, label)).toBe('Adds fal, browse. Removes ffmpeg. Media workers may also reach 2 hosts.');
    expect(confirmLine([], ['mcp:gone'], 0, label)).toBe('Removes gone.');
    expect(confirmLine(['mcp:fal'], [], 1, label)).toBe('Adds fal. Media workers may also reach 1 host.');
  });
  it('counts the output folders a local server brings', () => {
    expect(confirmLine(['mcp:meshy'], [], 0, label, 1)).toBe('Adds meshy. galleryAdd also takes files from one more folder.');
    expect(confirmLine(['mcp:meshy', 'mcp:minimax'], [], 0, label, 2)).toBe('Adds meshy, minimax. galleryAdd also takes files from 2 more folders.');
    expect(confirmLine(['mcp:fal'], [], 0, label)).toBe('Adds fal.');
  });
});
