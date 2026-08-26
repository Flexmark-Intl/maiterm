/**
 * Merge freshly OBSERVED auto-resume paths over the SAVED ones.
 *
 * The rule is one-way: an observation may REPLACE a saved path, never ERASE one. A null here
 * means "not visible from where I am standing" — the remote shell has not reported its cwd yet
 * (OSC 7 / prompt cwd), or lsof could not read the local one — and that is never a reason to
 * forget a path the user pinned, typed, or that the tab has been resuming into for weeks.
 *
 * The incident behind it: `handleEnableAutoResume` runs on `agent-init-session`, which fires
 * when the AGENT STARTS — often before the remote end has said where it is. The old code wrote
 * that null straight over a saved path, and the damage compounds: with no remote cwd the next
 * ssh replay carries no `cd`, so the session lands in the remote HOME, and home is then observed
 * and recorded as the tab's cwd. Seen live — four tabs on one host reduced to `/home/ews`, while
 * the single PINNED tab (which this path skips) kept its real project path.
 */
export function mergeAutoResumeContext(
  observed: { cwd: string | null; remoteCwd: string | null },
  saved: { cwd: string | null; remoteCwd: string | null },
): { cwd: string | null; remoteCwd: string | null } {
  return {
    cwd: observed.cwd ?? saved.cwd,
    remoteCwd: observed.remoteCwd ?? saved.remoteCwd,
  };
}
