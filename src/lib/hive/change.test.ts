import { describe, expect, it } from 'vitest';
import { addHost, addSocket, formFor, MAX_CAP, toChange, validateForm } from './change';
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
    for (const home of ['lvh.me', '*.nip.io', 'app.localtest.me']) expect(addHost([], home), home).toHaveProperty('error');
  });

  it('takes only a database socket by full path, and sends what changed', () => {
    expect(addSocket([], ' /tmp/.s.PGSQL.5432 ')).toEqual({ sockets: ['/tmp/.s.PGSQL.5432'] });
    expect(addSocket([], '/opt/homebrew/var/mysql/mysql.sock')).toHaveProperty('sockets');
    expect(addSocket([], '/tmp/redis.sock')).toHaveProperty('sockets');
    for (const bad of ['/var/run/docker.sock', 'tmp/.s.PGSQL.5432', '/tmp/../x/.s.PGSQL.5432', '/tmp/.s.PGSQL.', '/Users/u/.maiterm/cm/h.sock']) {
      expect(addSocket([], bad), bad).toHaveProperty('error');
    }
    expect(addSocket(['/tmp/.s.PGSQL.5432'], '/tmp/.s.PGSQL.5432')).toHaveProperty('error');
    const f = formFor(hive());
    expect(toChange(hive(), { ...f, sockets: ['/tmp/.s.PGSQL.5432'] })).toEqual({ add_sockets: ['/tmp/.s.PGSQL.5432'] });
    const had = hive({ settings: { worker_sockets: ['/tmp/.s.PGSQL.5432'] } });
    expect(toChange(had, { ...formFor(had), sockets: [] })).toEqual({ remove_sockets: ['/tmp/.s.PGSQL.5432'] });
  });

  it('bounds the cap', () => {
    const f = formFor(hive());
    expect(validateForm(f)).toBeNull();
    expect(validateForm({ ...f, cap: 0 })).toMatch(/1 to/);
    expect(validateForm({ ...f, cap: MAX_CAP + 1 })).toMatch(/1 to/);
    expect(validateForm({ ...f, cap: 2.5 })).toMatch(/1 to/);
  });
});
