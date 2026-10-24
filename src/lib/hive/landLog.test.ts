import { describe, expect, it } from 'vitest';
import { flags, kindLabel, moveLine, offersUndo, shownEntries, suspectLines, undoBlocked, undoConfirm } from './landLog';
import type { HiveLandLog, HiveLandLogEntry, HiveUndoPreview } from '$lib/tauri/types';

const entry = (over: Partial<HiveLandLogEntry> = {}): HiveLandLogEntry => ({
  id: 'L-1a2b3c4d/web', land: 'L-1a2b3c4d', kind: 'land', task_id: 't1', worker: 'login', title: 'Login form', folder: 'web', target: 'main',
  reviewed_sha: 'a'.repeat(40), tested_sha: 'b'.repeat(40), merge_commit: null, base_before: '1111111aaaa', base_after: '2222222bbbb',
  summary: 's', by: 'coordinator', request: null, review: 'pass', undoes: null, pushed: 'off', check: '', at: 'now', undone_by: null, suspect: false,
  ...over,
});

const preview = (over: Partial<HiveUndoPreview> = {}): HiveUndoPreview => ({
  id: 'L-1a2b3c4d/web', land: 'L-1a2b3c4d', folder: 'web', target: 'main', title: 'Login form', worker: 'login', method: 'fast-forward',
  commits: [{ sha: 'c'.repeat(40), subject: 'Add the login form' }], commits_total: 1, tip: '3333333cccc', config_files: [], config_fp: null,
  has_check: true, refusal: null, ...over,
});

const log = (over: Partial<HiveLandLog> = {}): HiveLandLog => ({ entries: [], repos: [], undos: [], integration: false, ...over });

describe('land log', () => {
  it('says what each line did', () => {
    expect(kindLabel(entry())).toBe('Landed');
    expect(kindLabel(entry({ kind: 'base_check', check: 'failed' }))).toBe('Post-land check failed');
    expect(moveLine(entry())).toBe('main 1111111 → 2222222');
    expect(moveLine(entry({ kind: 'base_check' }))).toBe('main at 2222222');
    expect(flags(entry({ undone_by: 'U-9/web', suspect: true, pushed: 'held: rejected' }))).toEqual(['undone by U-9/web', 'suspect', 'push held: rejected']);
  });

  it('offers Undo only on a land, and says why when it is blocked', () => {
    expect(offersUndo(entry())).toBe(true);
    expect(offersUndo(entry({ kind: 'undo' }))).toBe(false);
    expect(offersUndo(entry({ kind: 'base_check' }))).toBe(false);
    expect(undoBlocked(entry(), log())).toBeNull();
    expect(undoBlocked(entry({ undone_by: 'U-9/web' }), log())).toBe('Already undone (U-9/web).');
    const waiting = log({ undos: [{ id: 'U-1', undoes: 'L-1a2b3c4d/web', state: 'awaiting_human', by: 'coordinator' }] });
    expect(undoBlocked(entry(), waiting)).toBe('Its undo waits for your approval on a card.');
    const held = log({ undos: [{ id: 'U-1', undoes: 'L-1a2b3c4d/web', state: 'held', by: 'human' }] });
    expect(undoBlocked(entry(), held), 'a held undo is undone again from here').toBeNull();
  });

  it('lists newest first, a worker only its own', () => {
    const l = log({ entries: [entry({ id: 'a', task_id: 't1' }), entry({ id: 'b', task_id: 't2' }), entry({ id: 'c', task_id: 't1' })] });
    expect(shownEntries(l).map((e) => e.id)).toEqual(['c', 'b', 'a']);
    expect(shownEntries(l, 't1').map((e) => e.id)).toEqual(['c', 'a']);
  });

  it('names the repo, the branch it writes, the commits and any config before anything is written', () => {
    const c = undoConfirm(preview());
    expect(c.title).toBe('Undo L-1a2b3c4d/web?');
    expect(c.head).toEqual([
      'Writes a revert commit to main in web, on top of 3333333. Nothing is reset: the land stays in the log and in git.',
      'Reverts the 1 commit that landed “Login form” (login), as one commit:',
    ]);
    expect(c.commits).toEqual([{ sha: 'c'.repeat(40), subject: 'Add the login form' }]);
    expect(c.tail).toEqual(['Its land check runs on the revert before it lands.', "Workers waiting on login stay blocked; the worker isn't reopened."]);
    expect(c.refusal).toBeNull();

    const m = undoConfirm(preview({ method: 'merge', target: 'hive/ship', commits_total: 25, config_files: ['.claude/settings.json'], has_check: false }));
    expect(m.head[0]).toContain('to hive/ship in web');
    expect(m.head[1]).toBe('Reverts the merge that landed “Login form” (login), and with it 25 commits:');
    expect(m.more).toBe(24);
    expect(m.tail[0]).toBe('It changes agent or execution config: .claude/settings.json.');
    expect(m.tail[1]).toBe('web has no land check: nothing tests the revert before it lands.');
    expect(undoConfirm(preview({ refusal: ' later work builds on it ' })).refusal).toBe('later work builds on it');
  });

  it('warns about a suspect repo', () => {
    expect(suspectLines(log({ repos: [{ folder: 'web', target: 'main', suspect: true, suspects: ['L-1/web'] }, { folder: 'api', target: 'main', suspect: false, suspects: [] }] })))
      .toEqual(['web: main fails its land check alone since L-1/web. Lands there need a fix forward until a land or an undo passes its check.']);
  });
});
