import { describe, expect, it } from 'vitest';
import type { AgentRequest } from '$lib/tauri/types';
import { isHiveRequest, landCard, unannouncedHiveCards, windowCarriesOut, windowTellsAsker } from './requests';

const req = (over: Partial<AgentRequest>): AgentRequest => ({
  id: 'r_1',
  tab_id: 'coord',
  window_label: 'main',
  kind: 'hive_land',
  args: {},
  summary: 'Land “x” into main in web',
  effects: [],
  reason: null,
  asker_title: 'coordinator',
  asker_workspace: 'Ship',
  requested_at: 0,
  expires_at: 1,
  status: 'pending',
  decided_at: null,
  decided_by: null,
  outcome: null,
  ...over,
});

describe('Hive requests', () => {
  it('a window never carries out a Hive request, nor tells its asker: Rust does both, once', () => {
    for (const kind of ['hive_land', 'hive_undo'] as const) {
      expect(isHiveRequest({ kind })).toBe(true);
      expect(windowCarriesOut({ kind, window_label: 'main' }, 'main')).toBe(false);
      expect(windowTellsAsker({ kind })).toBe(false);
    }
    expect(windowCarriesOut({ kind: 'tab_move', window_label: 'main' }, 'main')).toBe(true);
    expect(windowCarriesOut({ kind: 'tab_move', window_label: 'other' }, 'main')).toBe(false);
    expect(windowTellsAsker({ kind: 'restart' })).toBe(true);
  });

  it("a worker's column shows the card its land waits on, and only while it waits", () => {
    const pending = [req({ id: 'r_1' }), req({ id: 'r_2' })];
    expect(landCard(pending, { land: { id: 'L-1', state: 'awaiting_human', request: 'r_2', repos: [] } })?.id).toBe('r_2');
    expect(landCard(pending, { land: { id: 'L-1', state: 'locking', request: 'r_2', repos: [] } })).toBeNull();
    expect(landCard(pending, { land: { id: 'L-1', state: 'awaiting_human', request: 'r_9', repos: [] } })).toBeNull();
    expect(landCard(pending, { land: null })).toBeNull();
  });

  it('each Rust-filed card is announced once', () => {
    const seen = new Set<string>();
    const list = [req({ id: 'r_1' }), req({ id: 'r_2', kind: 'tab_move' }), req({ id: 'r_3', status: 'approved' })];
    expect(unannouncedHiveCards(list, seen).map((r) => r.id)).toEqual(['r_1']);
    expect(unannouncedHiveCards(list, seen)).toEqual([]);
  });
});
