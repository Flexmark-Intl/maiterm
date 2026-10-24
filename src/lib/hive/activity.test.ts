import { describe, expect, it } from 'vitest';
import { activityLine, contextGauge, formatDiffStat, STICKY_MS, stickyStep, toolLine } from './activity';
import type { HiveDiffStat } from '$lib/tauri/types';

const NOW = Date.parse('2026-10-24T12:00:00Z');
const busy = (toolName?: string, toolDetail?: string) => ({ state: 'active' as const, toolName, toolDetail });

describe('activityLine', () => {
  it.each([
    ['Edit', 'store.ts', 'Editing store.ts'],
    ['Write', 'src/auth/store.ts', 'Editing store.ts'],
    ['MultiEdit', undefined, 'Editing'],
    ['Read', 'README.md', 'Reading README.md'],
    ['Bash', 'npm test\necho done', 'Running npm test'],
    ['Grep', 'refreshToken', 'Searching for refreshToken'],
    ['Glob', '**/*.svelte', 'Finding **/*.svelte'],
    ['Agent', 'Review the diff', 'Delegating: Review the diff'],
    ['mcp__maiterm__hiveReport', undefined, 'Reporting'],
    ['mcp__maiterm__listTasks', undefined, 'Using listTasks'],
    ['mcp__a__b__c', undefined, 'Using b__c'],
    ['WebFetch', 'https://x', 'Using WebFetch'],
  ])('%s %s → %s', (tool, detail, text) => {
    expect(activityLine(busy(tool, detail), NOW)).toEqual({ text, tone: 'busy' });
  });

  it('a permission dialog waits, whatever tool it holds', () => {
    expect(activityLine({ state: 'permission', toolName: 'Bash', toolDetail: 'rm -rf x' }, NOW)).toEqual({ text: 'Waiting for permission', tone: 'wait' });
  });

  it('a question reads as cardState words it, and waits', () => {
    expect(activityLine(busy('AskUserQuestion'), NOW)).toEqual({ text: 'Asking a question', tone: 'wait' });
  });

  it('active with no tool: the last tool line for 4 s, then Thinking', () => {
    const sticky = { text: 'Editing store.ts', at: NOW - 1_000 };
    expect(activityLine(busy(), NOW, sticky)).toEqual({ text: 'Editing store.ts', tone: 'busy' });
    expect(activityLine(busy(), NOW + STICKY_MS, sticky).text).toBe('Thinking');
    expect(activityLine(busy(), NOW).text).toBe('Thinking');
    // A coarse clock behind the sticky still shows it: the store drops it on time.
    expect(activityLine(busy(), NOW - 20_000, sticky).text).toBe('Editing store.ts');
  });

  it('a tool the stale timer cleared is still running: no sticky expiry, no Thinking', () => {
    // agentState clears toolName 15 s into any tool; a 2-minute cargo test has sent no PostToolUse.
    const stale = { state: 'active' as const, staleTool: 'Bash', staleToolDetail: 'cargo test' };
    expect(activityLine(stale, NOW)).toEqual({ text: 'Running cargo test', tone: 'busy' });
    expect(activityLine(stale, NOW + 100_000, { text: 'Running cargo test', at: NOW - 100_000 }).text).toBe('Running cargo test');
    // A dialog over it still reads as the dialog.
    expect(activityLine({ ...stale, state: 'permission' }, NOW).text).toBe('Waiting for permission');
  });

  it('a tool in flight beats the sticky', () => {
    expect(activityLine(busy('Read', 'a.ts'), NOW, { text: 'Editing b.ts', at: NOW }).text).toBe('Reading a.ts');
  });

  it('idle carries its age; under a minute, or with no stamp, just Idle', () => {
    expect(activityLine({ state: 'idle', idleSince: NOW - 2 * 60_000 }, NOW)).toEqual({ text: 'Idle · 2m', tone: 'idle' });
    expect(activityLine({ state: 'idle', idleSince: NOW - 3 * 3_600_000 }, NOW).text).toBe('Idle · 3h');
    expect(activityLine({ state: 'idle', idleSince: NOW - 2 * 86_400_000 }, NOW).text).toBe('Idle · 2d');
    expect(activityLine({ state: 'idle', idleSince: NOW - 30_000 }, NOW).text).toBe('Idle');
    expect(activityLine({ state: 'idle', idleSince: NOW + 5_000 }, NOW).text).toBe('Idle');
    expect(activityLine({ state: 'idle' }, NOW).text).toBe('Idle');
  });

  it('no session is a dash', () => {
    expect(activityLine(undefined, NOW)).toEqual({ text: '—', tone: 'none' });
  });
});

describe('toolLine', () => {
  it('a blank detail is no target', () => {
    expect(toolLine('Edit', '  ')).toBe('Editing');
    expect(toolLine('Bash', '\n  \n')).toBe('Running');
  });
});

describe('stickyStep', () => {
  it('a tool ending hands its line on; the next tool clears it', () => {
    const a = stickyStep(undefined, busy('Edit', 'a.ts'), NOW);
    expect(a).toEqual({ last: 'Editing a.ts', sticky: null });
    const b = stickyStep(a.last, busy(), NOW + 10);
    expect(b).toEqual({ last: undefined, sticky: { text: 'Editing a.ts', at: NOW + 10 } });
    expect(stickyStep(b.last, busy('Read', 'b.ts'), NOW + 20)).toEqual({ last: 'Reading b.ts', sticky: null });
  });

  it('the stale timer is not a tool ending; the real PostToolUse after it is', () => {
    const a = stickyStep(undefined, busy('Bash', 'cargo test'), NOW);
    const b = stickyStep(a.last, { state: 'active', staleTool: 'Bash', staleToolDetail: 'cargo test' }, NOW + 15_000);
    expect(b).toEqual({ last: 'Running cargo test', sticky: null });
    expect(stickyStep(b.last, busy(), NOW + 120_000)).toEqual({ last: undefined, sticky: { text: 'Running cargo test', at: NOW + 120_000 } });
  });

  it('a re-read with nothing new leaves the sticky to expire', () => {
    expect(stickyStep(undefined, busy(), NOW).sticky).toBeUndefined();
  });

  it('a turn ending or the session going holds nothing', () => {
    expect(stickyStep('Editing a.ts', { state: 'idle' }, NOW)).toEqual({ last: undefined, sticky: null });
    expect(stickyStep('Editing a.ts', undefined, NOW)).toEqual({ last: undefined, sticky: null });
  });
});

const stat = (over: Partial<HiveDiffStat> = {}): HiveDiffStat => ({
  task_id: 't1',
  total: { files: 0, insertions: 0, deletions: 0 },
  untracked: 0,
  committed: { files: 0, insertions: 0, deletions: 0 },
  computed_at: NOW,
  error: null,
  ...over,
});

describe('formatDiffStat', () => {
  it('not measured, or no changes: nothing on the card', () => {
    expect(formatDiffStat(undefined)).toEqual({ text: '', tip: '' });
    expect(formatDiffStat(stat())).toEqual({ text: '', tip: 'No changes yet' });
  });

  it('insertions, deletions and new files; committed only in the tip', () => {
    const d = formatDiffStat(stat({
      total: { files: 5, insertions: 1212, deletions: 40 },
      untracked: 3,
      committed: { files: 1, insertions: 1, deletions: 0 },
    }));
    expect(d.text).toBe('+1,212 −40 · 3 new');
    expect(d.tip).toBe([
      'Changed against its landing target: 5 files: 1,212 insertions, 40 deletions',
      '3 new files not yet added to git',
      'Committed (what a land would take): 1 file: 1 insertion, 0 deletions',
    ].join('\n'));
  });

  it('untracked files alone still show', () => {
    expect(formatDiffStat(stat({ untracked: 1 }))).toEqual({ text: '1 new', tip: '1 new file not yet added to git' });
  });

  it('a worktree not measured: partial sums say so, none at all say unavailable', () => {
    const partial = formatDiffStat(stat({ total: { files: 1, insertions: 2, deletions: 3 }, error: 'web: gone' }));
    expect(partial.text).toBe('+2 −3 · partial');
    expect(partial.tip).toContain('Not measured: web: gone');
    expect(formatDiffStat(stat({ error: 'web: gone' }))).toEqual({ text: 'diff unavailable', tip: 'Not measured: web: gone' });
  });
});

describe('contextGauge', () => {
  it('toned as the chat column: high from 75, warn from the threshold', () => {
    expect(contextGauge({ contextPct: 18 }, 60)).toEqual({ pct: 18, tone: 'ok', tip: 'Context 18% used' });
    expect(contextGauge({ contextPct: 62.4 }, 60)?.tone).toBe('warn');
    expect(contextGauge({ contextPct: 50 }, 40)?.tone).toBe('warn');
    expect(contextGauge({ contextPct: 80 }, null)?.tone).toBe('high');
    expect(contextGauge({ contextPct: 140 }, 60)?.pct).toBe(100);
  });

  it('tokens in the tip when known; nothing before the first reply', () => {
    expect(contextGauge({ contextPct: 18, contextUsed: 36_000, contextLimit: 200_000 }, 60)?.tip).toBe('Context 18% used (36k of 200k tokens)');
    expect(contextGauge({}, 60)).toBeNull();
    expect(contextGauge(null, 60)).toBeNull();
  });
});
