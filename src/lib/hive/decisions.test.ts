import { describe, expect, it } from 'vitest';
import { canOverride, canPromote, categoryLabel, countsText, noteError, outcomeText, precedentTags, ruleError, ruleWording, whoText, whoTone } from './decisions';

const answered = { status: 'answered', by: 'coordinator', by_label: 'coordinator', recommended: null, can_override: true, overridden: false };

describe('decision log view', () => {
  it('names categories, a later build’s as it is', () => {
    expect(categoryLabel('questions')).toBe('Workers’ questions');
    expect(categoryLabel('never')).toBe('Always yours');
    expect(categoryLabel('config')).toBe('Config-changing land');
    expect(categoryLabel('budget')).toBe('budget');
  });

  it('says who decided, or where it stands', () => {
    expect(whoText({ ...answered, by_label: 'coordinator · by precedent' })).toBe('coordinator · by precedent');
    expect(whoText({ ...answered, by_label: 'human, after the coordinator’s timeout' })).toBe('human, after the coordinator’s timeout');
    expect(whoText({ status: 'recommended', by_label: '', recommended: 'Yes' })).toBe('waiting for you · the coordinator recommends “Yes”');
    expect(whoText({ status: 'lapsed', by_label: '', recommended: null })).toBe('closed unanswered');
    expect(whoText({ status: 'open', by_label: '', recommended: null })).toBe('waiting');
    expect(whoText({ status: 'digest', by_label: 'maiTerm', recommended: null })).toBe('sent to the coordinator’s chat');
    expect(categoryLabel('digest')).toBe('Daily digest');
    expect(whoTone({ status: 'answered', by: 'human' })).toBe('human');
    expect(whoTone({ status: 'answered', by: 'maiterm' })).toBe('maiterm');
    expect(whoTone({ status: 'open', by: '' })).toBe('open');
    expect(whoTone({ status: 'digest', by: 'maiterm' })).toBe('maiterm');
  });

  it('offers Override… only on the coordinator’s own answered decisions, once', () => {
    expect(canOverride(answered)).toBe(true);
    expect(canOverride({ ...answered, overridden: true })).toBe(false);
    expect(canOverride({ ...answered, by: 'human' })).toBe(false);
    expect(canOverride({ ...answered, can_override: false })).toBe(false);
    expect(canOverride({ ...answered, status: 'lapsed' })).toBe(false);
  });

  it('checks the note and the rule as Rust does', () => {
    expect(noteError('  ')).toMatch(/Say what should have happened/);
    expect(noteError('Not on prod')).toBeNull();
    expect(noteError('x'.repeat(501))).toMatch(/under 500/);
    expect(ruleError('When X, choose Y')).toBeNull();
    expect(ruleError('When X,\nchoose Y')).toMatch(/one line/);
    expect(ruleError(`a${String.fromCharCode(0x2028)}b`)).toMatch(/one line/);
    expect(ruleError('r'.repeat(401))).toMatch(/under 400/);
    expect(ruleError('')).toMatch(/needs words/);
  });

  it('reads outcomes as sentences', () => {
    expect(outcomeText('landed')).toBe('Landed');
    expect(outcomeText('held: main keeps moving')).toBe('Held: main keeps moving');
    expect(outcomeText(null)).toBe('');
    expect(outcomeText('hand_over')).toBe('Offers to let the coordinator decide this');
    expect(outcomeText('hand_back')).toBe('Offers to hand this back to you');
    expect(outcomeText('handed to the coordinator')).toBe('Handed to the coordinator');
  });
});

describe('precedents view', () => {
  const p = { scope: 'hive', status: 'active', hive_name: 'Ship it', promoted_to: null as string | null, rule_by: 'coordinator', cited: 1, overridden: 0 };

  it('labels whose wording the rule is', () => {
    expect(ruleWording(p)).toBe('the coordinator’s wording');
    expect(ruleWording({ rule_by: 'human' })).toBe('your wording');
  });

  it('tags global, made-global and retired ones, and promotes only active ones of this Hive', () => {
    expect(precedentTags(p, [])).toEqual([]);
    expect(precedentTags({ ...p, scope: 'global' }, [])).toEqual(['global, from Ship it']);
    expect(precedentTags({ ...p, promoted_to: 'P-2' }, ['P-2'])).toEqual(['made global']);
    expect(precedentTags({ ...p, promoted_to: 'P-2' }, [])).toEqual([]);
    expect(precedentTags({ ...p, status: 'retired' }, [])).toEqual(['retired']);
    expect(canPromote(p, [])).toBe(true);
    expect(canPromote({ ...p, promoted_to: 'P-2' }, ['P-2'])).toBe(false);
    expect(canPromote({ ...p, promoted_to: 'P-2' }, [])).toBe(true);
    expect(canPromote({ ...p, status: 'retired' }, [])).toBe(false);
    expect(canPromote({ ...p, scope: 'global' }, [])).toBe(false);
  });

  it('counts in words', () => {
    expect(countsText(p)).toBe('cited 1 time · overridden 0 times');
    expect(countsText({ cited: 3, overridden: 1 })).toBe('cited 3 times · overridden 1 time');
  });
});
