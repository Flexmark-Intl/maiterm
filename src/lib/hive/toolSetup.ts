/** The window's side of a Hive's media ask (`tool_setup`, docs/hive-media.md §8): the coordinator's
 *  `hiveRequestCapability` files the card in Rust (`hive/tool_setup.rs`), the human approves it on
 *  the Loom or the phone, and Rust emits `hive-tool-setup-approved` to the Hive's window. That
 *  window opens Hive settings with the media panel's setup sheet for the provider, through the
 *  panel's own opening path (its baseline, its `onstarted`, the settings sheet's watch).
 *
 *  An opening waits for the window: it opens at once when the window has focus (a desktop
 *  approval always has: the click was in it), otherwise on the window's next focus. A window that
 *  wasn't listening (it was reloading when the event came) recovers the opening from the request
 *  list, where Rust keeps a settled request for an hour. Pure: the listeners are
 *  `stores/hiveToolSetup.svelte.ts`. */

import type { AgentRequest, HiveMediaCatalogue, HiveMediaInstallFinished, HiveMediaInstallStarted, HiveMediaRow } from '$lib/tauri/types';
import { groupRows, panelProvider, readyToolIds, type PanelProvider } from './media';

export const KIND = 'tool_setup';
/** Rust's `tool_setup::APPROVED_EVENT`. */
export const APPROVED_EVENT = 'hive-tool-setup-approved';
/** The window event a Hive view hears to take an opening for its workspace. */
export const HIVE_OPEN_TOOL_SETUP = 'maiterm:hive-open-tool-setup';

/** `hive-tool-setup-approved`'s payload (`tool_setup::approved_in`). */
export interface ToolSetupApproved {
  request_id: string;
  workspace_id: string;
  hive_id: string;
  capability: string;
  provider_id: string;
  by: 'desktop' | 'phone';
  /** Rust already started this install (a phone approval of a phone-startable setup): the sheet
   *  shows its tab, never a second install. */
  install: HiveMediaInstallStarted | null;
  waiting_for_mac: boolean;
}

/** What the Hive's settings sheet hands its media panel: which setup sheet to open at mount. */
export interface SetupSeed {
  capability: string;
  providerId: string;
  install: HiveMediaInstallStarted | null;
  /** That install's end, heard while the opening waited for the window. */
  finished: HiveMediaInstallFinished | null;
}

/** One opening waiting for its window. */
export interface ToolSetupOpening extends SetupSeed {
  requestId: string;
  workspaceId: string;
  /** When it was queued: the newest opens first. */
  at: number;
}

const str = (v: unknown): v is string => typeof v === 'string' && v.length > 0;

/** The payload, checked: an event from a build that sends something else is dropped, never half-opened. */
export function parseApproved(v: unknown): ToolSetupApproved | null {
  const p = v as Partial<ToolSetupApproved> | null;
  if (!p || typeof p !== 'object') return null;
  if (!str(p.request_id) || !str(p.workspace_id) || !str(p.capability) || !str(p.provider_id)) return null;
  const i = p.install as Partial<HiveMediaInstallStarted> | null | undefined;
  const install = i && typeof i === 'object' && str(i.tab_id) ? { tab_id: i.tab_id, command_shown: String(i.command_shown ?? ''), watches_exit: i.watches_exit === true } : null;
  return {
    request_id: p.request_id,
    workspace_id: p.workspace_id,
    hive_id: str(p.hive_id) ? p.hive_id : '',
    capability: p.capability,
    provider_id: p.provider_id,
    by: p.by === 'phone' ? 'phone' : 'desktop',
    install,
    waiting_for_mac: p.waiting_for_mac === true,
  };
}

export function openingFrom(p: ToolSetupApproved, at: number): ToolSetupOpening {
  return { requestId: p.request_id, workspaceId: p.workspace_id, capability: p.capability, providerId: p.provider_id, install: p.install, finished: null, at };
}

/** Rust keeps a settled request this long (`agent_requests::SETTLED_TTL_MS`): older ones aren't listed. */
const RECOVER_MS = 60 * 60 * 1000;

/** Approvals this window missed (it was reloading when the event came), from the request list: a
 *  `tool_setup` it filed, approved and settled done, in one of its workspaces, not yet opened. One
 *  whose install Rust started from the phone isn't recovered: its tab is in this window, the
 *  request doesn't name it, and a sheet without it would offer a second install. That setup goes on
 *  by itself (Rust adds the tools at Ready and says so), and Hive settings → Tools reaches its
 *  sheet. The outcome's words are Rust's (`tool_setup::approved_in`). */
export function missedOpenings(requests: readonly AgentRequest[], me: string, workspaceIds: ReadonlySet<string>, opened: ReadonlySet<string>, now: number): ToolSetupOpening[] {
  const out: ToolSetupOpening[] = [];
  for (const r of requests) {
    if (r.kind !== KIND || r.status !== 'done' || r.window_label !== me || opened.has(r.id)) continue;
    const at = r.decided_at ?? r.requested_at;
    if (now - at > RECOVER_MS) continue;
    const ws = r.args.workspace_id, cap = r.args.capability, prov = r.args.provider_id;
    if (!str(ws) || !str(cap) || !str(prov) || !workspaceIds.has(ws)) continue;
    const outcome = r.outcome ?? '';
    if (!outcome.startsWith('Approved') || outcome.includes('is installing in a tab')) continue;
    out.push({ requestId: r.id, workspaceId: ws, capability: cap, providerId: prov, install: null, finished: null, at });
  }
  return out;
}

/** How long the end of a taken opening's install is kept for the sheet that adopts it. */
const TAKEN_KEEP_MS = 10 * 60 * 1000;

/** The openings waiting in a window, one per workspace (a newer ask for the same Hive replaces the
 *  older: one sheet at a time). `next` is the newest, for the window's focus to open.
 *
 *  `spent` holds every request id this window is done with: opened (`take`), replaced by a newer
 *  one (`add`) or dropped (`drop`). Each opening gets one try, so recovery after a reload
 *  (`missedOpenings`) must never find any of them again; the store persists `spent`. An opening
 *  already spent is never queued again. */
export function createOpeningQueue(spent: Set<string> = new Set()) {
  const byWs = new Map<string, ToolSetupOpening>();
  /** The install tabs of taken openings: their end is kept here until the sheet adopts it. */
  const taken = new Map<string, { finished: HiveMediaInstallFinished | null; at: number }>();
  return {
    spent,
    /** Queued, unless already spent or older than the one waiting for its Hive. Whichever of the
     *  two loses is spent. */
    add(o: ToolSetupOpening) {
      if (spent.has(o.requestId)) return;
      const had = byWs.get(o.workspaceId);
      if (had?.requestId === o.requestId) return;
      if (had && had.at > o.at) {
        spent.add(o.requestId);
        return;
      }
      if (had) spent.add(had.requestId);
      byWs.set(o.workspaceId, o);
    },
    /** The newest waiting opening, left in place: the Hive view takes it. */
    next(): ToolSetupOpening | null {
      let best: ToolSetupOpening | null = null;
      for (const o of byWs.values()) if (!best || o.at > best.at) best = o;
      return best;
    },
    /** The opening waiting for a workspace, left in place. */
    peek(workspaceId: string): ToolSetupOpening | null {
      return byWs.get(workspaceId) ?? null;
    },
    /** Taken once, by the Hive view of its workspace: spent, and its install's end (if any) kept
     *  for the sheet (`endOf`). */
    take(workspaceId: string, now = Date.now()): ToolSetupOpening | null {
      const o = byWs.get(workspaceId) ?? null;
      byWs.delete(workspaceId);
      if (!o) return null;
      spent.add(o.requestId);
      for (const [tab, t] of taken) if (now - t.at > TAKEN_KEEP_MS) taken.delete(tab);
      if (o.install) taken.set(o.install.tab_id, { finished: o.finished, at: now });
      return o;
    },
    /** Given up on without opening (its Hive gone or suspended, or no view took it): spent. */
    drop(workspaceId: string) {
      const o = byWs.get(workspaceId);
      byWs.delete(workspaceId);
      if (o) spent.add(o.requestId);
    },
    /** Spent without ever being queued (an event this window ignored). */
    spend(requestId: string) {
      spent.add(requestId);
    },
    /** `hive-media-install-finished` for an install Rust started from the phone, heard before the
     *  sheet that shows it listens: held on the waiting opening, or for a taken one until its sheet
     *  asks (`endOf`). True when it was one of ours. */
    installEnded(f: HiveMediaInstallFinished): boolean {
      for (const o of byWs.values()) {
        if (o.install?.tab_id === f.tab_id) {
          o.finished = f;
          return true;
        }
      }
      const t = taken.get(f.tab_id);
      if (t) t.finished = f;
      return !!t;
    },
    /** The end of a taken opening's install, if it came, for the sheet that adopted it. */
    endOf(tabId: string, now = Date.now()): HiveMediaInstallFinished | null {
      const t = taken.get(tabId);
      return t && now - t.at <= TAKEN_KEEP_MS ? t.finished : null;
    },
    get size() {
      return byWs.size;
    },
  };
}

/** The sheet a seed opens, from the panel's first fetch: the catalogue's provider and the row's
 *  label (the sheet's heading). The capability needn't be missing any more: the sheet's Verify
 *  then says it works. An id this build's catalogue doesn't know is said, never guessed. */
export function seededSetup(
  rows: readonly HiveMediaRow[],
  catalogue: HiveMediaCatalogue,
  seed: Pick<SetupSeed, 'capability' | 'providerId'>,
): { provider: PanelProvider; capability: string; label: string; readyBefore: string[] } | { error: string } {
  const groups = groupRows([...rows], catalogue);
  const row = [...groups.common, ...groups.more].find((r) => r.capability === seed.capability);
  const known = [row?.recommended, ...(row?.alternatives ?? [])].find((p) => p?.id === seed.providerId);
  const raw = known ? null : catalogue.providers.find((p) => p.id === seed.providerId);
  // A provider the row no longer offers (something else became ready meanwhile) still opens.
  const provider: PanelProvider | null = known ?? (raw ? panelProvider(raw) : null);
  if (!provider) return { error: `The approved setup names a provider this maiTerm doesn’t know (${seed.providerId}).` };
  const label = row?.label ?? catalogue.capabilities.find((c) => c.id === seed.capability)?.label ?? seed.capability;
  return { provider, capability: seed.capability, label, readyBefore: readyToolIds(rows) };
}

/** What the card says after the human's Approve on the desktop: the sheet opens in this window. */
export const APPROVED_NOTE = 'Approved. Its setup sheet opens in the Hive: nothing is set up until you do it there. The queen will be told.';
