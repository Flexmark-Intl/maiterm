/**
 * What a newly resolved place does to the tree on screen (docs/file-tree.md §4.2).
 *
 * The rule is "re-root when the root the tab resolves to differs", not "when the tab's folder
 * leaves the root": a home root contains every project under it, so a tab in `~/proj` (a
 * repository) used to only move the you-are-here tick in `~` and never reach its project. A
 * cd inside a project resolves to the same root and stays cheap (`same`). A folder in no
 * repository resolves to itself, so a cd into a plain subfolder of the root only moves the
 * tick (`locate`: inside the root, reveal; outside, re-root).
 */

import type { TreePlace } from '$lib/tauri/types';
import { sameSshHost } from './remote';

export type FollowStep = 'same' | 'reroot' | 'locate';

type Placed = Pick<TreePlace, 'place' | 'kind' | 'host_key' | 'source'>;

export function followStep(current: Placed | null, next: Placed & Pick<TreePlace, 'project'>): FollowStep {
  if (!current) return 'reroot';
  if (current.place === next.place) return 'same';
  // A pin always comes back as its own place, and a pinned root is left only for another pin.
  if (next.source === 'pin' || current.source === 'pin') return 'reroot';
  // A remote root keys the host's paths, which a local `/tmp/x` can spell too.
  const sameMachine = current.kind === next.kind && (next.kind !== 'remote' || sameSshHost(current.host_key, next.host_key));
  if (!sameMachine) return 'reroot';
  // Another repository's top level, even one inside this root (home, a volume, a folder of
  // checkouts): that project is the tab's root now.
  if (next.project) return 'reroot';
  return 'locate';
}
