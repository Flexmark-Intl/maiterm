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
  /** A watch script (§5.1): `{ script, label?, every_seconds?, timeout_seconds? }`. */
  when_script?: unknown;
  /** Deliver no later than this many minutes from now, or drop it. */
  expires_in_minutes?: unknown;
}

/** §5.1 limits, mirrored in `watch.rs` (which clamps again — this is where the agent hears why). */
export const SCRIPT_MAX_BYTES = 16 * 1024;
export const SCRIPT_EVERY_MIN_S = 15;
export const SCRIPT_EVERY_MAX_S = 24 * 60 * 60;
export const SCRIPT_EVERY_DEFAULT_S = 60;
export const SCRIPT_TIMEOUT_MAX_S = 60;
export const SCRIPT_TIMEOUT_DEFAULT_S = 10;
const SCRIPT_LABEL_CHARS = 80;

/** Where a watch script would run, or why this tab can't have one — the store decides, since only
 *  it can see the tab (its folder, ssh, exemption). */
export type ResolvedScriptHome = { ok: true; cwd: string } | { ok: false; reason: string; detail: string };

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
  /** Where a watch script runs. Absent: watch scripts are refused. */
  resolveScriptHome?: () => ResolvedScriptHome;
}

export type Resolved =
  | { ok: true; followUp: FollowUp }
  | { ok: false; reason: string; detail: string };

export function isEventKind(kind: string): kind is EventKind {
  return kind === 'service_ready' || kind === 'service_stopped' || kind === 'task_done';
}

/** A follow-up that waits on a condition — an event or a watch script — and is due once `met_at`
 *  is set. Scripts are met in Rust (`watch.rs`); events by this window's store. */
export function isConditionKind(kind: string): boolean {
  return isEventKind(kind) || kind === 'script';
}

/** A watch script still waiting for the human to say it may run. */
export function needsApproval(f: FollowUp, unattended: boolean): boolean {
  return f.due.kind === 'script' && !f.due.met_at && !f.due.approved && !unattended;
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
  const hasScript = given(args.when_script);
  const events = EVENT_ARGS.filter(([arg]) => given(args[arg]));
  if (Number(hasAt) + Number(hasIn) + Number(hasScript) + events.length !== 1) {
    return refuse(
      'need_one_trigger',
      'Pass exactly one trigger: `at` (an ISO 8601 time), `in_minutes`, `when_script`, `when_service_ready`, `when_service_stopped` or `when_task_done`.',
    );
  }

  if (hasScript) return resolveScriptCreate(text, args, ctx);
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

  const expiresMs = conditionExpiry(args);
  if (typeof expiresMs !== 'number') return expiresMs;

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

/** How long a condition follow-up may wait, in ms: 7 days, or `expires_in_minutes` up to that. */
function conditionExpiry(args: CreateArgs): number | Resolved {
  if (!given(args.expires_in_minutes)) return MAX_DELAY_MS;
  const m = minutes(args.expires_in_minutes);
  if (m == null || m <= 0) return refuse('bad_expiry', '`expires_in_minutes` must be a positive number.');
  if (m * 60_000 > MAX_DELAY_MS) return refuse('bad_expiry', 'An event or script follow-up can wait at most 7 days (`expires_in_minutes` ≤ 10080).');
  return m * 60_000;
}

/** A whole number of seconds in [lo, hi], `dflt` when not given; null when given and not that. */
function seconds(v: unknown, lo: number, hi: number, dflt: number): number | null {
  if (!given(v)) return dflt;
  const n = minutes(v); // the same "a number, or a numeric string" reading
  return n != null && Number.isInteger(n) && n >= lo && n <= hi ? n : null;
}

/** Characters a script never needs and the approval card could be fooled by: control characters
 *  other than tab and newline (a CR can redraw a line over itself), and Unicode format characters
 *  — bidi overrides and isolates (U+202A–202E, U+2066–2069) reorder how a line LOOKS without
 *  changing what runs, and zero-width ones hide. Also the line and paragraph separators and every
 *  default-ignorable code point (variation selectors, Hangul fillers), which WebKit draws as
 *  nothing, and any space but U+0020: a no-break space LOOKS like one, but the shell reads it as
 *  part of a word, so `gh<NBSP>pr checks` shows as `gh pr checks` and runs a planted `gh<NBSP>pr`
 *  (re-review of bf90a19). Refused, never stripped: the human approves the bytes that run. */
const UNSHOWABLE = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}]|(?! )\p{Zs}/u;

/** A watch script (§5.1). Its text is kept EXACTLY as sent — it is code, and the human approves
 *  what will run — so anything the card couldn't show truthfully is refused, not cleaned. */
function resolveScriptCreate(text: string, args: CreateArgs, ctx: CreateContext): Resolved {
  const w = args.when_script;
  if (typeof w !== 'object' || w === null || Array.isArray(w)) {
    return refuse('bad_script', '`when_script` must be an object: { script, label?, every_seconds?, timeout_seconds? }.');
  }
  const o = w as Record<string, unknown>;
  const script = typeof o.script === 'string' ? o.script : '';
  if (!script.trim()) return refuse('bad_script', '`when_script.script` must be the script to run.');
  if (UNSHOWABLE.test(script)) {
    return refuse('bad_script', 'The script contains a control or invisible formatting character (a CR, an escape, a bidi or zero-width mark). Your human approves the script as shown, so write it in plain text — use \\r, \\033 and the like as escapes instead.');
  }
  if (new TextEncoder().encode(script).length > SCRIPT_MAX_BYTES) {
    return refuse('bad_script', `A watch script can be at most ${SCRIPT_MAX_BYTES / 1024} KB. Keep the logic in the script and the data in files.`);
  }
  const every = seconds(o.every_seconds, SCRIPT_EVERY_MIN_S, SCRIPT_EVERY_MAX_S, SCRIPT_EVERY_DEFAULT_S);
  if (every == null) return refuse('bad_script', `\`every_seconds\` must be a whole number from ${SCRIPT_EVERY_MIN_S} to ${SCRIPT_EVERY_MAX_S}.`);
  const timeout = seconds(o.timeout_seconds, 1, SCRIPT_TIMEOUT_MAX_S, SCRIPT_TIMEOUT_DEFAULT_S);
  if (timeout == null) return refuse('bad_script', `\`timeout_seconds\` must be a whole number from 1 to ${SCRIPT_TIMEOUT_MAX_S}. A watch script checks and exits — it never waits.`);
  const label = (typeof o.label === 'string' ? o.label : '').replace(CONTROL_CHARS, ' ').trim().slice(0, SCRIPT_LABEL_CHARS)
    || scriptLabel(script);

  if (!ctx.resolveScriptHome) return refuse('bad_trigger', 'Watch scripts are not available here.');
  const home = ctx.resolveScriptHome();
  if (!home.ok) return refuse(home.reason, home.detail);

  const expiresMs = conditionExpiry(args);
  if (typeof expiresMs !== 'number') return expiresMs;

  const limited = limits(ctx);
  if (limited) return limited;

  return {
    ok: true,
    followUp: {
      id: ctx.newId(),
      text,
      due: { kind: 'script', script, cwd: home.cwd, every_secs: every, timeout_secs: timeout, label },
      author: ctx.author,
      created_at: new Date(ctx.now).toISOString(),
      expires_at: new Date(ctx.now + expiresMs).toISOString(),
    },
  };
}

/** A label for a script the agent didn't name: its first line of code. */
function scriptLabel(script: string): string {
  const line = script.split('\n').map(l => l.trim()).find(l => l && !l.startsWith('#')) ?? 'watch script';
  const clean = line.replace(CONTROL_CHARS, ' ');
  return clean.length > 60 ? `${clean.slice(0, 57)}…` : clean;
}

function given(v: unknown): boolean {
  return v != null && v !== '';
}

/** When a follow-up is due, in ms: a time one's `at`, an event one's `met_at` once its event has
 *  happened. Null while an event one waits — and for any kind this build doesn't know (a newer
 *  build wrote it). An unknown kind is never due here, never an error. */
export function dueAt(f: FollowUp): number | null {
  const stamp = f.due.kind === 'at' ? f.due.at : isConditionKind(f.due.kind) ? f.due.met_at : null;
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
  // `starting` → `stopped` is a start that never ran or was called off ("not started — node is in
  // the foreground", "start cancelled") — not a running service stopping. It is what a reloaded
  // window's auto-start does to a service still running in its reattached tab (review of 8344b6f).
  if (to.status === 'stopped' && from !== 'starting') return to.note ? `it stopped (${to.note})` : 'it stopped';
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

/** Waiting on an event that hasn't happened yet (and isn't expired). Events only: a watch script
 *  waits too, but Rust checks it — nothing in this window's stack or tasks can meet one. */
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
    : f.due.kind === 'script' ? [`watch script “${label}”`, 'to pass', 'passed', 'passes']
    : [f.due.kind, '', '', ''];
  return `${what} ${tense === 'wait' ? wait : tense === 'past' ? past : when}`.trim();
}

/** A condition follow-up's trigger, "when service `web` is ready"; null for a time one. */
export function triggerText(f: FollowUp): string | null {
  return isConditionKind(f.due.kind) ? `when ${conditionText(f, 'when')}` : null;
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
  if (isConditionKind(f.due.kind) && !f.due.met_at) return `waiting for ${conditionText(f, 'wait')}`;
  const t = dueAt(f);
  if (t == null) return 'waiting';
  if (isConditionKind(f.due.kind)) return `due: ${conditionText(f, 'past')} ${durationText(now - t)} ago`;
  return t > now ? `in ${durationText(t - now)}` : `due ${durationText(now - t)} ago`;
}

/** What a tab's clock badge says (docs/follow-ups.md §8), or null when there is nothing to
 *  show. `due`: one is past its time and waiting for the agent. `live`: the feature is on —
 *  off, they are held, and the badge says so rather than disappearing. */
export function badgeSummary(
  fus: FollowUp[],
  now: number,
  live: boolean,
  unattended = false,
): { count: number; due: boolean; approval: boolean; tooltip: string } | null {
  if (fus.length === 0) return null;
  const active = fus.filter(f => !isExpired(f, now));
  const expired = fus.length - active.length;
  const asking = active.filter(f => needsApproval(f, unattended)).length;
  const next = [...active].sort((a, b) => (dueAt(a) ?? Infinity) - (dueAt(b) ?? Infinity))[0];
  const parts: string[] = [];
  if (asking > 0) parts.push(`${asking} watch script${asking === 1 ? '' : 's'} waiting for your approval to run`);
  if (next) {
    const text = next.text.length > 60 ? `${next.text.slice(0, 57)}…` : next.text;
    parts.push(`${active.length} follow-up${active.length === 1 ? '' : 's'} — next ${whenText(next, now)}: “${text}”`);
  }
  if (expired > 0) parts.push(`${expired} expired`);
  if (!live) parts.push('held: follow-ups are off (Preferences → Overlord)');
  parts.push(asking > 0 ? 'Click to review.' : 'Click, or right-click → Follow-ups…, to manage.');
  return {
    count: active.length,
    due: live && active.some(f => isDue(f, now)),
    approval: live && asking > 0,
    tooltip: parts.join(' · '),
  };
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
  const event = isConditionKind(f.due.kind);
  const script = f.due.kind === 'script';
  const when = early
    ? event ? 'delivered before it happened, at your human’s request' : 'delivered early, at your human’s request'
    : late > LATE_AFTER_MS ? `delivered ${durationText(late)} late` : 'delivered on time';
  // An event one names its condition and what actually happened — "it crashed" and "the service
  // was removed" are both an end to "when `web` stops", and the agent needs to know which. A
  // script's is "it passed" or "it BROKE instead", so it reads in the present: "when your watch
  // script … passes — it BROKE instead".
  const condition = script ? `your ${conditionText(f, 'when')}` : conditionText(f, 'past');
  const target = !event
    ? clockText(due, now)
    : early || !f.due.met_at
      ? `when ${condition}`
      : `when ${condition} — ${f.due.outcome ?? 'it happened'}, at ${clockText(due, now)}`;
  // What the script printed is the agent's answer to "what did it find" — after the note, so the
  // note still reads first.
  const report = script && f.due.report && !early ? `\n\nYour watch script printed:\n${f.due.report}` : '';
  const scheduled = `at ${clockText(created, now)} for ${target} (${when})`;
  const who =
    f.author === 'human'
      ? `Your human scheduled this ${scheduled}:`
      : f.author === 'maiterm'
        ? `maiTerm scheduled this for you ${scheduled}:`
        : `You scheduled this ${scheduled} — your own earlier note, not a new message from your human:`;
  // The label is a service name or task title someone typed; strip the whole thing, not only the
  // text, before it reaches a bracketed paste (see CONTROL_CHARS).
  return `⟦FOLLOW-UP⟧ ${who}\n${f.text}${report}`.replace(CONTROL_CHARS, '');
}
