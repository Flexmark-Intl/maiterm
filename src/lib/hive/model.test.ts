import { describe, expect, it } from 'vitest';
import {
  charCount, dispatchHiveOpenColumn, engineSkips, folderName, HIVE_PROMPT_KINDS, hiveOwnsPrompt, isEligible, MAX_INSTRUCTIONS,
  openTargetFor, slugify, takePendingHiveOpen, validateSpec,
} from './model';

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
    cap_confirmed: false,
  };
  const with_ = (patch: Partial<typeof ok>) => validateSpec({ ...ok, ...patch }, 5);

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

  it('holds a cap above the ceiling until confirmed', () => {
    expect(with_({ cap: 0 })).toMatch(/at least 1/);
    expect(with_({ cap: 6 })).toMatch(/ceiling of 5/);
    expect(with_({ cap: 6, cap_confirmed: true })).toBeNull();
    expect(with_({ cap: 5 })).toBeNull();
  });

  it('needs a Base for a git folder, as Rust does', () => {
    const git = (f: object) => validateSpec({ ...ok, folders: [{ path: '/r/web', primary: true, name: 'web', kind: 'git', ...f }] }, 5);
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

describe('hiveOwnsPrompt', () => {
  it('owns nothing before hiveAnswerWorker (step 7): every prompt reaches the human', () => {
    const tabs = [{ id: 'c', hive_role: 'coordinator' }, { id: 'k', hive_role: 'worker' }, { id: 't', hive_role: null }, null, undefined];
    for (const tab of tabs) for (const kind of HIVE_PROMPT_KINDS) expect(hiveOwnsPrompt(tab, kind)).toBe(false);
  });
});
