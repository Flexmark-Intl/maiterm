/**
 * Follow-ups — the pure half (docs/follow-ups.md). No Svelte, no Tauri: what a request resolves
 * to, when a follow-up is due, and what the agent reads when it lands. The store
 * (`stores/followUps.svelte.ts`) wraps these with state, persistence and the delivery tick.
 */
import type { FollowUp } from '$lib/tauri/types';

/** §4 limits. Each refusal names the one that tripped. */
export const MAX_PENDING = 10;
export const MIN_DELAY_MS = 60_000;
export const MAX_DELAY_MS = 7 * 24 * 60 * 60_000;
export const MAX_CREATED_PER_HOUR = 20;
export const MAX_TEXT_CHARS = 4000;
/** Delivered later than this after it was due, the envelope says by how much (§6.3). */
export const LATE_AFTER_MS = 5 * 60_000;

export interface CreateArgs {
  text?: unknown;
  /** RFC 3339 / ISO 8601. Exactly one of `at` or `in_minutes`. */
  at?: unknown;
  in_minutes?: unknown;
  /** Deliver no later than this many minutes from now, or drop it. */
  expires_in_minutes?: unknown;
}

export interface CreateContext {
  now: number;
  /** Follow-ups already pending on the tab. */
  pending: number;
  /** Follow-ups this tab created in the last hour. */
  createdLastHour: number;
  author: 'agent' | 'human' | 'maiterm';
  newId: () => string;
}

export type Resolved =
  | { ok: true; followUp: FollowUp }
  | { ok: false; reason: string; detail: string };

function refuse(reason: string, detail: string): Resolved {
  return { ok: false, reason, detail };
}

function minutes(v: unknown): number | null {
  const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : v;
  return typeof n === 'number' && Number.isFinite(n) ? n : null;
}

/** Control characters, except newline and tab, in both the C0 and the 8-bit C1 range. The text
 *  is typed into a terminal inside a bracketed paste: an ESC can end that paste early
 *  (`ESC[201~`) and turn the rest — a trailing `\r/clear`, say — into keystrokes, and an 8-bit
 *  CSI (0x9B) does the same where a terminal honours it. A prompt has no use for any of them. */
const CONTROL_CHARS = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g;

/** An ISO 8601 date-time WITH a zone. `Date.parse` accepts one without and reads it in THIS
 *  desktop's zone (and a date-only string as UTC midnight), so a remote agent on a UTC host
 *  asking for 15:00 would be served hours off. The schema says a zone is required; hold it. */
const ZONED_DATE_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})$/i;

/** Turn a create request into a follow-up, or say exactly why not. */
export function resolveCreate(args: CreateArgs, ctx: CreateContext): Resolved {
  const text = typeof args.text === 'string' ? args.text.replace(CONTROL_CHARS, '').trim() : '';
  if (!text) return refuse('missing_text', 'Give the follow-up some `text` — the prompt you want delivered back to you.');
  if (text.length > MAX_TEXT_CHARS) {
    return refuse('text_too_long', `Keep \`text\` under ${MAX_TEXT_CHARS} characters; it is a prompt, not a document.`);
  }

  const hasAt = args.at != null && args.at !== '';
  const hasIn = args.in_minutes != null && args.in_minutes !== '';
  if (hasAt === hasIn) {
    return refuse('need_one_trigger', 'Pass exactly one of `at` (an ISO 8601 time) or `in_minutes`.');
  }

  let due: number;
  if (hasAt) {
    const at = typeof args.at === 'string' ? args.at.trim() : '';
    due = ZONED_DATE_TIME.test(at) ? Date.parse(at) : NaN;
    if (!Number.isFinite(due)) {
      return refuse('bad_time', '`at` must be an ISO 8601 time with a zone, e.g. 2026-10-01T15:30:00-05:00 or …Z.');
    }
  } else {
    const m = minutes(args.in_minutes);
    if (m == null) return refuse('bad_time', '`in_minutes` must be a number.');
    due = ctx.now + m * 60_000;
  }

  const delay = due - ctx.now;
  if (delay < MIN_DELAY_MS) {
    return refuse(
      'too_soon',
      'A follow-up has to be at least 1 minute out. For anything sooner, keep working — or wait on the thing directly (waitForService).',
    );
  }
  if (delay > MAX_DELAY_MS) return refuse('too_far', 'A follow-up can be at most 7 days out.');

  let expiresAt: string | null = null;
  if (args.expires_in_minutes != null && args.expires_in_minutes !== '') {
    const m = minutes(args.expires_in_minutes);
    if (m == null || m <= 0) return refuse('bad_expiry', '`expires_in_minutes` must be a positive number.');
    const exp = ctx.now + m * 60_000;
    if (exp < due) return refuse('bad_expiry', '`expires_in_minutes` would expire it before it is due.');
    expiresAt = new Date(exp).toISOString();
  }

  // Checked last, so a request that is wrong in itself says so rather than "come back later".
  if (ctx.pending >= MAX_PENDING) {
    return refuse('too_many_pending', `This tab already has ${MAX_PENDING} follow-ups pending. Cancel one first (listFollowUps / cancelFollowUp).`);
  }
  if (ctx.createdLastHour >= MAX_CREATED_PER_HOUR) {
    return refuse('rate_limited', `This tab has created ${MAX_CREATED_PER_HOUR} follow-ups in the last hour.`);
  }

  return {
    ok: true,
    followUp: {
      id: ctx.newId(),
      text,
      due: { kind: 'at', at: new Date(due).toISOString() },
      author: ctx.author,
      created_at: new Date(ctx.now).toISOString(),
      expires_at: expiresAt,
    },
  };
}

/** When a time follow-up is due, in ms — null for any other kind, including one this build
 *  doesn't know (a newer build wrote it). An unknown kind is never due here, never an error. */
export function dueAt(f: FollowUp): number | null {
  if (f.due.kind !== 'at' || !f.due.at) return null;
  const t = Date.parse(f.due.at);
  return Number.isFinite(t) ? t : null;
}

export function isExpired(f: FollowUp, now: number): boolean {
  if (!f.expires_at) return false;
  const t = Date.parse(f.expires_at);
  return Number.isFinite(t) && t < now;
}

/** Due now and deliverable: past its time and not expired. */
export function isDue(f: FollowUp, now: number): boolean {
  const t = dueAt(f);
  return t != null && t <= now && !isExpired(f, now);
}

export type FollowUpStatus = 'pending' | 'due' | 'expired';

/** How a follow-up reads right now — for `listFollowUps` and the human's list. */
export function statusOf(f: FollowUp, now: number): FollowUpStatus {
  if (isExpired(f, now)) return 'expired';
  return isDue(f, now) ? 'due' : 'pending';
}

/** "3h 12m", "45m", "2d 3h" — the largest two units, never "0m". */
export function durationText(ms: number): string {
  const totalMin = Math.max(1, Math.round(ms / 60_000));
  const d = Math.floor(totalMin / 1440);
  const h = Math.floor((totalMin % 1440) / 60);
  const m = totalMin % 60;
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  return `${m}m`;
}

const pad = (n: number) => String(n).padStart(2, '0');

/** Local wall-clock time, with the date only when it isn't the same day as `now`. */
export function clockText(ms: number, now: number): string {
  const t = new Date(ms);
  const hm = `${pad(t.getHours())}:${pad(t.getMinutes())}`;
  const n = new Date(now);
  const sameDay = t.getFullYear() === n.getFullYear() && t.getMonth() === n.getMonth() && t.getDate() === n.getDate();
  return sameDay ? hm : `${t.getFullYear()}-${pad(t.getMonth() + 1)}-${pad(t.getDate())} ${hm}`;
}

/**
 * What the agent reads (§7). Framed, unlike an Overlord directive: its author is usually the
 * agent itself, possibly hours ago, about a situation that may have moved on — so it must not
 * read as the human having just typed it. The frame is also why a follow-up can't smuggle in a
 * slash command.
 */
export function envelope(f: FollowUp, now: number): string {
  const created = Date.parse(f.created_at);
  const due = dueAt(f) ?? now;
  const late = now - due;
  const when = late > LATE_AFTER_MS ? `delivered ${durationText(late)} late` : 'delivered on time';
  const scheduled = `at ${clockText(created, now)} for ${clockText(due, now)} (${when})`;
  const who =
    f.author === 'human'
      ? `Your human scheduled this ${scheduled}:`
      : f.author === 'maiterm'
        ? `maiTerm scheduled this for you ${scheduled}:`
        : `You scheduled this ${scheduled} — your own earlier note, not a new message from your human:`;
  return `⟦FOLLOW-UP⟧ ${who}\n${f.text}`;
}
