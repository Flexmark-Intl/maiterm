/** The two questions a `!service_id` check used to answer at once (docs/hive.md §5).
 *
 *  A stack service tab is out of the strip AND not an agent; a Hive tab is out of the strip
 *  but IS an agent. Strip, active-tab, counting and cycling code asks `isStripHidden`;
 *  "is this an agent" code (tasks, Loom, mesh, liveness…) asks `isServiceTab`, so Hive agents
 *  keep their tasks and their place in those lists. Rust twins: `Tab::is_strip_hidden` /
 *  `Tab::is_service_tab`. Pure, so stores can import it without a cycle. */

type HiddenFields = { service_id?: string | null; hive_role?: string | null };

/** Not in the tab strip, so never `pane.active_tab_id`. */
export function isStripHidden(tab: HiddenFields): boolean {
  return !!tab.service_id || !!tab.hive_role;
}

/** Runs a stack service, so not an agent. */
export function isServiceTab(tab: HiddenFields): boolean {
  return !!tab.service_id;
}
