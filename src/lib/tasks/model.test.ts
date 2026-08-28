import { describe, expect, it } from 'vitest';
import {
  coerceStatus,
  effectiveStatus,
  isInFlight,
  isParked,
  normalizeWorkstreamName,
  findDuplicate,
  findImportedDuplicate,
  hasUnmetDeps,
  makeTask,
  normalizeTitle,
  statusFromAgent,
  TASK_STATUSES,
} from './model';
import type { Task } from '$lib/tauri/types';

/** Build a task the way the store does, so `normalized_title` matches `title` unless a
 *  test deliberately overrides it (the legacy-row case below). */
const task = (over: Partial<Task> = {}): Task => {
  const base = makeTask({ title: over.title ?? 'x', tab_id: over.tab_id, status: over.status });
  return { ...base, ...over, normalized_title: over.normalized_title ?? base.normalized_title };
};

describe('normalizeTitle', () => {
  it('collapses the cosmetic drift agents produce when restating a task', () => {
    for (const input of ['Add auth guard', 'add auth guard', '  Add   auth  guard ', 'Add auth guard.', 'ADD AUTH GUARD:']) {
      expect(normalizeTitle(input)).toBe('add auth guard');
    }
  });

  it('agrees with the Rust normalizer on the whitespace both languages get wrong', () => {
    // U+0085 NEL: whitespace to Rust, not to JS's \s. U+FEFF BOM: the reverse.
    // Both must collapse, or a pasted title re-duplicates on every restart.
    expect(normalizeTitle('a\u0085b')).toBe('a b');
    expect(normalizeTitle('a\uFEFFb')).toBe('a b');
    expect(normalizeTitle('\uFEFFAdd auth guard')).toBe('add auth guard');
    expect(normalizeTitle('ab\u0085')).toBe('ab');
    expect(normalizeTitle('ab\uFEFF')).toBe('ab');
  });

  it('strips a trailing separator left behind after collapsing', () => {
    expect(normalizeTitle('Add auth guard .')).toBe('add auth guard');
  });

  it('keeps genuinely different titles apart', () => {
    expect(normalizeTitle('Add auth guard')).not.toBe(normalizeTitle('Add auth guards'));
    // Only trailing separators are trimmed — interior punctuation carries meaning.
    expect(normalizeTitle('Fix v1.2 parser')).toBe('fix v1.2 parser');
  });
});

describe('findDuplicate', () => {
  it('matches on normalized title so a re-primed agent does not duplicate its list', () => {
    const list = [task({ title: 'Write the tests', tab_id: 'a' })];
    expect(findDuplicate(list, 'write the tests.', 'a')).toBeDefined();
  });

  it('is scoped per tab — two agents may each own the same-titled task', () => {
    const list = [task({ title: 'Write the tests', tab_id: 'a' })];
    expect(findDuplicate(list, 'Write the tests', 'b')).toBeUndefined();
  });

  it('treats undefined and null assignees as the same backlog lane', () => {
    const list = [task({ title: 'Ship it', tab_id: null })];
    expect(findDuplicate(list, 'Ship it', undefined)).toBeDefined();
  });

  it('falls back to hashing the title when normalized_title was never stored', () => {
    const list = [task({ title: 'Legacy row', tab_id: 'a', normalized_title: '' })];
    expect(findDuplicate(list, 'legacy row', 'a')).toBeDefined();
  });
});

describe('findDuplicate — reclaiming work after a tab id change', () => {
  it('reclaims an unassigned row so a reloaded tab does not duplicate its whole list', () => {
    // A reload is duplicate-then-close: the replacement tab has a new id, and the closing
    // tab released its tasks to the backlog.
    const released = task({ title: 'Wire the parser', tab_id: null, status: 'active' });
    expect(findDuplicate([released], 'Wire the parser', 'new-tab-id')?.id).toBe(released.id);
  });

  it('will not resurrect a finished backlog task for a new claimant', () => {
    const done = task({ title: 'Wire the parser', tab_id: null, status: 'done' });
    expect(findDuplicate([done], 'Wire the parser', 'new-tab-id')).toBeUndefined();
  });

  it('prefers the caller\'s own row over an unassigned one with the same title', () => {
    const backlog = task({ id: 'b', title: 'Ship it', tab_id: null, status: 'active' });
    const mine = task({ id: 'm', title: 'Ship it', tab_id: 'a', status: 'active' });
    expect(findDuplicate([backlog, mine], 'Ship it', 'a')?.id).toBe('m');
  });

  it('does not let an unassigned create claim another tab\'s row', () => {
    const theirs = task({ title: 'Ship it', tab_id: 'other', status: 'active' });
    expect(findDuplicate([theirs], 'Ship it', null)).toBeUndefined();
  });
});

describe('findDuplicate — what the unassigned fallback must NOT do', () => {
  it('never matches a row still tagged to a live tab', () => {
    // Reload remaps ids directly (tasksStore.remapTab); the backlog fallback exists only
    // for genuinely closed tabs, and must not let one tab adopt another's active work.
    const theirs = task({ title: 'Wire the parser', tab_id: 'T1', status: 'active' });
    expect(findDuplicate([theirs], 'Wire the parser', 'T2')).toBeUndefined();
  });

  it('matches by normalized title, so cosmetic drift still reclaims', () => {
    const released = task({ title: 'Wire the parser', tab_id: null, status: 'active' });
    expect(findDuplicate([released], 'wire  the parser.', 'T2')?.id).toBe(released.id);
  });
});

describe('the backlog is a parking lot, not a to-do list', () => {
  it('starts new work in todo, never in the parking lot', () => {
    // If new tasks defaulted to backlog it would be an inbox, and everything that exempts
    // backlog from staleness would silently hide live work.
    expect(makeTask({ title: 'x' }).status).toBe('todo');
    expect(coerceStatus(undefined)).toBe('todo');
    expect(coerceStatus('nonsense')).toBe('todo');
  });

  it("maps a runtime's not-started state to todo, not backlog", () => {
    expect(statusFromAgent('pending')).toBe('todo');
    // ...but an explicit backlog is honoured: the agent meant to shelve it.
    expect(statusFromAgent('backlog')).toBe('backlog');
  });

  it('treats parked work as neither finished nor in flight', () => {
    expect(isParked('backlog')).toBe(true);
    expect(isParked('todo')).toBe(false);
    expect(isInFlight(task({ status: 'backlog' }))).toBe(false);
    expect(isInFlight(task({ status: 'done' }))).toBe(false);
    expect(isInFlight(task({ status: 'todo' }))).toBe(true);
  });

  it('orders the lanes with backlog leftmost, so parking is a move backwards', () => {
    expect(TASK_STATUSES).toEqual(['backlog', 'todo', 'active', 'blocked', 'review', 'done']);
  });
});

describe('workstream names', () => {
  it('dedups the spellings one agent will produce for one job', () => {
    for (const name of ['Auth refactor', 'auth refactor', '  Auth   Refactor ', 'Auth refactor.']) {
      expect(normalizeWorkstreamName(name)).toBe('auth refactor');
    }
  });
});

describe('dedup survives an agent changing how it groups its work', () => {
  it('adopts a loose row when the same title is re-sent under a workstream', () => {
    // The priming asks agents to name their jobs, and initSession fires on every compact,
    // so a list recorded loose and re-sent grouped is the expected flip — not an edge case.
    const loose = task({ title: 'Wire the parser', tab_id: 'T1', workstream_id: null });
    expect(findDuplicate([loose], 'Wire the parser', 'T1', 'ws-auth')?.id).toBe(loose.id);
  });

  it('reuses a grouped row when the same title is re-sent loose', () => {
    const grouped = task({ title: 'Wire the parser', tab_id: 'T1', workstream_id: 'ws-auth' });
    expect(findDuplicate([grouped], 'Wire the parser', 'T1', null)?.id).toBe(grouped.id);
  });

  it('refuses to guess when the title exists in several jobs', () => {
    // Merging these would silently collapse two genuinely different pieces of work.
    const a = task({ title: 'Write the tests', tab_id: 'T1', workstream_id: 'ws-auth' });
    const b = task({ title: 'Write the tests', tab_id: 'T1', workstream_id: 'ws-db' });
    expect(findDuplicate([a, b], 'Write the tests', 'T1', null)).toBeUndefined();
  });

  it('keeps two jobs\' same-titled tasks apart on an exact match', () => {
    const a = task({ id: 'a', title: 'Write the tests', tab_id: 'T1', workstream_id: 'ws-auth' });
    const b = task({ id: 'b', title: 'Write the tests', tab_id: 'T1', workstream_id: 'ws-db' });
    expect(findDuplicate([a, b], 'Write the tests', 'T1', 'ws-db')?.id).toBe('b');
  });

  it('never adopts a row that belongs to a different job', () => {
    const other = task({ title: 'Wire the parser', tab_id: 'T1', workstream_id: 'ws-db' });
    // Incoming names ws-auth; the existing row is filed under ws-db, so moving it would
    // be a guess. A new row is correct here.
    expect(findDuplicate([other], 'Wire the parser', 'T1', 'ws-auth')).toBeUndefined();
  });
});

describe('findImportedDuplicate', () => {
  it('still recognizes a row a human has filed into a workstream', () => {
    // The importer re-reads the runtime's store every 5s. If filing a card into a job hid
    // it from this lookup, the same task would be re-imported forever.
    const filed = task({ title: 'Add the auth guard', tab_id: 'T1', workstream_id: 'ws-auth' });
    expect(findImportedDuplicate([filed], 'Add the auth guard', 'T1')?.id).toBe(filed.id);
  });

  it('still reclaims a released row regardless of grouping', () => {
    const released = task({ title: 'Add the auth guard', tab_id: null, workstream_id: 'ws-auth', status: 'active' });
    expect(findImportedDuplicate([released], 'Add the auth guard', 'T1')?.id).toBe(released.id);
  });
});

describe('coerceStatus', () => {
  it('accepts our own vocabulary unchanged', () => {
    for (const s of ['backlog', 'active', 'blocked', 'review', 'done'] as const) {
      expect(coerceStatus(s)).toBe(s);
    }
  });

  it('translates the runtimes\' vocabulary rather than storing it raw', () => {
    // A raw "completed" is invisible on the board (lanes match by equality) and
    // permanently unfinished to the dependency check, which wedges its dependents.
    expect(coerceStatus('completed')).toBe('done');
    expect(coerceStatus('in_progress')).toBe('active');
    // 'pending' is not-started work, which is `todo`. It must NOT land in `backlog`:
    // that is the parking lot, exempt from staleness, so live work would go invisible.
    expect(coerceStatus('pending')).toBe('todo');
    expect(coerceStatus('nonsense')).toBe('todo');
    expect(coerceStatus(undefined)).toBe('todo');
  });
});

describe('dependencies', () => {
  it('blocks while a prerequisite is unfinished', () => {
    const dep = task({ id: 'd1', status: 'active' });
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['d1'] });
    expect(hasUnmetDeps(t, [dep, t])).toBe(true);
    expect(effectiveStatus(t, [dep, t])).toBe('blocked');
  });

  it('unblocks once the prerequisite is done', () => {
    const dep = task({ id: 'd1', status: 'done' });
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['d1'] });
    expect(effectiveStatus(t, [dep, t])).toBe('backlog');
  });

  it('ignores a prerequisite that no longer exists rather than wedging forever', () => {
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['gone'] });
    expect(hasUnmetDeps(t, [t])).toBe(false);
  });

  it('keeps blocking on a prerequisite parked by archiving its tab', () => {
    // Off `Workspace.tasks` but not deleted: archiving the tab that owned the migration
    // must not advertise the backfill waiting on it as ready work.
    const t = task({ id: 't1', status: 'todo', blocked_by: ['parked'] });
    expect(hasUnmetDeps(t, [t])).toBe(false); // unknown id: gone, so it must not wedge
    expect(hasUnmetDeps(t, [t], new Set(['parked']))).toBe(true);
    expect(effectiveStatus(t, [t], new Set(['parked']))).toBe('blocked');
    // Restoring the tab puts the row back in the list, where its real status decides.
    const dep = task({ id: 'parked', status: 'done' });
    expect(effectiveStatus(t, [dep, t], new Set(['parked']))).toBe('todo');
  });

  it('never overrides done — a finished task is finished', () => {
    const dep = task({ id: 'd1', status: 'active' });
    const t = task({ id: 't1', status: 'done', blocked_by: ['d1'] });
    expect(effectiveStatus(t, [dep, t])).toBe('done');
  });
});

describe('statusFromAgent', () => {
  it('maps the runtimes\' vocabulary onto ours', () => {
    expect(statusFromAgent('completed')).toBe('done');
    expect(statusFromAgent('in_progress')).toBe('active');
    expect(statusFromAgent('pending')).toBe('todo');
    expect(statusFromAgent(undefined)).toBe('todo');
    expect(statusFromAgent('something-new')).toBe('todo');
  });

  it('lets an unmet dependency win over pending/in_progress', () => {
    expect(statusFromAgent('in_progress', true)).toBe('blocked');
    // ...but not over completion.
    expect(statusFromAgent('completed', true)).toBe('done');
  });
});
