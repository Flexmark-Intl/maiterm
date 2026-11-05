/** The "What it can make" panel's data and logic (docs/hive-media.md §7), as pure functions.
 *
 *  Rust answers twice: `hive_media_status` (one row per capability, ids only) and
 *  `hive_media_catalogue` (each capability's and provider's display facts). `groupRows` joins the
 *  two. Every kind on the wire is a String, so each function here takes a kind it has never seen
 *  and says something neutral rather than throwing. Subscriber promotion ("I already pay for…")
 *  is Rust's (`already_pay_for`); nothing here reorders a row's recommendation. */

import type { HiveDetectedTool, HiveMediaCapability, HiveMediaCatalogue, HiveMediaProvider, HiveMediaRow } from '$lib/tauri/types';

// ─── Labels ─────────────────────────────────────────────────────────────────

const STATE_LABELS = new Map<string, string>([
  ['ready', 'Ready'],
  ['needs_sign_in', 'Needs sign-in'],
  ['unreachable', 'Not reachable'],
  ['installed', 'Installed'],
]);

/** A cover's state in maiTerm's words. A state a later Rust build adds is "Found": there, but
 *  nothing says it works. "Not reachable" names no cause: Rust maps a disabled server, one scoped
 *  away from the workspace, an upstream error and an upstream that is down to it, and Preferences
 *  (no workspace) judges no scope. */
export function stateLabel(state: string): string {
  return STATE_LABELS.get(state) ?? 'Found';
}

/** How a cover's state is drawn: "ok" (ready), "warn" (there but not usable yet: needs a sign-in,
 *  or not enabled for this workspace) or "neutral" (installed, or a state this build doesn't know). */
export function stateTone(state: string): 'ok' | 'warn' | 'neutral' {
  if (state === 'ready') return 'ok';
  if (state === 'needs_sign_in' || state === 'unreachable') return 'warn';
  return 'neutral';
}

/** What a job spends, in maiTerm's words (§2.3), never the provider's. `null` for a kind this
 *  build does not know (and for a provider with no route): the line is left out, not guessed. */
export function creditsLine(provider: Pick<HiveMediaProvider, 'name' | 'credits' | 'quote_tool'>): string | null {
  const quote = provider.quote_tool ? ' · prices a job before running it' : '';
  switch (provider.credits) {
    case 'api':
      return `Prepaid API credit${quote}`;
    case 'plan':
      return `Spends your ${provider.name} plan's credits${quote}`;
    // Two kinds the catalogue holds that §2.3 does not name: still money, so still said.
    case 'plan_or_wallet':
      return `Spends your ${provider.name} plan's credits or wallet balance${quote}`;
    case 'comfy_credits':
      return `Spends your ${provider.name} credits${quote}`;
    case 'free':
    case 'none':
      return 'Free';
    default:
      return null;
  }
}

/** The models line for a provider's card under `capability`: the provider's own line for that
 *  capability when the catalogue has one, else its general `models_note`. A fal card under Music
 *  must not list the video models. "" when there is neither. */
export function modelsLine(provider: Pick<HiveMediaProvider, 'models_note' | 'capability_models'>, capability: string): string {
  const own = provider.capability_models?.[capability]?.trim();
  return own || provider.models_note;
}

/** `mcp:fal-mirror` → `fal-mirror`, `cli:ffmpeg` → `ffmpeg`, `comfyui` → `comfyui`: the tool as
 *  its id names it, without the kind prefix. */
export function toolLabel(toolId: string): string {
  const i = toolId.indexOf(':');
  return i >= 0 && i < toolId.length - 1 ? toolId.slice(i + 1) : toolId;
}

/** The cover line's two names: the tool and, beside it, the provider it belongs to. When they are
 *  the same word ("fal" and "fal", "ideogram" and "Ideogram") it is said once, as the provider
 *  names it. `by` is null then. */
export function coverNames(cover: Pick<PanelCover, 'toolLabel' | 'providerName'>): { tool: string; by: string | null } {
  return cover.toolLabel.toLowerCase() === cover.providerName.toLowerCase() ? { tool: cover.providerName, by: null } : { tool: cover.toolLabel, by: cover.providerName };
}

/** Programs maiTerm detects only as plumbing for setting providers up. `cli:brew` installs the
 *  stdio providers' helpers (hive_media_status needs to see it); it is not something a worker makes
 *  media with, so the Tools step's picker, and with it the picks and the Review receipt, never list it. */
const SETUP_ONLY_TOOLS = new Set(['cli:brew']);

/** Whether a stored tool id is one of those programs: a default or draft saved while the picker
 *  still offered it holds it. It is neither a missing tool nor one to keep. */
export function isSetupOnly(id: string): boolean {
  return SETUP_ONLY_TOOLS.has(id);
}

/** `detected` without the setup-only programs: what the picker offers. Detection itself keeps them. */
export function pickerTools<T extends Pick<HiveDetectedTool, 'id'>>(detected: readonly T[]): T[] {
  return detected.filter((t) => !isSetupOnly(t.id));
}

// ─── Joining a status row with the catalogue ────────────────────────────────

/** A provider with its credits line worked out. Everything the catalogue knows, plus `creditsLine`. */
export type PanelProvider = HiveMediaProvider & { creditsLine: string | null };

/** One tool that covers a capability. */
export interface PanelCover {
  /** What `Hive.tools` stores for it. */
  toolId: string;
  /** `toolId` without its kind prefix. */
  toolLabel: string;
  providerId: string;
  /** The provider's name; its id for one the catalogue no longer names. */
  providerName: string;
  /** The raw state, for styling. */
  state: string;
  stateLabel: string;
}

export interface PanelRow {
  capability: string;
  label: string;
  means: string;
  common: boolean;
  covers: PanelCover[];
  /** Some cover is "ready". The only meaning "ready" has (the folded summary uses it too). */
  ready: boolean;
  /** For a capability nothing ready covers. Null when something does, or nothing can. */
  recommended: PanelProvider | null;
  alternatives: PanelProvider[];
}

export interface PanelGroups {
  common: PanelRow[];
  more: PanelRow[];
}

function panelProvider(p: HiveMediaProvider): PanelProvider {
  return { ...p, creditsLine: creditsLine(p) };
}

/** A catalogue capability the Rust build knows and this one does not: shown under "More", by its id. */
function unknownCapability(id: string): HiveMediaCapability {
  return { id, label: id, means: '', common: false };
}

/** Join each status row with the catalogue's display facts, in the catalogue's capability order
 *  (a row for a capability it doesn't list follows, in the order Rust sent it). The common five
 *  are the first page; everything else is "More". A provider id the catalogue doesn't hold can't
 *  be shown as a recommendation, so it is dropped there and named by its id on a cover. */
export function groupRows(rows: HiveMediaRow[], catalogue: HiveMediaCatalogue): PanelGroups {
  const providers = new Map(catalogue.providers.map((p) => [p.id, p]));
  const order = new Map(catalogue.capabilities.map((c, i) => [c.id, i]));
  const caps = new Map(catalogue.capabilities.map((c) => [c.id, c]));
  const showable = (ids: string[]): PanelProvider[] => ids.flatMap((id) => (providers.has(id) ? [panelProvider(providers.get(id)!)] : []));

  const built = rows.map((row, sent) => {
    const cap = caps.get(row.capability) ?? unknownCapability(row.capability);
    const covers: PanelCover[] = row.covered_by.map((c) => ({
      toolId: c.tool_id,
      toolLabel: toolLabel(c.tool_id),
      providerId: c.provider,
      providerName: providers.get(c.provider)?.name ?? c.provider,
      state: c.state,
      stateLabel: stateLabel(c.state),
    }));
    const recommended = row.recommended ? showable([row.recommended])[0] ?? null : null;
    const panel: PanelRow = {
      capability: cap.id,
      label: cap.label,
      means: cap.means,
      common: cap.common,
      covers,
      ready: covers.some((c) => c.state === 'ready'),
      recommended,
      alternatives: showable(row.alternatives),
    };
    return { panel, at: order.get(row.capability) ?? catalogue.capabilities.length + sent };
  });
  built.sort((a, b) => a.at - b.at);
  const all = built.map((b) => b.panel);
  return { common: all.filter((r) => r.common), more: all.filter((r) => !r.common) };
}

/** The folded panel's one line: "Media: video, image ready · voice, music not set up". The common
 *  five only; "ready" is a ready cover (`PanelRow.ready`). */
export function summaryLine(groups: PanelGroups): string {
  const names = (rows: PanelRow[]) => rows.map((r) => r.capability).join(', ');
  const ready = groups.common.filter((r) => r.ready);
  const missing = groups.common.filter((r) => !r.ready);
  const parts: string[] = [];
  if (ready.length) parts.push(`${names(ready)} ready`);
  if (missing.length) parts.push(`${names(missing)} not set up`);
  return parts.length ? `Media: ${parts.join(' · ')}` : 'Media: nothing to show';
}

// ─── The panel's small helpers ──────────────────────────────────────────────

/** What the recommended provider's card says when the provider is already on this row's covers but
 *  can't be used yet: a subscriber provider ("I already pay for…") is promoted even while its
 *  server can't be reached or is waiting for a sign-in, and a card that said "Recommended" then
 *  would read like something still to set up.
 *
 *  "unreachable" is Rust's one state for several causes (the server is disabled, scoped away from
 *  the workspace, in error, or its upstream is down; with no workspace, scope is never judged), so
 *  its words claim none of them. Null when the recommendation is not on the row, or its cover is in
 *  another state. With covers in both states, the sign-in wins: it is the one the human can act on. */
export function recommendedSetupNote(row: Pick<PanelRow, 'covers' | 'recommended'>): string | null {
  const id = row.recommended?.id;
  if (!id) return null;
  const states = row.covers.filter((c) => c.providerId === id).map((c) => c.state);
  if (states.includes('needs_sign_in')) return 'Set up, needs sign-in';
  if (states.includes('unreachable')) return 'Set up, but not reachable from here: check Preferences → MCP servers';
  return null;
}

/** What a capability's compact row says (the panel's open form): a covered row lists its covers
 *  (drawn with their state chips); an uncovered one says "Not set up" and, when there is a
 *  recommendation, "Suggested: <name>". `details` is whether a full card sits behind the row's
 *  disclosure: only for a row nothing ready covers that has a provider to set up. */
export interface CompactLine {
  covered: boolean;
  /** "Not set up" for an uncovered row, else "". */
  status: string;
  suggested: string | null;
  details: boolean;
  /** The disclosure button's words; "" when `details` is false. */
  detailsLabel: string;
}

export function compactLine(row: Pick<PanelRow, 'covers' | 'ready' | 'recommended'>): CompactLine {
  const covered = row.covers.length > 0;
  const details = !row.ready && row.recommended !== null;
  const name = row.recommended?.name ?? null;
  return {
    covered,
    status: covered ? '' : 'Not set up',
    suggested: !covered && name ? `Suggested: ${name}` : null,
    details,
    detailsLabel: !details ? '' : covered ? `Details: ${name}` : `How to set up ${name}`,
  };
}

/** The providers "I already pay for…" lists: the catalogue's `subscriber: true` ones, in its order. */
export function subscriberProviders(catalogue: HiveMediaCatalogue): HiveMediaProvider[] {
  return catalogue.providers.filter((p) => p.subscriber);
}

/** `ids` with `id` added, or removed when it is there. A new array, never a mutation. */
export function toggleId(ids: readonly string[], id: string): string[] {
  return ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id];
}

/** "unverified as of 2026-11-03" for a fact the catalogue has not confirmed from a real job. Given
 *  `capability`, a provider whose unverified claims are listed per capability (`unverified_capabilities`
 *  non-empty) is flagged only for those: fal's video models are confirmed though its music models are
 *  not, and Rust sets `unverified` for the provider whenever that list is non-empty. A provider with
 *  no such list is flagged for every capability when `unverified` (a host or its note). Null when
 *  confirmed. */
export function unverifiedNote(provider: Pick<HiveMediaProvider, 'unverified' | 'unverified_capabilities' | 'checked'>, capability?: string): string | null {
  const perCapability = capability !== undefined && provider.unverified_capabilities.length > 0;
  const flagged = perCapability ? provider.unverified_capabilities.includes(capability) : provider.unverified;
  if (!flagged) return null;
  return provider.checked ? `unverified as of ${provider.checked}` : 'unverified';
}

/** A catalogue link that is safe to hand to the system browser: http(s) only, else null. */
export function externalUrl(url: string): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.href : null;
  } catch {
    return null;
  }
}

/** Guards an async fetch against a newer one and against unmount. `next()` starts a request and
 *  returns its `stale()` check: true once a later `next()` or `cancel()` has happened, so a slow
 *  first answer can't overwrite a newer one and nothing is set after the component is gone. */
export function createLatest(): { next: () => () => boolean; cancel: () => void } {
  let seq = 0;
  return {
    next() {
      const mine = ++seq;
      return () => mine !== seq;
    },
    cancel() {
      seq++;
    },
  };
}

// ─── Whether the panel starts open ──────────────────────────────────────────

/** Words that say a goal makes media. Matched whole (so "imagery" is its own entry and "ad" never
 *  matches "add" or "advance"), case-insensitively, with a plural or "-ed"/"-ing" ending allowed.
 *  A list, not a judgment: a missing word costs one click on a folded line. */
const MEDIA_WORDS = [
  'video',
  'image',
  'imagery',
  'picture',
  'photo',
  'illustration',
  'artwork',
  'voice',
  'voice[ -]?over',
  'narration',
  'speech',
  'tts',
  'music',
  'soundtrack',
  'sound[ -]effect',
  'sfx',
  'audio',
  'podcast',
  'animation',
  'logo',
  '3d',
  'render',
  'thumbnail',
  'trailer',
  'movie',
  'film',
  'song',
  'gif',
  'poster',
  'banner',
  'reel',
  'clip',
  'dub(?:b(?:ed|ing))?',
  'animat(?:e|ed|es|ing)',
  'narrat(?:e|ed|es|ing)',
  'ad',
];

const MEDIA_RE = new RegExp(`(?:^|[^a-z0-9])(?:${MEDIA_WORDS.join('|')})(?:s|es|ed|ing)?(?![a-z0-9])`, 'i');

/** "re-render" is a UI-code word (a component draws again), not a render of media. Without this it
 *  would match "render" across the hyphen. */
const NOT_MEDIA_RE = /(?:^|[^a-z0-9])re-?render(?:s|ed|ing)?(?![a-z0-9])/gi;

/** Whether the goal names media by one of the list's words. Exported for tests and the sheet's hint. */
export function goalNamesMedia(goal: string): boolean {
  return MEDIA_RE.test(goal.replace(NOT_MEDIA_RE, ' '));
}

/** §7's obvious default, in code and not an AI judgment: the panel starts OPEN when the goal names
 *  media, when something asked for a capability up front (`mediaRequested`), or when no folder is
 *  a git repo (a plain folder is where assets go; with no folder at all there is no code to
 *  protect). Otherwise it starts folded to `summaryLine`, so a code Hive isn't slowed.
 *
 *  A code word beside a media word ("fix the video player bug" in a repo) opens it. Telling that
 *  apart from "make a video" would be a judgment, and opening costs one extra panel, folding costs
 *  a missed setup. */
export function shouldStartOpen(goal: string, folders: { kind: string }[], mediaRequested: string[] = []): boolean {
  if (mediaRequested.length > 0) return true;
  if (goalNamesMedia(goal)) return true;
  return !folders.some((f) => f.kind === 'git');
}

/** A proposal's `media` as the panel and the sheet keep it (`proposeHive`, docs/hive-media.md §8):
 *  the ids asked for, each once. An id the catalogue doesn't hold matches no row. */
export function requestedCapabilities(requested: readonly string[] | undefined): string[] {
  return [...new Set(requested ?? [])];
}

/** The rows of the panel's main list for a proposal's `media`: the common rows in catalogue order,
 *  then each asked-for row that lives under "More", in the order asked, so it is seen without
 *  opening More (which never opens by itself). `more` is what is left. Nothing asked: the groups
 *  as they are. */
export function placeRequested(groups: PanelGroups, requested: readonly string[]): { main: PanelRow[]; more: PanelRow[] } {
  const asked = requestedCapabilities(requested);
  const lifted = asked.flatMap((id) => groups.more.filter((r) => r.capability === id));
  return { main: [...groups.common, ...lifted], more: groups.more.filter((r) => !lifted.includes(r)) };
}

/** Capability ids as the human reads them in the panel: the catalogue's label, or the id itself for
 *  one the catalogue doesn't hold. */
export function capabilityLabels(ids: readonly string[], capabilities: readonly { id: string; label: string }[]): string[] {
  const label = new Map(capabilities.map((c) => [c.id, c.label]));
  return ids.map((id) => label.get(id) ?? id);
}

// ─── After a setup: what became ready, and the picks that follow ────────────

/** The tool ids `rows` call ready (a cover in state "ready"), each once. */
export function readyToolIds(rows: readonly HiveMediaRow[] | null): string[] {
  if (!rows) return [];
  return [...new Set(rows.flatMap((r) => r.covered_by.filter((c) => c.state === 'ready').map((c) => c.tool_id)))];
}

/** The tools ready in `after` that weren't in `before`: what a setup just made usable. */
export function newlyReady(before: readonly string[], after: readonly string[]): string[] {
  const had = new Set(before);
  return after.filter((id) => !had.has(id));
}

/** The Create sheet's picks after detection runs again because a setup changed this Mac
 *  (docs/hive-media.md §6). The picks still detected stay as they are, as at first detection
 *  (`knownTools`: a tool that is gone can't be sent). A tool the setup just made ready is ticked,
 *  as a media tool the human set up for this Hive, unless the human has ticked or unticked it
 *  themselves (`touched`): their choice is never overridden. Setup plumbing (`cli:brew`) and a
 *  ready tool detection doesn't list are never ticked. A new Set, never a mutation. */
export function picksAfterRedetect(
  picked: ReadonlySet<string>,
  detected: readonly Pick<HiveDetectedTool, 'id'>[],
  ready: readonly string[],
  touched: ReadonlySet<string>,
): Set<string> {
  const have = new Set(detected.map((t) => t.id));
  const next = new Set([...picked].filter((id) => have.has(id)));
  for (const id of ready) {
    if (have.has(id) && !touched.has(id) && !isSetupOnly(id)) next.add(id);
  }
  return next;
}

/** The ids whose tick a picker change flipped: what the human chose, for `picksAfterRedetect`. */
export function flippedIds(before: ReadonlySet<string>, after: ReadonlySet<string>): string[] {
  return [...[...before].filter((id) => !after.has(id)), ...[...after].filter((id) => !before.has(id))];
}

/** The Create sheet's detections and the picks each one leaves (docs/hive-media.md §6).
 *
 *  - Every setup notice's newly ready ids are KEPT until a detection's answer is applied: only the
 *    newest request's answer is applied, and a later notice with nothing new (a second Verify, a
 *    window refocus) must not drop an earlier one's tool.
 *  - The first answer that SUCCEEDS is the first answer: `first` (the defaults, or a draft's picks)
 *    decides its base even after a failed first detection, so a setup's re-detection never starts
 *    from the empty picks a failure left.
 *  Pure: the component owns sequencing (which answer is the newest) and unmount. */
export function createPickTracker(first: (detected: readonly HiveDetectedTool[]) => Set<string>) {
  let applied = false;
  const pending = new Set<string>();
  return {
    /** A setup newly made these ready: tick them at the next applied answer. */
    notice(ready: readonly string[]) {
      for (const id of ready) pending.add(id);
    },
    /** A detection answered and is the newest: the picks it leaves. Consumes the pending ids. */
    answered(picked: ReadonlySet<string>, detected: readonly HiveDetectedTool[], touched: ReadonlySet<string>): Set<string> {
      const base = applied ? picked : first(detected);
      applied = true;
      const next = picksAfterRedetect(base, detected, [...pending], touched);
      pending.clear();
      return next;
    },
    /** Some detection has succeeded. */
    get applied() {
      return applied;
    },
  };
}

/** What a setup sheet started (`onstarted`): its provider, and the install tab or the MCP server
 *  whose end the sheet can't outlive. */
export interface SetupStarted {
  provider_id: string;
  tab_id?: string | null;
  server_id?: string | null;
  /** The tools ready when the sheet was opened (the panel's `readyToolIds(rows)` then): the
   *  baseline that tells what the setup made ready from what was there already. */
  ready_before: string[];
}

/** The bookkeeping of a parent that outlives the media panel (the Create sheet, Hive settings):
 *  which install tabs and MCP servers its panel's sheets started, so it hears only THEIR end. An
 *  install that exits in another window, another panel's install, or a server that reconnects for
 *  any other reason is not this Hive's setup and is ignored. Each started id answers once: its end
 *  removes it. `readyFrom` names the tools a setup made ready, from a status fetch: the ready covers
 *  of the providers set up here, less the baseline taken when each was first started. Pure: the listeners are `lib/hive/mediaWatch.ts`. */
export function createSetupWatch() {
  const tabs = new Set<string>();
  const servers = new Set<string>();
  /** Each provider set up here, with the baseline of its FIRST start. */
  const baselines = new Map<string, Set<string>>();
  return {
    started(s: SetupStarted) {
      if (!baselines.has(s.provider_id)) baselines.set(s.provider_id, new Set(s.ready_before));
      if (s.tab_id) tabs.add(s.tab_id);
      if (s.server_id) servers.add(s.server_id);
    },
    /** `hive-media-install-finished`: true, once, for an install a sheet here started. */
    installEnded(e: { tab_id: string }): boolean {
      return tabs.delete(e.tab_id);
    },
    /** `mcp-proxy-status`: true, once, when a server a sheet here added or reused is connected. */
    serverStatus(e: { id: string; status?: { state?: string } | null }): boolean {
      return e.status?.state === 'connected' && servers.delete(e.id);
    },
    /** What the setups here made ready, from `hive_media_status` rows: the ready covers of the
     *  providers set up here that weren't ready when their first setup started. ffmpeg, already
     *  there before the Homebrew kit was set up for whisper-cli, is not among them. */
    readyFrom(rows: readonly HiveMediaRow[]): string[] {
      const ids = rows.flatMap((r) => r.covered_by.filter((c) => c.state === 'ready' && baselines.get(c.provider)?.has(c.tool_id) === false).map((c) => c.tool_id));
      return [...new Set(ids)];
    },
  };
}

export type SetupWatch = ReturnType<typeof createSetupWatch>;

/** The one rule for what a setup ticks (docs/hive-media.md §6), run by the parent that owns the
 *  watch (the Create sheet): read the status fresh, then `readyFrom` it. Whatever the panel showed or
 *  fetched in between (a Re-detect, an "I already pay for…" tick) and whether it is still mounted
 *  don't matter: the baseline is the setup's start, and the rows are this call's own. A failed read
 *  notices nothing (detection still runs, with nothing to tick). */
export async function setupTicks(watch: SetupWatch, fetchStatus: () => Promise<HiveMediaRow[]>): Promise<string[]> {
  try {
    return watch.readyFrom(await fetchStatus());
  } catch {
    return [];
  }
}
