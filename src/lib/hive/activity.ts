/** A Hive card's live activity line and diff stat (docs/hive.md §4.3, §4.4), as pure functions.
 *  The line reads only the hook state the frontend already keeps per tab (`toolName`,
 *  `toolDetail`, `idleSince`): no transcript, no new event. The phone's `toolVerb`
 *  (`loom/model.ts`) is a verb only and stays as it is; this is the verb plus its target. */

import type { AgentTabSession } from '$lib/stores/agentState.svelte';
import type { HiveDiffStat, HiveShortstat } from '$lib/tauri/types';

export interface Activity {
  text: string;
  tone: 'busy' | 'wait' | 'idle' | 'none';
}

type Session = Pick<AgentTabSession, 'state' | 'toolName' | 'toolDetail' | 'idleSince'> &
  Partial<Pick<AgentTabSession, 'staleTool' | 'staleToolDetail'>>;

/** The tool still running: the hook's own, or one the stale timer cleared with no PostToolUse
 *  yet. That timer fires 15 s in, and long builds and tests are most of what a worker runs. */
function inFlight(s: Session): { name: string; detail?: string } | undefined {
  if (s.toolName) return { name: s.toolName, detail: s.toolDetail };
  if (s.state === 'active' && s.staleTool) return { name: s.staleTool, detail: s.staleToolDetail };
  return undefined;
}

/** A tool's line as it ended. PostToolUse clears `toolName`, so between two tools the line would
 *  flicker to "Thinking" for a beat: the last one is held this long instead. */
export interface Sticky {
  text: string;
  at: number;
}
export const STICKY_MS = 4_000;

const EDIT_TOOLS = ['Edit', 'Write', 'MultiEdit', 'NotebookEdit', 'apply_patch'];
const SHELL_TOOLS = ['Bash', 'PowerShell', 'shell'];

/** The file's base name: the summarizer already cuts most to one, and this covers the rest. */
const base = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
const firstLine = (s: string) => s.split('\n').find((l) => l.trim())?.trim() ?? '';
const withTarget = (verb: string, target: string | undefined) => (target ? `${verb} ${target}` : verb);

/** The line for a tool in flight: "Editing store.ts", "Running npm test". */
export function toolLine(toolName: string, toolDetail?: string): string {
  const detail = toolDetail?.trim() || undefined;
  if (EDIT_TOOLS.includes(toolName)) return withTarget('Editing', detail && base(detail));
  if (toolName === 'Read') return withTarget('Reading', detail && base(detail));
  if (SHELL_TOOLS.includes(toolName)) return withTarget('Running', detail && firstLine(detail));
  if (toolName === 'Grep') return withTarget('Searching for', detail);
  if (toolName === 'Glob') return withTarget('Finding', detail);
  if (toolName === 'Agent' || toolName === 'Task') return detail ? `Delegating: ${detail}` : 'Delegating';
  // Matches cardState's sub-label, so a card never says it twice in two wordings.
  if (toolName === 'AskUserQuestion') return 'Asking a question';
  if (toolName.startsWith('mcp__')) {
    const name = toolName.split('__').slice(2).join('__');
    if (name === 'hiveReport') return 'Reporting';
    return `Using ${name || toolName}`;
  }
  return `Using ${toolName}`;
}

/** 2d, 4h, 12m; '' under a minute (or for a clock behind the stamp). */
function ago(ms: number): string {
  if (!Number.isFinite(ms) || ms < 60_000) return '';
  const d = Math.floor(ms / 86_400_000);
  if (d > 0) return `${d}d`;
  const h = Math.floor(ms / 3_600_000);
  return h > 0 ? `${h}h` : `${Math.floor(ms / 60_000)}m`;
}

/** What a worker's agent is doing right now (§4.4). `now` is the board's coarse clock and only
 *  ages the idle line; a sticky past its 4 s is also dropped by the store that holds it, so a
 *  stale `now` can't keep one on screen. */
export function activityLine(s: Session | undefined, now: number, sticky?: Sticky): Activity {
  if (!s) return { text: '—', tone: 'none' };
  if (s.state === 'permission') return { text: 'Waiting for permission', tone: 'wait' };
  if (s.state === 'idle') {
    const age = s.idleSince ? ago(now - s.idleSince) : '';
    return { text: age ? `Idle · ${age}` : 'Idle', tone: 'idle' };
  }
  const tool = inFlight(s);
  if (tool) return { text: toolLine(tool.name, tool.detail), tone: tool.name === 'AskUserQuestion' ? 'wait' : 'busy' };
  if (sticky && now - sticky.at < STICKY_MS) return { text: sticky.text, tone: 'busy' };
  return { text: 'Thinking', tone: 'busy' };
}

/** One step of the sticky bookkeeping for a tab: `last` is the line of the tool in flight when
 *  last seen. A tool ending (the session still active) hands its line on as the sticky; a turn
 *  ending or the session going away holds nothing. */
export function stickyStep(last: string | undefined, s: Session | undefined, now: number): { last: string | undefined; sticky: Sticky | null | undefined } {
  const tool = s && inFlight(s);
  if (s?.state === 'active' && tool) return { last: toolLine(tool.name, tool.detail), sticky: null };
  if (s?.state === 'active' && last) return { last: undefined, sticky: { text: last, at: now } };
  // `undefined`: leave whatever sticky there is to its own expiry (a re-read with no change).
  return { last: undefined, sticky: s?.state === 'active' ? undefined : null };
}

const fmtTok = (n: number) => (n >= 1_000_000 ? `${+(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`);

/** The card's context gauge (§4.3 "ctx 18% ▮"), toned as the chat column's is: high from 75%,
 *  warn from the Overlord's checkpoint threshold, so one number reads one colour everywhere.
 *  Null before the agent's first reply, when its meta has no figure. */
export function contextGauge(
  meta: { contextPct?: number; contextUsed?: number; contextLimit?: number } | null | undefined,
  warnAt: number | null,
): { pct: number; tone: 'ok' | 'warn' | 'high'; tip: string } | null {
  const p = meta?.contextPct;
  if (p == null || !Number.isFinite(p)) return null;
  const pct = Math.max(0, Math.min(100, Math.round(p)));
  const tone = pct >= 75 ? 'high' : pct >= (warnAt ?? 60) ? 'warn' : 'ok';
  const used = meta?.contextUsed != null && meta.contextLimit ? ` (${fmtTok(meta.contextUsed)} of ${fmtTok(meta.contextLimit)} tokens)` : '';
  return { pct, tone, tip: `Context ${pct}% used${used}` };
}

const fmt = (n: number) => n.toLocaleString('en-US');
const plural = (n: number, one: string, many = `${one}s`) => `${fmt(n)} ${n === 1 ? one : many}`;
const lines = (s: HiveShortstat) =>
  `${plural(s.files, 'file')}: ${plural(s.insertions, 'insertion')}, ${plural(s.deletions, 'deletion')}`;
const empty = (s: HiveShortstat) => !s.files && !s.insertions && !s.deletions;

/** The card's diff stat (§4.4): "+212 −40 · 3 new". `text` is '' when there's nothing to show
 *  (not measured yet, or no changes), and the tip carries the long form, committed included. */
export function formatDiffStat(d: HiveDiffStat | undefined): { text: string; tip: string } {
  if (!d) return { text: '', tip: '' };
  const { total, untracked, committed } = d;
  const parts: string[] = [];
  if (!empty(total)) parts.push(`+${fmt(total.insertions)} −${fmt(total.deletions)}`);
  if (untracked) parts.push(`${fmt(untracked)} new`);
  const tip: string[] = [];
  if (!empty(total)) tip.push(`Changed against its landing target: ${lines(total)}`);
  if (untracked) tip.push(`${plural(untracked, 'new file')} not yet added to git`);
  if (!empty(committed)) tip.push(`Committed (what a land would take): ${lines(committed)}`);
  if (d.error) {
    tip.push(`Not measured: ${d.error}`);
    parts.push(parts.length ? 'partial' : 'diff unavailable');
  }
  if (!tip.length) tip.push('No changes yet');
  return { text: parts.join(' · '), tip: tip.join('\n') };
}
