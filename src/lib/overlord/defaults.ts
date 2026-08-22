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

  todo_hygiene: {
    name: 'Keep a todo list',
    description:
      'A tab doing sustained work with no todo list gets nudged to create one with its native task tooling.',
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
        text: "If the current work is multi-step, track it: create a proper task/todo list with your task tooling so nothing gets lost. If it's a one-off, ignore this.",
        await: { until: 'turn_end' },
        timeout_seconds: 600,
        on_timeout: 'continue',
      },
    ],
  },
};

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
      if (!linked.user_modified) {
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
