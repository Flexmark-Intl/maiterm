import { describe, expect, it } from 'vitest';
import { doneLabel, doneNotice, doneStaleTip, landedLine, receiptLine } from './finish';

describe('receiptLine', () => {
  it('splits the kind off maiTerm’s description', () => {
    expect(receiptLine('land L-ab12cd34/web: “Login API” into main, at a1b2c3d')).toEqual({ kind: 'land', text: 'L-ab12cd34/web: “Login API” into main, at a1b2c3d' });
    expect(receiptLine('commit web:a1b2c3d on main')).toEqual({ kind: 'commit', text: 'web:a1b2c3d on main' });
    expect(receiptLine('check j-0000aaa3: `cargo test` passed').kind).toBe('check');
    expect(receiptLine('file /r/web/README.md').kind).toBe('file');
    expect(receiptLine('asset a-0123456789ab: “Hero” (image)').kind).toBe('asset');
  });

  it('a line of a kind it doesn’t know is kept whole', () => {
    expect(receiptLine('vote 3 of 4')).toEqual({ kind: 'other', text: 'vote 3 of 4' });
    expect(receiptLine('land')).toEqual({ kind: 'other', text: 'land' });
  });
});

describe('landedLine', () => {
  const l = { land: 'L-1/web', worker: 'api', title: 'Login API', folder: 'web', target: 'main', base_after: 'a1b2c3d4e5f6', undone: false, suspect: false };
  it('names the work, where it landed and the commit', () => {
    expect(landedLine(l)).toBe('“Login API” (api) into main in web, at a1b2c3d');
  });
  it('says what became of it since', () => {
    expect(landedLine({ ...l, undone: true, suspect: true, worker: '', base_after: '' })).toBe('“Login API” into main in web · undone · suspect');
  });
});

describe('doneLabel', () => {
  const at = new Date(2026, 9, 23, 14, 5).getTime();
  it('today shows the time, another day the date too', () => {
    expect(doneLabel(at, new Date(2026, 9, 23, 20, 0).getTime())).toBe('Done · 14:05');
    expect(doneLabel(at, new Date(2026, 9, 25, 9, 0).getTime())).toBe('Done · Oct 23, 14:05');
  });
  it('no time recorded is just Done', () => {
    expect(doneLabel(null)).toBe('Done');
    expect(doneLabel(undefined)).toBe('Done');
  });
});

describe('doneNotice', () => {
  it('only the move into done', () => {
    expect(doneNotice('active', 'done', 'Ship it')?.title).toBe('Hive done: Ship it');
    expect(doneNotice('done', 'done', 'Ship it')).toBeNull();
    expect(doneNotice('done', 'active', 'Ship it')).toBeNull();
    expect(doneNotice('active', 'paused', 'Ship it')).toBeNull();
    expect(doneNotice(null, 'done', 'Ship it')).toBeNull();
  });
});

describe('doneStaleTip', () => {
  it('names the land and says nothing wakes the coordinator', () => {
    const t = doneStaleTip('L-1a2b/web');
    expect(t).toContain('Land L-1a2b/web');
    expect(t).toContain('when you wake the Hive');
  });
});
