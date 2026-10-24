import { describe, expect, it } from 'vitest';
import { strings, type Box, type StringNode } from './strings';

const node = (taskId: string, dependsOn: string[] = [], over: Partial<StringNode> = {}): StringNode => ({
  taskId,
  dependsOn,
  active: false,
  waiting: false,
  done: false,
  ...over,
});
const box = (l: number, t: number, w = 200, h = 80): Box => ({ l, r: l + w, t, b: t + h });

describe('strings', () => {
  it('one string per dependency, upstream to downstream, side to side when it sits to the right', () => {
    const out = strings([[node('a')], [node('b', ['a'])]], new Map([['a', box(0, 0)], ['b', box(240, 0)]]));
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ key: 'a>b', from: 'a', to: 'b', kind: 'dep', held: false });
    expect(out[0].d).toBe('M200,40 C220,40 220,40 240,40');
  });

  it('a wrapped wave is reached from below', () => {
    const out = strings([[node('a')], [node('b', ['a'])]], new Map([['a', box(0, 0)], ['b', box(0, 200)]]));
    expect(out[0].d).toBe('M14,80 C14,140 14,140 14,200');
  });

  it('an active upstream runs; amber when the downstream waits on it', () => {
    const pos = new Map([['a', box(0, 0)], ['b', box(240, 0)]]);
    expect(strings([[node('a', [], { active: true })], [node('b', ['a'])]], pos)[0]).toMatchObject({ kind: 'run', held: false });
    expect(strings([[node('a', [], { active: true })], [node('b', ['a'], { waiting: true })]], pos)[0]).toMatchObject({ kind: 'run', held: true });
  });

  it('a waiter on an idle upstream is the amber dash; a done upstream is met and holds nothing', () => {
    const pos = new Map([['a', box(0, 0)], ['b', box(240, 0)]]);
    expect(strings([[node('a')], [node('b', ['a'], { waiting: true })]], pos)[0]).toMatchObject({ kind: 'wait', held: true });
    expect(strings([[node('a', [], { done: true })], [node('b', ['a'], { waiting: true })]], pos)[0]).toMatchObject({ kind: 'met', held: false });
  });

  it('a dependency off the board, unmeasured, repeated or on itself draws once or not at all', () => {
    const waves = [[node('a')], [node('b', ['a', 'a', 'hand-made', 'b'])], [node('c', ['a'])]];
    const out = strings(waves, new Map([['a', box(0, 0)], ['b', box(240, 0)]]));
    expect(out.map((s) => s.key)).toEqual(['a>b']);
  });

  it('nothing in, nothing out', () => {
    expect(strings([], new Map())).toEqual([]);
  });
});
