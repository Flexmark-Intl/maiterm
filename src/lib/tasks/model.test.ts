import { describe, expect, it } from 'vitest';
import {
  appendNote,
  blocking,
  coerceStatus,
  TASK_NOTE_CAP,
  effectiveStatus,
  FLOW_STATUSES,
  isDropped,
  isInFlight,
  isParked,
  isRetired,
  resolveBlockers,
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

  it('orders the flow with backlog leftmost, so parking is a move backwards', () => {
    expect(FLOW_STATUSES).toEqual(['backlog', 'todo', 'active', 'blocked', 'review', 'done']);
    // Every lane is the flow plus the retraction lane, in that order — the board renders
    // TASK_STATUSES, the steppers walk FLOW_STATUSES, and Dropped sits past Done.
    expect(TASK_STATUSES).toEqual([...FLOW_STATUSES, 'dropped']);
  });
});

describe('dropped is retracted work, not a seventh flavour of done', () => {
  it('does NOT satisfy a dependent — retracting a prerequisite is not doing it', () => {
    const blocker = task({ id: 'b', title: 'migrate schema', status: 'dropped' });
    const dependent = task({ id: 'd', title: 'backfill rows', blocked_by: ['b'] });
    const all = [blocker, dependent];
    expect(hasUnmetDeps(dependent, all)).toBe(true);
    expect(effectiveStatus(dependent, all)).toBe('blocked');
  });

  it('is the difference between retracting a blocker and closing it out', () => {
    const dependent = task({ id: 'd', title: 'backfill rows', blocked_by: ['b'] });
    const done = [task({ id: 'b', title: 'migrate schema', status: 'done' }), dependent];
    // The same edge, the same dependent — only the blocker's lane differs.
    expect(hasUnmetDeps(dependent, done)).toBe(false);
  });

  it('counts as retired but never as in flight or parked', () => {
    expect(isRetired('dropped')).toBe(true);
    expect(isRetired('done')).toBe(true);
    expect(isParked('dropped')).toBe(false);
    expect(isInFlight(task({ status: 'dropped' }))).toBe(false);
  });

  it('short-circuits effectiveStatus, so a retracted row never renders as blocked', () => {
    // Without the short-circuit a dropped task holding an unfinished prerequisite lands in
    // BLOCKED — the one lane that means the opposite of retired.
    const t = task({ id: 'd', status: 'dropped', blocked_by: ['missing'] });
    const all = [t, task({ id: 'missing', status: 'active' })];
    expect(effectiveStatus(all[0], all)).toBe('dropped');
  });

  it('is a lane but not a step in the flow', () => {
    expect(TASK_STATUSES).toContain('dropped');
    expect(FLOW_STATUSES).not.toContain('dropped');
    // One click past DONE must not mean "this should never have existed".
    expect(FLOW_STATUSES[FLOW_STATUSES.length - 1]).toBe('done');
  });

  it('maps the spellings runtimes actually use for a retraction', () => {
    for (const s of ['dropped', 'cancelled', 'canceled', 'abandoned']) {
      expect(statusFromAgent(s)).toBe('dropped');
      expect(coerceStatus(s)).toBe('dropped');
    }
  });

  it('will not be resurrected by a tab restating its title', () => {
    // Same guard `done` has, and it matters more here: reclaiming a dropped row puts back
    // exactly the work somebody decided against.
    const list = [task({ id: 'x', title: 'add auth guard', tab_id: null, status: 'dropped' })];
    expect(findDuplicate(list, 'add auth guard', 'tab-1')).toBeUndefined();
    expect(findImportedDuplicate(list, 'add auth guard', 'tab-1')).toBeUndefined();
  });

  it('is not swept up by the parked exemptions', () => {
    expect(isDropped('backlog')).toBe(false);
    expect(isDropped('dropped')).toBe(true);
  });
});

describe('blockers are legible, not raw ids', () => {
  it('names each blocker and says whether it is actually holding things up', () => {
    const all = [
      task({ id: 'a', title: 'migrate schema', status: 'done' }),
      task({ id: 'b', title: 'ship the API', status: 'active' }),
      task({ id: 'c', title: 'backfill', blocked_by: ['a', 'b'] }),
    ];
    expect(resolveBlockers(all[2], all)).toEqual([
      { id: 'a', title: 'migrate schema', status: 'done', state: 'met' },
      { id: 'b', title: 'ship the API', status: 'active', state: 'waiting' },
    ]);
  });

  it('names a parked prerequisite and the archived tab holding it', () => {
    // `Tab.archived_tasks` is off every list the tools read, so this arrived as a bare id
    // resolving to nothing — an agent told it was waiting on something invisible. The tab
    // id is what makes it actionable: restoreArchivedTab takes one.
    const t = task({ id: 'c', blocked_by: ['parked'] });
    const [b] = resolveBlockers(t, [t], new Set(['parked']), () => ({
      title: 'migrate schema',
      status: 'active',
      tab_id: 'tab-db',
      tab_name: 'db-work',
    }));
    expect(b).toEqual({
      id: 'parked',
      title: 'migrate schema',
      status: 'active',
      state: 'parked',
      parked_with: { tab_id: 'tab-db', tab_name: 'db-work' },
    });
  });

  it('tells a deleted prerequisite from one parked with an archived tab', () => {
    const t = task({ id: 'c', blocked_by: ['gone', 'parked'] });
    const resolved = resolveBlockers(t, [t], new Set(['parked']));
    expect(resolved.map((r) => r.state)).toEqual(['gone', 'parked']);
    // Same line hasUnmetDeps draws: gone does not block, parked does.
    expect(hasUnmetDeps(t, [t], new Set(['parked']))).toBe(true);
    expect(hasUnmetDeps(task({ id: 'c', blocked_by: ['gone'] }), [t])).toBe(false);
  });

  it('reports the reverse edge — who was waiting on this one', () => {
    const done = task({ id: 'a', title: 'migrate schema' });
    const all = [done, task({ id: 'b', blocked_by: ['a'] }), task({ id: 'c', blocked_by: ['a', 'b'] }), task({ id: 'd' })];
    expect(blocking(done, all).map((t) => t.id)).toEqual(['b', 'c']);
    expect(blocking(all[3], all)).toEqual([]);
  });

  it('never reports a task as blocking itself', () => {
    const self = task({ id: 'a', blocked_by: ['a'] });
    expect(blocking(self, [self])).toEqual([]);
  });
});

describe('the progress log is append-only and bounded', () => {
  it('appends oldest-first without touching the spec', () => {
    const t = task({ title: 'migrate schema', detail: 'the spec' });
    const notes = appendNote(t, 'waiting on the staging dump', 'agent', '2026-09-10T10:00:00Z');
    expect(notes).toEqual([{ at: '2026-09-10T10:00:00Z', text: 'waiting on the staging dump', by: 'agent' }]);
    // The whole reason this is not a `detail` rewrite.
    expect(t.detail).toBe('the spec');
  });

  it('never mutates the row it appends to', () => {
    // Every writer here commits WHOLE lists; a vector mutated in place on a $state row
    // persists from one surface while another still holds the pre-append copy.
    const t = task({ notes: [{ at: 'a', text: 'one', by: 'human' }] });
    appendNote(t, 'two', 'agent');
    expect(t.notes).toHaveLength(1);
  });

  it('keeps the newest CAP entries, dropping from the front', () => {
    let t = task();
    for (let i = 0; i < TASK_NOTE_CAP + 5; i++) t = { ...t, notes: appendNote(t, `note ${i}`, 'agent') };
    expect(t.notes).toHaveLength(TASK_NOTE_CAP);
    expect(t.notes![0].text).toBe('note 5');
    expect(t.notes![TASK_NOTE_CAP - 1].text).toBe(`note ${TASK_NOTE_CAP + 4}`);
  });

  it('starts every task with an empty log rather than an absent one', () => {
    expect(makeTask({ title: 'x' }).notes).toEqual([]);
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
