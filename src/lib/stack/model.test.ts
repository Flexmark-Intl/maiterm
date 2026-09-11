import { describe, expect, it } from 'vitest';
import { exitIsCrash, restartAllowed, restartDelay, rollupStatus, shq, startLine, RESTART_WINDOW_MS } from './model';

describe('startLine', () => {
  it('cds first, then runs the command', () => {
    expect(startLine({ cwd: '/srv/app', env: [], command: 'npm run dev' })).toBe(`cd '/srv/app' && npm run dev`);
  });

  it('prefixes env through `env` so fish works too', () => {
    expect(startLine({ cwd: '/srv/app', env: [['PORT', '5173'], ['NAME', "o'brien"]], command: 'npm run dev' }))
      .toBe(`cd '/srv/app' && env PORT='5173' NAME='o'\\''brien' npm run dev`);
  });

  it('skips the cd when there is no cwd', () => {
    expect(startLine({ cwd: '', env: [], command: 'redis-server' })).toBe('redis-server');
  });

  it('quotes a cwd with spaces and quotes', () => {
    expect(shq("/Users/d/My Proj's")).toBe(`'/Users/d/My Proj'\\''s'`);
  });
});

describe('exitIsCrash', () => {
  it('treats 0 and SIGINT as a stop, anything else as a crash', () => {
    expect(exitIsCrash(0)).toBe(false);
    expect(exitIsCrash(130)).toBe(false);
    expect(exitIsCrash(1)).toBe(true);
    expect(exitIsCrash(137)).toBe(true);
  });
});

describe('restart ceiling', () => {
  it('allows five restarts inside the window and refuses the sixth', () => {
    const now = 1_000_000_000;
    const recent = [1, 2, 3, 4].map((i) => now - i * 1000);
    expect(restartAllowed(recent, now)).toBe(true);
    expect(restartAllowed([...recent, now - 5000], now)).toBe(false);
  });

  it('forgets restarts older than the window', () => {
    const now = 1_000_000_000;
    const old = [1, 2, 3, 4, 5].map((i) => now - RESTART_WINDOW_MS - i * 1000);
    expect(restartAllowed(old, now)).toBe(true);
  });

  it('backs off and caps at 30s', () => {
    expect(restartDelay(0)).toBe(1000);
    expect(restartDelay(2)).toBe(4000);
    expect(restartDelay(99)).toBe(30000);
  });
});

describe('rollupStatus', () => {
  it('is null for an empty or fully stopped stack', () => {
    expect(rollupStatus([])).toBeNull();
    expect(rollupStatus(['stopped', 'stopped'])).toBeNull();
  });
  it('reports the worst thing first', () => {
    expect(rollupStatus(['ready', 'crashed'])).toBe('crashed');
    expect(rollupStatus(['ready', 'starting'])).toBe('starting');
    expect(rollupStatus(['ready', 'running', 'stopped'])).toBe('ready');
  });
});
