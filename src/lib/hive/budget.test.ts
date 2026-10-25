import { describe, expect, it } from 'vitest';
import { allHivesLabel, budgetLabel, budgetLevel, centsOf, formFor, newHosts, normalizeHost, toSave, usd, validateForm } from './budget';
import type { Hive, HiveSpend } from '$lib/tauri/types';

const spend = (spent: number, budget: number, all = spent, over = spent > 0 && spent >= budget): HiveSpend => ({
  spent_micros: spent,
  budget_micros: budget,
  all_hives_micros: all,
  over,
});

describe('dollars', () => {
  it('reads as Rust says them', () => {
    expect(usd(10_400_000)).toBe('$10.40');
    expect(usd(0)).toBe('$0.00');
    expect(usd(3_000)).toBe('$0.003');
    expect(usd(400)).toBe('$0.001');
    expect(usd(9_999)).toBe('$0.010');
    expect(usd(10_000)).toBe('$0.01');
    expect(usd(1_250_005_000)).toBe('$1,250.01');
    expect(usd(100_000_000_000)).toBe('$100,000.00');
    expect(usd(-2_500_000)).toBe('-$2.50');
  });
});

describe('the header', () => {
  it('turns amber at 80% and red once the budget is reached', () => {
    expect(budgetLevel(spend(3_200_000, 10_000_000))).toBe('ok');
    expect(budgetLevel(spend(7_999_999, 10_000_000))).toBe('ok');
    expect(budgetLevel(spend(8_000_000, 10_000_000))).toBe('amber');
    expect(budgetLevel(spend(10_000_000, 10_000_000))).toBe('over');
    expect(budgetLevel(spend(0, 0))).toBe('ok');
    expect(budgetLabel(spend(3_200_000, 10_000_000))).toBe('Budget: $3.20 of $10.00');
  });

  it('shows every Hive only when another Hive has spent', () => {
    expect(allHivesLabel(spend(3_200_000, 10_000_000))).toBeNull();
    expect(allHivesLabel(spend(3_200_000, 10_000_000, 5_100_000))).toBe('all Hives $5.10');
    expect(allHivesLabel(spend(0, 10_000_000, 0))).toBeNull();
  });
});

describe('the gallery settings sheet', () => {
  it('takes hosts as Rust does', () => {
    expect(normalizeHost('  Queue.FAL.run. ')).toEqual({ host: 'queue.fal.run' });
    expect(normalizeHost('*.fal.media')).toEqual({ host: '*.fal.media' });
    const refused = (h: string, says: string) => {
      const r = normalizeHost(h);
      expect('error' in r && r.error).toContain(says);
    };
    refused('', 'empty');
    refused('https://fal.run', 'bare host');
    refused('fal.run:443', 'bare host');
    refused('[::1]', 'bare host');
    refused('ai', 'needs a dot');
    refused('*.ai', 'needs a dot');
    refused('10.0.0.7', 'IP address');
    refused('printer.local', 'localhost');
    refused('api.localhost', 'localhost');
    refused('fal_run.com', "isn't a host name");
    refused('*.*.x.com', "isn't a host name");
  });

  it('reads the budget in dollars and saves cents', () => {
    expect(centsOf('25')).toBe(2500);
    expect(centsOf('$12.5')).toBe(1250);
    expect(centsOf('1,000.00')).toBe(100_000);
    expect(centsOf('12.345')).toBeNull();
    expect(centsOf('-3')).toBeNull();
    expect(centsOf('ten')).toBeNull();
    const f = formFor({ budget_cents: 1000, settings: { gallery: { output_folders: ['/o'], generator_hosts: ['fal.run'] } } } as Pick<Hive, 'budget_cents' | 'settings'>);
    expect(f).toEqual({ dollars: '10.00', outputFolders: ['/o'], hosts: ['fal.run'] });
    expect(validateForm(f)).toBeNull();
    expect(validateForm({ ...f, dollars: '100000.01' })).toContain('$100,000.00');
    expect(validateForm({ ...f, hosts: ['fal.run', 'localhost.localhost'] })).toContain('localhost');
    expect(validateForm({ ...f, hosts: Array.from({ length: 51 }, (_, i) => `h${i}.example.com`) })).toContain('At most 50');
    expect(validateForm({ ...f, outputFolders: Array.from({ length: 21 }, (_, i) => `/o${i}`) })).toContain('At most 20');
    expect(toSave({ ...f, dollars: '7.5', hosts: ['FAL.run', 'fal.run', ' api.kie.ai '] })).toEqual({ budget_cents: 750, output_folders: ['/o'], generator_hosts: ['fal.run', 'api.kie.ai'] });
  });

  it('offers only the hosts Re-detect found that the list lacks', () => {
    expect(newHosts(['fal.run', 'API.kie.ai'], ['fal.run', 'api.kie.ai', 'v3.fal.media'])).toEqual(['v3.fal.media']);
    expect(newHosts([], [])).toEqual([]);
  });
});
