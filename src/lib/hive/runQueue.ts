/** The Run queue settings form (docs/hive.md §6.8): a Hive's queued commands as editable text,
 *  and back. Rust checks everything again on save (`run::validate_run_queue`); the form only
 *  says early what Rust would refuse. */
import type { QueuedCommand, RunQueueSettings } from '$lib/tauri/types';

/** One queued command as the form edits it: lists as comma-separated text, env and extra
 *  writes one per line. */
export interface QueueRow {
  folder: string;
  prefix: string;
  needs: string;
  services: string;
  env: string;
  extraWrite: string;
}

/** What `hiveRun` refuses in a command, and so in a queued prefix (`run::REFUSED`). */
const REFUSED = /[;|&$`<>()\\'"\r\n]/;

export function prefixError(prefix: string): string | null {
  if (!prefix.trim()) return 'A queued command needs its leading words (cargo test).';
  const bad = prefix.match(REFUSED);
  if (bad) return `Plain words only: ${bad[0] === '\n' || bad[0] === '\r' ? 'a line break' : `“${bad[0]}”`} is shell syntax, which hiveRun refuses.`;
  return null;
}

const list = (text: string) => text.split(',').map((s) => s.trim()).filter(Boolean);
const lines = (text: string) => text.split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

export function toRows(rq: RunQueueSettings | undefined): QueueRow[] {
  return (rq?.commands ?? []).map((c) => ({
    folder: c.folder,
    prefix: c.prefix,
    needs: c.needs.join(', '),
    services: c.services.join(', '),
    env: c.env.map(([k, v]) => `${k}=${v}`).join('\n'),
    extraWrite: c.extra_write.join('\n'),
  }));
}

/** `KEY=value` per line, the first `=` splitting; a line without one is dropped. */
function parseEnv(text: string): [string, string][] {
  const out: [string, string][] = [];
  for (const line of lines(text)) {
    const eq = line.indexOf('=');
    if (eq > 0) out.push([line.slice(0, eq).trim(), line.slice(eq + 1).trim()]);
  }
  return out;
}

export function resourcesText(rq: RunQueueSettings | undefined): string {
  return Object.entries(rq?.resources ?? {}).map(([k, v]) => `${k}=${v}`).join('\n');
}

export function fromRows(rows: QueueRow[], resources: string): RunQueueSettings {
  const commands: QueuedCommand[] = rows.map((r) => ({
    folder: r.folder,
    prefix: r.prefix.trim().split(/\s+/).join(' '),
    needs: list(r.needs),
    services: list(r.services),
    env: parseEnv(r.env),
    extra_write: lines(r.extraWrite),
  }));
  const res: Record<string, number> = {};
  for (const [k, v] of parseEnv(resources)) {
    const n = Number(v);
    // Rust refuses what isn't 1 to 64; a non-number goes through as 0 so the refusal names it.
    res[k] = Number.isInteger(n) ? n : 0;
  }
  return { commands, resources: res };
}
