import type { Trigger, MatchMode } from '$lib/tauri/types';
import { getResumeCommand } from '$lib/agents/resume';

/** Shared Claude resume command — used by auto-resume presets and hooks-based auto-resume. */
export const CLAUDE_RESUME_COMMAND = getResumeCommand('claude');

/** App-provided default trigger templates. Keyed by stable default_id. */
export const DEFAULT_TRIGGERS: Record<string, Omit<Trigger, 'id' | 'enabled' | 'workspaces' | 'tabs' | 'default_id'> & { match_mode?: MatchMode }> = {
};

/**
 * Seed default triggers into an existing trigger list.
 * Returns the updated list if changes were made, or null if no changes needed.
 */
export function seedDefaultTriggers(
  existing: Trigger[],
  hiddenIds: string[],
  enableAll = false,
): Trigger[] | null {
  let list = [...existing];
  let changed = false;

  // Remove triggers whose default_id no longer exists in DEFAULT_TRIGGERS
  const before = list.length;
  list = list.filter(t => {
    if (!t.default_id) return true; // user-created
    if (t.default_id in DEFAULT_TRIGGERS) return true; // still active
    return false; // stale default — remove
  });
  if (list.length !== before) changed = true;

  for (const [defaultId, tmpl] of Object.entries(DEFAULT_TRIGGERS)) {
    if (hiddenIds.includes(defaultId)) continue;

    const linked = list.find(t => t.default_id === defaultId);
    if (linked) {
      // Auto-update unmodified defaults to latest template values
      if (!linked.user_modified) {
        linked.name = tmpl.name;
        linked.description = tmpl.description ?? null;
        linked.pattern = tmpl.pattern;
        linked.cooldown = tmpl.cooldown;
        linked.plain_text = tmpl.plain_text;
        linked.match_mode = tmpl.match_mode ?? null;
        linked.actions = structuredClone(tmpl.actions);
        linked.variables = structuredClone(tmpl.variables);
        changed = true;
      }
      continue;
    }

    // Adopt existing trigger that matches by name
    const match = list.find(t => !t.default_id && t.name === tmpl.name);
    if (match) {
      match.default_id = defaultId;
      if (!match.description && tmpl.description) {
        match.description = tmpl.description;
      }
      changed = true;
      continue;
    }

    // Seed new default trigger
    list = [{
      id: crypto.randomUUID(),
      ...structuredClone(tmpl),
      enabled: enableAll,
      workspaces: [],
      tabs: [],
      default_id: defaultId,
    }, ...list];
    changed = true;
  }

  return changed ? list : null;
}
