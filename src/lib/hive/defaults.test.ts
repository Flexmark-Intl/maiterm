import { describe, expect, it } from 'vitest';
import type { HiveDetectedTool } from '$lib/tauri/types';
import {
  BUILTIN, defaultsFromWizard, EMPTY_HIVE_DEFAULTS, groupTools, knownTools, normalizeHiveDefaults,
  summarizeTools, toggleGroup, toolMatches, wizardStart,
} from './defaults';

const tool = (id: string, kind: string, label: string, host: string | null = null): HiveDetectedTool => ({ id, kind, label, host });
const TOOLS = [
  tool('cli:ffmpeg', 'cli', 'ffmpeg'),
  tool('mcp:figma', 'mcp', 'Figma', 'figma.com'),
  tool('skill:frontend-design', 'skill', 'frontend-design'),
  tool('mcp:replicate', 'mcp', 'replicate'),
  tool('comfyui', 'comfyui', 'ComfyUI', '127.0.0.1:8188'),
  tool('odd:thing', 'zzz', 'Thing'),
];

describe('normalizeHiveDefaults', () => {
  it('fills every missing field', () => {
    expect(normalizeHiveDefaults(undefined)).toEqual(EMPTY_HIVE_DEFAULTS);
    expect(normalizeHiveDefaults({ cap: 6 })).toEqual({ ...EMPTY_HIVE_DEFAULTS, cap: 6 });
  });
});

describe('wizardStart', () => {
  it('empty defaults are the built-ins', () => {
    const s = wizardStart(EMPTY_HIVE_DEFAULTS);
    expect(s).toMatchObject({ ...BUILTIN, cap: null, rules: '', tools: [] });
  });
  it('takes what is set, and falls back on values this build does not know', () => {
    const s = wizardStart({
      ...EMPTY_HIVE_DEFAULTS,
      coordinator_model: 'sonnet', worker_effort: 'ludicrous', cap: 0, budget_dollars: 0,
      phone: 'all', push_after_land: true, rules: 'use pnpm', tools: ['a', 'a', 'b'],
    });
    expect(s.coordModel).toBe('sonnet');
    expect(s.workerEffort).toBe(BUILTIN.workerEffort);
    expect(s.cap).toBeNull();
    expect(s.budgetDollars).toBe(0);
    expect(s.phone).toBe('all');
    expect(s.pushAfterLand).toBe(true);
    expect(s.rules).toBe('use pnpm');
    expect(s.tools).toEqual(['a', 'b']);
  });
});

describe('knownTools', () => {
  it('drops ids not detected here, keeping order', () => {
    expect(knownTools(['mcp:figma', 'mcp:gone', 'cli:ffmpeg'], TOOLS)).toEqual(['mcp:figma', 'cli:ffmpeg']);
    expect(knownTools(['x'], [])).toEqual([]);
  });
});

describe('defaultsFromWizard', () => {
  it('writes picks and crew, keeps the rules default', () => {
    const d = defaultsFromWizard(
      { coordModel: 'opus', coordEffort: 'high', workerModel: 'haiku', workerEffort: 'low', cap: 7, budgetDollars: 12.4, pushAfterLand: true, phone: 'none', tools: ['b', 'a', 'b'] },
      { ...EMPTY_HIVE_DEFAULTS, rules: 'keep me' },
    );
    expect(d).toEqual({
      tools: ['b', 'a'], coordinator_model: 'opus', coordinator_effort: 'high', worker_model: 'haiku', worker_effort: 'low',
      cap: 7, budget_dollars: 12, push_after_land: true, phone: 'none', rules: 'keep me',
    });
  });
});

describe('groupTools', () => {
  it('groups in a fixed order, unknown kinds last, sorted by label', () => {
    const g = groupTools([...TOOLS, tool('mcp:airtable', 'mcp', 'airtable')]);
    expect(g.map((x) => x.label)).toEqual(['MCP servers', 'Skills', 'Programs', 'Local generators', 'zzz']);
    expect(g[0].tools.map((t) => t.label)).toEqual(['airtable', 'Figma', 'replicate']);
  });
  it('filters by every word, across label, host and id, and leaves out empty groups', () => {
    expect(groupTools(TOOLS, 'figma').map((x) => x.kind)).toEqual(['mcp']);
    expect(groupTools(TOOLS, '8188').map((x) => x.kind)).toEqual(['comfyui']);
    expect(groupTools(TOOLS, 'nothing-like-it')).toEqual([]);
    expect(toolMatches(TOOLS[1], 'FIG com')).toBe(true);
    expect(toolMatches(TOOLS[1], 'fig nope')).toBe(false);
  });
});

describe('toggleGroup', () => {
  it('picks all shown, or clears them when all are picked, leaving others alone', () => {
    const [mcp] = groupTools(TOOLS);
    const some = toggleGroup(new Set(['mcp:figma', 'cli:ffmpeg']), mcp);
    expect([...some].sort()).toEqual(['cli:ffmpeg', 'mcp:figma', 'mcp:replicate']);
    const none = toggleGroup(some, mcp);
    expect([...none]).toEqual(['cli:ffmpeg']);
  });
  it('touches only what the filter shows', () => {
    const [mcp] = groupTools(TOOLS, 'figma');
    expect([...toggleGroup(new Set(), mcp)]).toEqual(['mcp:figma']);
  });
});

describe('summarizeTools', () => {
  it('names three, then counts', () => {
    expect(summarizeTools(['a', 'b'])).toBe('a, b');
    expect(summarizeTools(['a', 'b', 'c'])).toBe('a, b, c');
    expect(summarizeTools(['ffmpeg', 'Figma', 'frontend-design', 'd', 'e', 'f', 'g'])).toBe('ffmpeg, Figma, frontend-design (+4 more)');
  });
});
