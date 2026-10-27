import { describe, expect, it } from 'vitest';
import {
  adoptInstalled, charCount, checkpointAsks, checkpointFor, coordinatorHolds, dispatchHiveOpenColumn, engineSkips, folderName, HIVE_HUMAN_KIND,
  HIVE_PROMPT_KINDS, hiveOwnsPromptIn, isEligible, MAX_INSTRUCTIONS, openTargetFor, pausedLabel, slugify, throttleTip, boardGone, takePendingHiveOpen, validateAbout, validateCap, validateFolders, validateSpec, withCoordinatorText,
} from './model';

describe('boardGone', () => {
  it('a fetch racing the delete is not an error; anything else on a live Hive is', () => {
    expect(boardGone("Error: That workspace isn't a Hive", true)).toBe(true);
    expect(boardGone('Workspace not found', true)).toBe(true);
    expect(boardGone('No Hive folder', false)).toBe(true);
    expect(boardGone('No Hive folder', true)).toBe(false);
  });
});

describe('throttleTip', () => {
  it('says what the throttle holds back, and that a held worker launches on its own', () => {
    expect(throttleTip(false)).toMatch(/no worker or queued run starts until it eases/);
    expect(throttleTip(true)).toMatch(/one at a time/);
    expect(throttleTip(false)).not.toMatch(/one at a time/);
    for (const c of [false, true]) expect(throttleTip(c)).toMatch(/launches on its own/);
  });
});

describe('pausedLabel', () => {
  it('says who or what paused the Hive, an unrecorded pause being a usage limit', () => {
    expect(pausedLabel('human')).toBe('Paused by you: no new workers start');
    expect(pausedLabel('account')).toMatch(/Claude account/);
    expect(pausedLabel('usage_limit')).toMatch(/usage limit/);
    expect(pausedLabel(undefined)).toBe(pausedLabel('usage_limit'));
  });
});

const ws = { id: 'w' };

describe('openTargetFor', () => {
  it('a Hive agent opens its column, never the tab', () => {
    expect(openTargetFor({ id: 'c', hive_role: 'coordinator' }, ws)).toEqual({ kind: 'hive', workspaceId: 'w', tabId: 'c' });
    expect(openTargetFor({ id: 'k', hive_role: 'worker' }, ws).kind).toBe('hive');
  });

  it('a service opens its console', () => {
    expect(openTargetFor({ id: 's', service_id: 'svc' }, ws)).toEqual({ kind: 'service', workspaceId: 'w', serviceId: 'svc' });
  });

  it('an ordinary tab is switched to', () => {
    expect(openTargetFor({ id: 't', service_id: null, hive_role: null }, ws)).toEqual({ kind: 'tab', workspaceId: 'w', tabId: 't' });
  });
});

describe('engineSkips', () => {
  it('skips a Hive agent, though nothing marked it exempt', () => {
    expect(engineSkips({ id: 'c', hive_role: 'coordinator' }, ws)).toBe(true);
    expect(engineSkips({ id: 'k', hive_role: 'worker' }, ws)).toBe(true);
  });

  it('keeps the human exemption, by tab or workspace', () => {
    expect(engineSkips({ id: 't', overlord_exempt: true }, ws)).toBe(true);
    expect(engineSkips({ id: 't' }, { id: 'w', overlord_exempt: true })).toBe(true);
  });

  it('supervises everything else, a service included', () => {
    expect(engineSkips({ id: 't' }, ws)).toBe(false);
    expect(engineSkips({ id: 's', service_id: 'svc', hive_role: null }, ws)).toBe(false);
  });
});

describe('slugify', () => {
  it('makes a branch-safe slug, like Rust', () => {
    expect(slugify('Ship it!')).toBe('ship-it');
    expect(slugify('  --Session  store: reads--  ')).toBe('session-store-reads');
    expect(slugify('Café Ünïcode')).toBe('caf-n-code');
    expect(slugify('!!!')).toBe('');
    expect(slugify('a-'.repeat(40))).toHaveLength(59);
  });
});

describe('validateSpec', () => {
  const ok = {
    name: 'Ship it',
    goal: 'Ship the thing',
    instructions: '',
    folders: [{ path: '/r/web', primary: true }],
    cap: 4,
  };
  const with_ = (patch: Partial<typeof ok>) => validateSpec({ ...ok, ...patch });

  it('accepts a complete spec', () => {
    expect(with_({})).toBeNull();
  });

  it('refuses long instructions by character, never cutting them', () => {
    expect(with_({ instructions: 'x'.repeat(MAX_INSTRUCTIONS) })).toBeNull();
    // An emoji is one character to Rust and two UTF-16 units to JS.
    expect(with_({ instructions: '😀'.repeat(MAX_INSTRUCTIONS) })).toBeNull();
    expect(with_({ instructions: 'x'.repeat(MAX_INSTRUCTIONS + 1) })).toMatch(/16001 characters.*nothing is cut/);
    expect(charCount('😀é')).toBe(2);
  });

  it('needs exactly one Primary', () => {
    expect(with_({ folders: [] })).toMatch(/Primary/);
    expect(with_({ folders: [{ path: '/a', primary: false }] })).toMatch(/Pick a Primary/);
    expect(with_({ folders: [{ path: '/a', primary: true }, { path: '/b', primary: true }] })).toMatch(/Only one/);
  });

  it('needs a name that slugs, and a goal', () => {
    expect(with_({ name: ' ' })).toBe('Name the Hive.');
    expect(with_({ name: '!!!' })).toMatch(/letter or digit/);
    expect(with_({ goal: '' })).toMatch(/goal/);
  });

  it('takes any cap from 1 to 64: no machine ceiling, memory pressure governs', () => {
    expect(with_({ cap: 0 })).toMatch(/1 to 64/);
    expect(with_({ cap: 65 })).toMatch(/1 to 64/);
    expect(with_({ cap: 64 })).toBeNull();
    expect(with_({ cap: 24 })).toBeNull();
  });

  it("each wizard step checks only its own part, and together they are validateSpec", () => {
    expect(validateAbout({ ...ok, goal: '' })).toMatch(/goal/);
    expect(validateAbout({ ...ok, folders: [] } as typeof ok)).toBeNull();
    expect(validateFolders({ folders: [] })).toMatch(/Primary/);
    expect(validateCap({ cap: 65 })).toMatch(/1 to 64/);
    expect(with_({ goal: '', folders: [], cap: 0 })).toBe(validateAbout({ ...ok, goal: '' }));
    expect(with_({ folders: [], cap: 0 })).toBe(validateFolders({ folders: [] }));
  });

  it('needs a Base for a git folder, as Rust does', () => {
    const git = (f: object) => validateSpec({ ...ok, folders: [{ path: '/r/web', primary: true, name: 'web', kind: 'git', ...f }] });
    expect(git({ base: '', detectedBase: null })).toMatch(/^web: pick a Base branch/);
    expect(git({ base: ' ', detectedBase: 'main' })).toBeNull();
    expect(git({ base: 'release', detectedBase: null })).toBeNull();
    expect(git({ kind: 'plain', base: '', detectedBase: null })).toBeNull();
  });
});

describe('folderName', () => {
  it('takes the last component, unique and never a reserved name', () => {
    expect(folderName('/r/web/', [])).toBe('web');
    expect(folderName('/r/web', ['web'])).toBe('web-2');
    expect(folderName('/x/coordinator', [])).toBe('coordinator-2');
    expect(folderName('/x/scratch', [])).toBe('scratch-2');
    expect(folderName('/x/cargo-target', [])).toBe('cargo-target-2');
    expect(folderName('/Users/x/Scratch', [])).toBe('Scratch-2');
    expect(folderName('/x/Claude-Tmp', [])).toBe('Claude-Tmp-2');
    expect(folderName('/r/Web', ['web'])).toBe('Web-2');
    expect(folderName('/r/.dots', [])).toBe('dots');
  });
});

describe('isEligible', () => {
  const shell = { id: 's', tab_type: 'terminal' };
  const ws = (tabs: Parameters<typeof isEligible>[0]['panes'][number]['tabs'], extra: object = {}) => ({ panes: [{ tabs }], ...extra });

  it('offers an empty workspace or one fresh shell', () => {
    expect(isEligible(ws([]), [])).toBe(true);
    expect(isEligible(ws([shell]), [])).toBe(true);
    expect(isEligible(ws([shell]), [{ tab_id: 's', status: 'done' }])).toBe(true);
  });

  it('never a used shell, two tabs, or a special workspace', () => {
    expect(isEligible(ws([shell, { id: 't' }]), [])).toBe(false);
    expect(isEligible(ws([{ ...shell, runtime: 'claude' }]), [])).toBe(false);
    expect(isEligible(ws([{ ...shell, auto_resume_command: 'claude --resume x' }]), [])).toBe(false);
    expect(isEligible(ws([{ ...shell, notes: 'mine' }]), [])).toBe(false);
    expect(isEligible(ws([{ ...shell, tab_type: 'editor' }]), [])).toBe(false);
    expect(isEligible(ws([shell]), [{ tab_id: 's', status: 'active' }])).toBe(false);
    expect(isEligible(ws([], { bridge_all: true }), [])).toBe(false);
    expect(isEligible(ws([], { overlord: true }), [])).toBe(false);
    expect(isEligible(ws([], { hive: { id: 'h' } }), [])).toBe(false);
    expect(isEligible(ws([], { suspended: true }), [])).toBe(false);
    expect(isEligible(ws([], { archived_tabs: [{ id: 'old' }] }), [])).toBe(false);
    expect(isEligible(ws([], { archived_tabs: [] }), [])).toBe(true);
  });
});

describe('takePendingHiveOpen', () => {
  it('hands a just-missed open to the view that mounts for it, once', () => {
    dispatchHiveOpenColumn({ workspaceId: 'w', tabId: 'c' });
    expect(takePendingHiveOpen('other')).toBeNull();
    expect(takePendingHiveOpen('w')).toEqual({ workspaceId: 'w', tabId: 'c' });
    expect(takePendingHiveOpen('w')).toBeNull();
    dispatchHiveOpenColumn({ workspaceId: 'w', tabId: 'c' });
    expect(takePendingHiveOpen('w', Date.now() + 5000)).toBeNull();
  });
});

describe('hiveOwnsPromptIn', () => {
  const hive = (over: Record<string, unknown> = {}) => ({ landing_mode: 'base', checkpoints: {} as Record<string, string>, status: 'active', ...over });
  const worker = { id: 'k', hive_role: 'worker' };
  const route = (kind: string, over: Record<string, unknown> = {}) => ({ tab_id: 'k', key: `${kind}-1`, kind, owner: 'coordinator', timed_out: false, taken_over: false, ...over });

  it("only a worker's notices, in a live Hive, under a coordinator checkpoint", () => {
    for (const tab of [{ id: 'c', hive_role: 'coordinator' }, { id: 't', hive_role: null }, null, undefined]) {
      for (const kind of HIVE_PROMPT_KINDS) expect(hiveOwnsPromptIn(hive(), tab, kind, [])).toBe(false);
    }
    for (const kind of HIVE_PROMPT_KINDS) {
      expect(hiveOwnsPromptIn(hive(), worker, kind, []), kind).toBe(true);
      expect(hiveOwnsPromptIn(hive({ status: 'done' }), worker, kind, []), `done: ${kind}`).toBe(false);
      expect(hiveOwnsPromptIn(null, worker, kind, [])).toBe(false);
    }
    expect(hiveOwnsPromptIn(hive(), worker, HIVE_HUMAN_KIND, []), "Rust's notice is never held back").toBe(false);
  });

  it('Ask me hands the kind back to the human; compaction and completion follow questions', () => {
    const asks = hive({ checkpoints: { questions: 'ask' } });
    for (const kind of ['question', 'compaction', 'completion']) expect(hiveOwnsPromptIn(asks, worker, kind, []), kind).toBe(false);
    expect(hiveOwnsPromptIn(asks, worker, 'permission', [])).toBe(true);
    expect(hiveOwnsPromptIn(hive({ checkpoints: { permissions: 'ask' } }), worker, 'permission', [])).toBe(false);
  });

  it('reads a route: held is owned; timed out, taken over or the human’s is not; unrouted reads owned', () => {
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission')])).toBe(true);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { timed_out: true })])).toBe(false);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { owner: 'human', taken_over: true })])).toBe(false);
    // Routed to the human outright: Rust's hive-prompt-notify was the notice, so the late one is held.
    expect(hiveOwnsPromptIn(hive(), worker, 'question', [route('question', { owner: 'human', why: 'outside the allowlist' })])).toBe(true);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { owner: 'human', why: 'this Hive has no coordinator to answer it' })])).toBe(true);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { owner: 'human', why: 'take_over', taken_over: true })])).toBe(false);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { owner: 'human', why: 'ask' })])).toBe(false);
    // Unrouted (Rust reads the screen about a second later): owned until Rust says otherwise.
    expect(hiveOwnsPromptIn(hive(), worker, 'question', [route('permission', { owner: 'human' })])).toBe(true);
    expect(hiveOwnsPromptIn(hive(), worker, 'permission', [route('permission', { tab_id: 'other', owner: 'human' })])).toBe(true);
  });

  it('a missing checkpoint key reads as its default for the landing mode', () => {
    expect(checkpointAsks({ landing_mode: 'base', checkpoints: {} }, 'landing')).toBe(false);
    expect(checkpointAsks({ landing_mode: 'integration', checkpoints: {} }, 'landing')).toBe(true);
    expect(checkpointAsks({ landing_mode: 'integration', checkpoints: { plan: 'ask' } }, 'plan')).toBe(true);
    expect(checkpointFor('trust')).toBeNull();
  });
});

describe('"With coordinator"', () => {
  it('is read-only only while the coordinator holds it alone', () => {
    const r = { owner: 'coordinator', timed_out: false, taken_over: false };
    expect(coordinatorHolds(r)).toBe(true);
    expect(coordinatorHolds({ ...r, timed_out: true })).toBe(false);
    expect(coordinatorHolds({ ...r, owner: 'human', taken_over: true })).toBe(false);
    expect(coordinatorHolds(null)).toBe(false);
  });

  it('says when the human is told', () => {
    expect(withCoordinatorText(10_000 + 4 * 60_000 + 1, 10_000)).toBe("With coordinator · you're told if it isn't answered in 5 min");
    expect(withCoordinatorText(10_000, 20_000)).toBe("With coordinator · you're told if it isn't answered in 1 min");
    expect(withCoordinatorText(0, 1)).toBe('With coordinator');
  });
});

describe('adoptInstalled', () => {
  it("mirrors the Create sheet's imported stack, so the next whole-list stack write keeps it", () => {
    const shell = { id: 'shell' };
    const other = { id: 'other' };
    const mirror = { name: 'Main', hive: null as unknown, stack: [] as { id: string }[], panes: [{ id: 'p', tabs: [shell, other], active_tab_id: 'shell' }] };
    const created = {
      name: 'Store', hive: { id: 'h' },
      stack: [{ id: 's1' }, { id: 's2' }],
      panes: [{ id: 'p', tabs: [other, { id: 'coord' }], active_tab_id: 'other' }],
    };
    adoptInstalled(mirror, created, 'coord', 'shell');
    expect(mirror.stack.map((s) => s.id)).toEqual(['s1', 's2']);
    expect(mirror.name).toBe('Store');
    expect(mirror.hive).toEqual({ id: 'h' });
    // The tabs still there are the mirror's own objects, left alone.
    expect(mirror.panes[0].tabs[0]).toBe(other);
    expect(mirror.panes[0].tabs.map((t) => t.id)).toEqual(['other', 'coord']);
    expect(mirror.panes[0].active_tab_id).toBe('other');
  });
});
