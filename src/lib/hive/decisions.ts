/** Memory's Decisions and Precedents tabs (docs/hive.md §7.5, §7.6), pure: labels, who decided,
 *  what may be overridden or promoted, and the human's text checked as Rust checks it again.
 *  Rust decides everything (the fold, the counts, retirement). */

import type { HiveDecisionView, HivePrecedent } from '$lib/tauri/types';

/** Rust's limits, checked again there. */
export const MAX_RULE = 400;
export const MAX_NOTE = 500;

const CATEGORY: Record<string, string> = {
  plan: 'The plan',
  questions: 'Workers’ questions',
  permissions: 'Permission prompts',
  landing: 'Landing',
  finals: 'Gallery finals',
  interfaces: 'Dependencies and interfaces',
  never: 'Always yours',
  config: 'Config-changing land',
  integration: 'Integration land',
  other: 'Other',
  offer: 'Autonomy offer',
  digest: 'Daily digest',
};

/** An open offer's outcome is its kind until the human answers (Rust's `autonomy.rs`). */
const OFFER_KIND: Record<string, string> = {
  hand_over: 'Offers to let the queen decide this',
  hand_back: 'Offers to hand this back to you',
};

/** A Rust-assigned category in words; an unknown one (a later build's) as it is. */
export function categoryLabel(category: string): string {
  return CATEGORY[category] ?? category;
}

/** Who decided, or where it stands when nobody has. */
export function whoText(d: Pick<HiveDecisionView, 'status' | 'by_label' | 'recommended'>): string {
  switch (d.status) {
    case 'answered':
      return d.by_label || 'answered';
    case 'lapsed':
      return 'closed unanswered';
    case 'digest':
      return 'sent to the queen’s chat';
    case 'recommended':
      return d.recommended ? `waiting for you · the queen recommends “${d.recommended}”` : 'waiting for you';
    default:
      return 'waiting';
  }
}

/** The badge tone for who decided. */
export function whoTone(d: Pick<HiveDecisionView, 'status' | 'by'>): 'human' | 'coordinator' | 'maiterm' | 'open' {
  if (d.status === 'digest') return 'maiterm';
  if (d.status !== 'answered') return 'open';
  if (d.by === 'human') return 'human';
  if (d.by === 'coordinator') return 'coordinator';
  return 'maiterm';
}

/** Override… shows on the coordinator's own decisions, once (plan D17). */
export function canOverride(d: Pick<HiveDecisionView, 'can_override' | 'overridden' | 'by' | 'status'>): boolean {
  return d.can_override && !d.overridden && d.by === 'coordinator' && d.status === 'answered';
}

/** A land's or a decision's outcome, as a sentence start. */
export function outcomeText(outcome: string | null | undefined): string {
  const o = (outcome ?? '').trim();
  if (OFFER_KIND[o]) return OFFER_KIND[o];
  return o ? o.charAt(0).toUpperCase() + o.slice(1) : '';
}

/** The override note: null when Rust would take it. */
export function noteError(note: string): string | null {
  const n = note.trim();
  if (!n) return 'Say what should have happened: your note goes to the queen.';
  if ([...n].length > MAX_NOTE) return `Keep the note under ${MAX_NOTE} characters.`;
  return null;
}

/** Every line break Rust refuses in a rule: CR, LF and the Unicode line and paragraph separators. */
const LINE_BREAK = new RegExp(`[\r\n${String.fromCharCode(0x2028, 0x2029)}]`);

/** A rule as the human rewords it: one line, at most `MAX_RULE`. Null when Rust would take it. */
export function ruleError(rule: string): string | null {
  const r = rule.trim();
  if (!r) return 'A rule needs words: “When X, choose Y, because Z”.';
  if (LINE_BREAK.test(r)) return 'Keep the rule to one line.';
  if ([...r].length > MAX_RULE) return `Keep the rule under ${MAX_RULE} characters.`;
  return null;
}

/** Whose wording a rule is: the coordinator's until the human edits it. */
export function ruleWording(p: Pick<HivePrecedent, 'rule_by'>): string {
  return p.rule_by === 'human' ? 'your wording' : 'the queen’s wording';
}

/** The small tags beside a precedent. */
export function precedentTags(p: Pick<HivePrecedent, 'scope' | 'status' | 'hive_name' | 'promoted_to'>, globalIds: readonly string[]): string[] {
  const tags: string[] = [];
  if (p.scope === 'global') tags.push(p.hive_name ? `global, from ${p.hive_name}` : 'global');
  else if (p.promoted_to && globalIds.includes(p.promoted_to)) tags.push('made global');
  if (p.status === 'retired') tags.push('retired');
  return tags;
}

/** Make global: this Hive's active ones not already global. */
export function canPromote(p: Pick<HivePrecedent, 'scope' | 'status' | 'promoted_to'>, globalIds: readonly string[]): boolean {
  return p.scope !== 'global' && p.status === 'active' && !(p.promoted_to && globalIds.includes(p.promoted_to));
}

/** Counters, in words. */
export function countsText(p: Pick<HivePrecedent, 'cited' | 'overridden'>): string {
  const t = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  return `cited ${t(p.cited, 'time', 'times')} · overridden ${t(p.overridden, 'time', 'times')}`;
}
