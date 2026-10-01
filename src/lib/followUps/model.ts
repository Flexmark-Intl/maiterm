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
  /** Event triggers (§5): a stack service by name or id, a task by id. */
  when_service_ready?: unknown;
  when_service_stopped?: unknown;
  when_task_done?: unknown;
  /** Deliver no later than this many minutes from now, or drop it. */
  expires_in_minutes?: unknown;
}

export type EventKind = 'service_ready' | 'service_stopped' | 'task_done';

const EVENT_ARGS: [keyof CreateArgs, EventKind][] = [
  ['when_service_ready', 'service_ready'],
  ['when_service_stopped', 'service_stopped'],
  ['when_task_done', 'task_done'],
];

/** What an event trigger's reference resolves to — the store looks it up, since only it can see
 *  the stack and the tasks. A refusal says why in words the agent can act on. */
export type ResolvedEvent =
  | { ok: true; workspace_id: string; service_id?: string; task_id?: string; label: string }
  | { ok: false; reason: string; detail: string };

export interface CreateContext {
  now: number;
  /** Follow-ups already pending on the tab. */
  pending: number;
  /** Follow-ups this tab created in the last hour. */
  createdLastHour: number;
  author: 'agent' | 'human' | 'maiterm';
  newId: () => string;
  /** Look up an event trigger's service or task. Absent: event triggers are refused. */
  resolveEvent?: (kind: EventKind, ref: string) => ResolvedEvent;
}

export type Resolved =
  | { ok: true; followUp: FollowUp }
  | { ok: false; reason: string; detail: string };

export function isEventKind(kind: string): kind is EventKind {
  return kind === 'service_ready' || kind === 'service_stopped' || kind === 'task_done';
}

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

  const hasAt = given(args.at);
  const hasIn = given(args.in_minutes);
  const events = EVENT_ARGS.filter(([arg]) => given(args[arg]));
  if (Number(hasAt) + Number(hasIn) + events.length !== 1) {
    return refuse(
      'need_one_trigger',
      'Pass exactly one trigger: `at` (an ISO 8601 time), `in_minutes`, `when_service_ready`, `when_service_stopped` or `when_task_done`.',
    );
  }

  if (events.length === 1) return resolveEventCreate(text, events[0], args, ctx);

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
  if (given(args.expires_in_minutes)) {
    const m = minutes(args.expires_in_minutes);
    if (m == null || m <= 0) return refuse('bad_expiry', '`expires_in_minutes` must be a positive number.');
    const exp = ctx.now + m * 60_000;
    if (exp < due) return refuse('bad_expiry', '`expires_in_minutes` would expire it before it is due.');
    expiresAt = new Date(exp).toISOString();
  }

  const limited = limits(ctx);
  if (limited) return limited;

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

/** The per-tab limits. Checked last, so a request that is wrong in itself says so rather than
 *  "come back later". */
function limits(ctx: CreateContext): Resolved | null {
  if (ctx.pending >= MAX_PENDING) {
    return refuse('too_many_pending', `This tab already has ${MAX_PENDING} follow-ups pending. Cancel one first (listFollowUps / cancelFollowUp).`);
  }
  if (ctx.createdLastHour >= MAX_CREATED_PER_HOUR) {
    return refuse('rate_limited', `This tab has created ${MAX_CREATED_PER_HOUR} follow-ups in the last hour.`);
  }
  return null;
}

/** An event trigger (§5). Waits for the event, at most 7 days: an event-triggered follow-up nobody
 *  has thought about for a week is not one to act on unannounced, so that is its default expiry. */
function resolveEventCreate(text: string, [arg, kind]: [keyof CreateArgs, EventKind], args: CreateArgs, ctx: CreateContext): Resolved {
  const ref = typeof args[arg] === 'string' ? (args[arg] as string).trim() : '';
  if (!ref) return refuse('bad_trigger', `\`${arg}\` must name a ${kind === 'task_done' ? 'task id' : 'service (name or id)'}.`);
  if (!ctx.resolveEvent) return refuse('bad_trigger', 'Event triggers are not available here.');
  const ev = ctx.resolveEvent(kind, ref);
  if (!ev.ok) return refuse(ev.reason, ev.detail);

  let expiresMs = MAX_DELAY_MS;
  if (given(args.expires_in_minutes)) {
    const m = minutes(args.expires_in_minutes);
    if (m == null || m <= 0) return refuse('bad_expiry', '`expires_in_minutes` must be a positive number.');
    if (m * 60_000 > MAX_DELAY_MS) return refuse('bad_expiry', 'An event follow-up can wait at most 7 days (`expires_in_minutes` ≤ 10080).');
    expiresMs = m * 60_000;
  }

  const limited = limits(ctx);
  if (limited) return limited;

  return {
    ok: true,
    followUp: {
      id: ctx.newId(),
      text,
      due: {
        kind,
        workspace_id: ev.workspace_id,
        service_id: ev.service_id ?? null,
        task_id: ev.task_id ?? null,
        label: ev.label.replace(CONTROL_CHARS, ''),
      },
      author: ctx.author,
      created_at: new Date(ctx.now).toISOString(),
      expires_at: new Date(ctx.now + expiresMs).toISOString(),
    },
  };
}

function given(v: unknown): boolean {
  return v != null && v !== '';
}

/** When a follow-up is due, in ms: a time one's `at`, an event one's `met_at` once its event has
 *  happened. Null while an event one waits — and for any kind this build doesn't know (a newer
 *  build wrote it). An unknown kind is never due here, never an error. */
export function dueAt(f: FollowUp): number | null {
  const stamp = f.due.kind === 'at' ? f.due.at : isEventKind(f.due.kind) ? f.due.met_at : null;
  if (!stamp) return null;
  const t = Date.parse(stamp);
  return Number.isFinite(t) ? t : null;
}

const UP = new Set(['starting', 'running', 'ready']);

/** Whether a stack status CHANGE meets an event follow-up of `kind`, and what to tell the agent
 *  (§5). Edges only: `ready` entered, or `stopped`/`crashed` entered from a running state — the
 *  store's `stopped` default for a service nothing has reported must never read as one stopping. */
export function serviceOutcome(
  kind: string,
  from: string,
  to: { status: string; lastExitCode: number | null; note: string | null },
): string | null {
  if (kind === 'service_ready') return to.status === 'ready' && from !== 'ready' ? 'it came up' : null;
  if (kind !== 'service_stopped' || !UP.has(from)) return null;
  if (to.status === 'crashed') return `it crashed${to.lastExitCode != null ? ` (exit ${to.lastExitCode})` : ''}`;
  if (to.status === 'stopped') return to.note ? `it stopped (${to.note})` : 'it stopped';
  return null;
}

/** Whether a task's state meets a `task_done` follow-up. Tasks are persisted, so this one IS a
 *  level: done or dropped, whenever it is seen. Dropped counts — the agent asked to hear when the
 *  task ended — and says so: it is the opposite of a dependency, where dropped satisfies nothing. */
export function taskOutcome(status: string | null): string | null {
  if (status === null) return 'the task was deleted';
  if (status === 'done') return 'it was done';
  if (status === 'dropped') return 'it was DROPPED, not done';
  return null;
}

/** Waiting on an event that hasn't happened yet (and isn't expired). */
export function isWaitingOnEvent(f: FollowUp): boolean {
  return isEventKind(f.due.kind) && !f.due.met_at;
}

/** The condition, for a human or the agent: "service `web` is ready". */
function conditionText(f: FollowUp, tense: 'wait' | 'past' | 'when'): string {
  const label = f.due.label ?? '?';
  const [what, wait, past, when] =
    f.due.kind === 'service_ready' ? [`service \`${label}\``, 'to be ready', 'was ready', 'is ready']
    : f.due.kind === 'service_stopped' ? [`service \`${label}\``, 'to stop', 'stopped', 'stops']
    : f.due.kind === 'task_done' ? [`task “${label}”`, 'to end', 'ended', 'ends']
    : [f.due.kind, '', '', ''];
  return `${what} ${tense === 'wait' ? wait : tense === 'past' ? past : when}`.trim();
}

/** An event follow-up's trigger, "when service `web` is ready"; null for a time one. */
export function triggerText(f: FollowUp): string | null {
  return isEventKind(f.due.kind) ? `when ${conditionText(f, 'when')}` : null;
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

/** Where a follow-up stands, for a human: "in 12m", "due 3m ago", "expired 2h ago", or — for a
 *  trigger kind this build can't time — "waiting". The UI's one phrasing, so the badge and the
 *  list can't disagree. */
export function whenText(f: FollowUp, now: number): string {
  if (isExpired(f, now)) return `expired ${durationText(now - Date.parse(f.expires_at!))} ago`;
  if (isWaitingOnEvent(f)) return `waiting for ${conditionText(f, 'wait')}`;
  const t = dueAt(f);
  if (t == null) return 'waiting';
  if (isEventKind(f.due.kind)) return `due: ${conditionText(f, 'past')} ${durationText(now - t)} ago`;
  return t > now ? `in ${durationText(t - now)}` : `due ${durationText(now - t)} ago`;
}

/** What a tab's clock badge says (docs/follow-ups.md §8), or null when there is nothing to
 *  show. `due`: one is past its time and waiting for the agent. `live`: the feature is on —
 *  off, they are held, and the badge says so rather than disappearing. */
export function badgeSummary(
  fus: FollowUp[],
  now: number,
  live: boolean,
): { count: number; due: boolean; tooltip: string } | null {
  if (fus.length === 0) return null;
  const active = fus.filter(f => !isExpired(f, now));
  const expired = fus.length - active.length;
  const next = [...active].sort((a, b) => (dueAt(a) ?? Infinity) - (dueAt(b) ?? Infinity))[0];
  const parts: string[] = [];
  if (next) {
    const text = next.text.length > 60 ? `${next.text.slice(0, 57)}…` : next.text;
    parts.push(`${active.length} follow-up${active.length === 1 ? '' : 's'} — next ${whenText(next, now)}: “${text}”`);
  }
  if (expired > 0) parts.push(`${expired} expired`);
  if (!live) parts.push('held: follow-ups are off (Preferences → Overlord)');
  parts.push('Right-click → Follow-ups… to manage.');
  return { count: active.length, due: live && active.some(f => isDue(f, now)), tooltip: parts.join(' · ') };
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
export function envelope(f: FollowUp, now: number, early = false): string {
  const created = Date.parse(f.created_at);
  const due = dueAt(f) ?? now;
  const late = now - due;
  const event = isEventKind(f.due.kind);
  const when = early
    ? event ? 'delivered before it happened, at your human’s request' : 'delivered early, at your human’s request'
    : late > LATE_AFTER_MS ? `delivered ${durationText(late)} late` : 'delivered on time';
  // An event one names its condition and what actually happened — "it crashed" and "the service
  // was removed" are both an end to "when `web` stops", and the agent needs to know which.
  const target = !event
    ? clockText(due, now)
    : early || !f.due.met_at
      ? `when ${conditionText(f, 'past')}`
      : `when ${conditionText(f, 'past')} — ${f.due.outcome ?? 'it happened'}, at ${clockText(due, now)}`;
  const scheduled = `at ${clockText(created, now)} for ${target} (${when})`;
  const who =
    f.author === 'human'
      ? `Your human scheduled this ${scheduled}:`
      : f.author === 'maiterm'
        ? `maiTerm scheduled this for you ${scheduled}:`
        : `You scheduled this ${scheduled} — your own earlier note, not a new message from your human:`;
  // The label is a service name or task title someone typed; strip the whole thing, not only the
  // text, before it reaches a bracketed paste (see CONTROL_CHARS).
  return `⟦FOLLOW-UP⟧ ${who}\n${f.text}`.replace(CONTROL_CHARS, '');
}
