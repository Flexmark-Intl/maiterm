/** The board's dependency strings (docs/hive.md §4.4), in the Loom Weave's vocabulary
 *  (docs/loom.md "The weave"): dots moving along a string mean the upstream task is being worked,
 *  a slow amber dash running backwards means the card waits on it, a faint dotted string is a
 *  dependency that is neither. Pure: the board measures the cards and draws what this returns. */

/** A card's box, relative to the layer the strings are drawn in. */
export interface Box {
  l: number;
  r: number;
  t: number;
  b: number;
}

export interface StringNode {
  taskId: string;
  dependsOn: readonly string[];
  /** Its agent is busy (the card reads Working). */
  active: boolean;
  /** Its worker is held until its dependencies are met (phase `waiting`). */
  waiting: boolean;
  /** Its task is done: a dependency on it is met. */
  done: boolean;
}

/** `run`: the upstream is active. `wait`: the downstream waits on an upstream that isn't.
 *  `met`: the upstream is done. `dep`: none of these (a failed or idle upstream nobody waits on). */
export type StringKind = 'run' | 'wait' | 'met' | 'dep';

export interface HiveString {
  key: string;
  from: string;
  to: string;
  d: string;
  kind: StringKind;
  /** The downstream waits on this one: amber, whether or not the upstream is moving. */
  held: boolean;
}

/** One string per dependency between two cards on the board, upstream to downstream. A
 *  dependency that isn't a card (a hand-made task), or isn't measured (not rendered yet), draws
 *  nothing. Waves sit side by side and wrap under each other on a narrow board, so the route is
 *  chosen from the boxes, not the wave numbers: side to side when the downstream is to the right,
 *  else down from the upstream's bottom edge to the downstream's top. */
export function strings(waves: readonly (readonly StringNode[])[], positions: ReadonlyMap<string, Box>): HiveString[] {
  const nodes = new Map<string, StringNode>();
  for (const w of waves) for (const n of w) nodes.set(n.taskId, n);
  const out: HiveString[] = [];
  for (const w of waves) {
    for (const down of w) {
      const db = positions.get(down.taskId);
      if (!db) continue;
      // Deduped: a task list stores dependencies as sent, and a repeat would draw twice.
      for (const id of new Set(down.dependsOn)) {
        const up = id === down.taskId ? undefined : nodes.get(id);
        const ub = up && positions.get(id);
        if (!up || !ub) continue;
        const held = down.waiting && !up.done;
        const kind: StringKind = up.active ? 'run' : held ? 'wait' : up.done ? 'met' : 'dep';
        out.push({ key: `${id}>${down.taskId}`, from: id, to: down.taskId, d: route(ub, db), kind, held });
      }
    }
  }
  return out;
}

const r1 = (n: number) => Math.round(n * 10) / 10;

function route(u: Box, d: Box): string {
  if (d.l >= u.r - 4) {
    const [x1, y1, x2, y2] = [u.r, (u.t + u.b) / 2, d.l, (d.t + d.b) / 2];
    const mx = Math.max(16, (x2 - x1) / 2);
    return `M${r1(x1)},${r1(y1)} C${r1(x1 + mx)},${r1(y1)} ${r1(x2 - mx)},${r1(y2)} ${r1(x2)},${r1(y2)}`;
  }
  const [x1, y1, x2, y2] = [u.l + 14, u.b, d.l + 14, d.t];
  const my = Math.max(16, Math.abs(y2 - y1) / 2);
  return `M${r1(x1)},${r1(y1)} C${r1(x1)},${r1(y1 + my)} ${r1(x2)},${r1(y2 - my)} ${r1(x2)},${r1(y2)}`;
}
