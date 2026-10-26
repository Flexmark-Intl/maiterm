import { describe, expect, it } from 'vitest';
import { addHost, formFor, MAX_CAP, toChange, validateForm } from './change';
import type { Hive } from '$lib/tauri/types';

const hive = (over: Partial<Hive> = {}) =>
  ({
    cap: 4,
    push_after_land: false,
    models: { coordinator: { runtime: 'claude', model: 'opus', effort: 'medium' }, worker: { runtime: 'claude', model: 'sonnet', effort: 'high' } },
    settings: { worker_hosts: ['docs.example.com'] },
    ...over,
  }) as Pick<Hive, 'cap' | 'models' | 'push_after_land' | 'settings'>;

describe('Workers and access', () => {
  it('sends nothing when nothing changed, and only what differs otherwise', () => {
    const f = formFor(hive());
    expect(toChange(hive(), f)).toBeNull();
    expect(toChange(hive(), { ...f, cap: 6, workerEffort: 'max', hosts: ['api.example.com'] })).toEqual({
      cap: 6,
      worker_effort: 'max',
      add_hosts: ['api.example.com'],
      remove_hosts: ['docs.example.com'],
    });
  });

  it('reads the phone choice from the record and sends it as workers_on_phone', () => {
    expect(formFor(hive()).phone).toBe('coordinator');
    expect(formFor(hive({ settings: { workers_on_phone: true } })).phone).toBe('all');
    expect(toChange(hive(), { ...formFor(hive()), phone: 'all' })).toEqual({ workers_on_phone: true });
  });

  it('keeps hosts normalised and refuses a repeat or a URL', () => {
    expect(addHost([], ' API.Example.com. ')).toEqual({ hosts: ['api.example.com'] });
    expect(addHost(['api.example.com'], 'api.example.com')).toHaveProperty('error');
    expect(addHost([], 'https://x.com/a')).toHaveProperty('error');
  });

  it('bounds the cap', () => {
    const f = formFor(hive());
    expect(validateForm(f)).toBeNull();
    expect(validateForm({ ...f, cap: 0 })).toMatch(/1 to/);
    expect(validateForm({ ...f, cap: MAX_CAP + 1 })).toMatch(/1 to/);
    expect(validateForm({ ...f, cap: 2.5 })).toMatch(/1 to/);
  });
});
