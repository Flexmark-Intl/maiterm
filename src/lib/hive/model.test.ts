import { describe, expect, it } from 'vitest';
import { engineSkips, openTargetFor } from './model';

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
