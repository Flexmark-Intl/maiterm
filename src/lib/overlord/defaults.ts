import type { OverlordRule } from '$lib/tauri/types';

/**
 * App-provided default Overlord rules (docs/overlord.md §5, §7). Keyed by stable
 * default_id, same lifecycle contract as DEFAULT_TRIGGERS: seeded on startup, hideable,
 * auto-updated while un-modified, frozen once the user edits (`user_modified`).
 *
 * Rules are seeded ENABLED — the engine only runs when `overlord_enabled` is on, and
 * propose-mode (default) additionally holds every directive for a human click, so an
 * enabled seed can't type into anything by surprise.
 */
export const DEFAULT_OVERLORD_RULES: Record<string, Omit<OverlordRule, 'id' | 'enabled' | 'workspaces' | 'default_id'>> = {
  checkpoint_at_context_pressure: {
    name: 'Checkpoint before compaction',
    description:
      'At ~55% context, have the agent update docs/memory, prepare for compaction, then compact — instead of hitting the auto-compact wall mid-thought.',
    cooldown: 1800,
    when: { event: 'context_pct', at_or_above: 55 },
    guards: {
      agent_state: ['idle'],
      min_quiet_ms: 3000,
      require_live_repl: true,
      max_per_hour: 1,
      only_if_no_outstanding: true,
    },
    sequence: [
      {
        kind: 'process',
        text: 'Before we continue — make sure any relevant docs and memory are updated if needed.',
        await: { until: 'turn_end' },
        timeout_seconds: 900,
        on_timeout: 'abort',
      },
      {
        kind: 'process',
        text: 'Prepare for compaction.',
        await: { until: 'turn_end' },
        timeout_seconds: 600,
        on_timeout: 'abort',
      },
      {
        // /compact is Claude Code vocabulary; Codex compacts via /compact too in recent
        // builds but that is NOT verified here, and Gemini has no equivalent — so the
        // step declares claude-only and the engine skips+ledgers elsewhere.
        kind: 'slash',
        text: '/compact',
        runtimes: ['claude'],
        await: { until: 'context_below', pct: 30 },
        timeout_seconds: 300,
        on_timeout: 'notify_human',
      },
    ],
  },

  review_after_commit: {
    name: 'Review after commit',
    description:
      'After a commit lands, nudge the agent to have non-trivial work reviewed by a subagent before moving on.',
    cooldown: 3600,
    when: { event: 'commit' },
    guards: {
      agent_state: ['idle'],
      min_quiet_ms: 3000,
      require_live_repl: true,
      max_per_hour: 2,
      only_if_no_outstanding: true,
    },
    sequence: [
      {
        kind: 'process',
        text: 'If that commit was non-trivial, spin up a subagent to review it for correctness before moving on. If it was a trivial change, skip the review and carry on.',
        await: { until: 'turn_end' },
        timeout_seconds: 1800,
        on_timeout: 'continue',
      },
    ],
  },

  reinit_unbound_agent: {
    name: 'Re-bind a running agent',
    description:
      'A tab whose agent is running but not bound to maiTerm gets a /maiterm init, which restores tool routing and reply delivery. Only fires when the agent process is confirmed alive — a tab sitting at a shell is left alone.',
    cooldown: 900,
    when: { event: 'agent_unready' },
    guards: {
      // There is no live REPL binding by definition — that IS the condition. Requiring
      // one would make this rule unfireable, which is how it stayed advice-only.
      require_live_repl: false,
      min_quiet_ms: 3000,
      max_per_hour: 3,
      only_if_no_outstanding: true,
    },
    sequence: [
      {
        kind: 'slash',
        text: '/maiterm init',
        await: { until: 'turn_end' },
        timeout_seconds: 120,
        on_timeout: 'continue',
      },
    ],
  },

  todo_hygiene: {
    name: 'Keep a task list',
    description:
      'A tab doing sustained work with nothing on the maiTerm task list gets nudged to record it.',
    cooldown: 14400,
    when: { event: 'no_todo_list' },
    guards: {
      agent_state: ['idle'],
      min_quiet_ms: 3000,
      require_live_repl: true,
      max_per_hour: 1,
      only_if_no_outstanding: true,
    },
    sequence: [
      {
        kind: 'process',
        text: "If the current work is multi-step, track it: call createTasks with what you're actually working on, so nothing gets lost and your human can see it. If it's a one-off, ignore this.",
        await: { until: 'turn_end' },
        timeout_seconds: 600,
        on_timeout: 'continue',
      },
    ],
  },
};

/** Key-order-insensitive stringify — persisted rules round-trip through serde, whose
 *  field order need not match the template literals here. Order-sensitive comparison
 *  would re-report "changed" (and re-save preferences) on every launch. */
function stableStringify(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(stableStringify).join(',')}]`;
  if (v !== null && typeof v === 'object') {
    const entries = Object.entries(v as Record<string, unknown>)
      .filter(([, val]) => val !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : 1));
    return `{${entries.map(([k, val]) => `${JSON.stringify(k)}:${stableStringify(val)}`).join(',')}}`;
  }
  return JSON.stringify(v) ?? 'null';
}

/**
 * Seed default Overlord rules into an existing rule list. Same mechanics as
 * seedDefaultTriggers: removes stale defaults, auto-updates un-modified ones to the
 * latest template, seeds missing ones. Returns the updated list, or null if unchanged.
 *
 * NOTE: assumes ONE rule per default_id (`list.find`) — a workspace-scoped override of
 * a default must be its own rule with default_id null + supersedes (docs/overlord.md §6).
 */
export function seedDefaultOverlordRules(
  existing: OverlordRule[],
  hiddenIds: string[],
): OverlordRule[] | null {
  let list = [...existing];
  let changed = false;

  const before = list.length;
  list = list.filter(r => {
    if (!r.default_id) return true; // user-created
    return r.default_id in DEFAULT_OVERLORD_RULES; // stale default → remove
  });
  if (list.length !== before) changed = true;

  for (const [defaultId, tmpl] of Object.entries(DEFAULT_OVERLORD_RULES)) {
    if (hiddenIds.includes(defaultId)) continue;

    const linked = list.find(r => r.default_id === defaultId);
    if (linked) {
      // Auto-update un-modified defaults — but only when the template actually
      // differs. An unconditional rewrite makes this function report "changed" on
      // every call, which turns every window start into a preferences save + full
      // state-file write + preferences-changed broadcast.
      const same =
        linked.name === tmpl.name &&
        (linked.description ?? null) === (tmpl.description ?? null) &&
        linked.cooldown === tmpl.cooldown &&
        stableStringify(linked.when) === stableStringify(tmpl.when) &&
        stableStringify(linked.guards) === stableStringify(tmpl.guards) &&
        stableStringify(linked.sequence) === stableStringify(tmpl.sequence) &&
        stableStringify(linked.supersedes ?? null) === stableStringify(tmpl.supersedes ?? null);
      if (!linked.user_modified && !same) {
        linked.name = tmpl.name;
        linked.description = tmpl.description ?? null;
        linked.cooldown = tmpl.cooldown;
        linked.when = structuredClone(tmpl.when);
        linked.guards = structuredClone(tmpl.guards);
        linked.sequence = structuredClone(tmpl.sequence);
        linked.supersedes = tmpl.supersedes ? [...tmpl.supersedes] : undefined;
        changed = true;
      }
      continue;
    }

    list = [{
      id: crypto.randomUUID(),
      ...structuredClone(tmpl),
      enabled: true,
      workspaces: [],
      default_id: defaultId,
      origin: 'default',
    }, ...list];
    changed = true;
  }

  return changed ? list : null;
}
