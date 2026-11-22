/** Hive settings → Workers and access (docs/hive.md §3.4), pure: the form the sheet edits, and
 *  the change it sends (`hive_set_settings`), naming only what differs from the record. Rust
 *  checks all of it again (`hive::change::check`); this only says early what it would refuse. */

import type { Hive, HiveSettingsChange } from '$lib/tauri/types';
import { normalizeHost } from './budget';

export const MODELS = ['opus', 'sonnet', 'haiku'] as const;
export const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'] as const;
/** `change::ADVISORS`: the workers' advisor. "follow" is stored as "". */
export const ADVISORS = ['follow', 'off', 'fable', 'opus', 'sonnet'] as const;

/** The sheet's word for an advisor choice; `followed` is what "follow" gives now ("" for none). */
export function advisorLabel(choice: string, followed: string | null): string {
  if (choice === 'follow') {
    if (followed === null) return 'As in my Claude settings';
    return `As in my Claude settings (${followed ? `Claude ${followed}` : 'none'})`;
  }
  return choice === 'off' ? 'None' : `Claude ${choice}`;
}
/** `change::MAX_CAP` and `MAX_WORKER_HOSTS`. */
export const MAX_CAP = 64;
export const MAX_WORKER_HOSTS = 50;
/** `change::MAX_WORKER_SOCKETS`. */
export const MAX_WORKER_SOCKETS = 10;

/** Who is a chat on the phone: `keep_off_phone` and `workers_on_phone`, as one choice. The
 *  "nobody" choice is fixed at Create (its coordinator was made off the phone). */
export type PhoneChoice = 'coordinator' | 'all';

export interface ChangeForm {
  cap: number;
  coordinatorModel: string;
  coordinatorEffort: string;
  workerModel: string;
  workerEffort: string;
  /** One of `ADVISORS`. */
  workerAdvisor: string;
  hosts: string[];
  sockets: string[];
  pushAfterLand: boolean;
  phone: PhoneChoice;
}

type Fields = Pick<Hive, 'cap' | 'models' | 'push_after_land' | 'settings'>;

export function formFor(hive: Fields): ChangeForm {
  return {
    cap: hive.cap,
    coordinatorModel: hive.models.coordinator.model,
    coordinatorEffort: hive.models.coordinator.effort,
    workerModel: hive.models.worker.model,
    workerEffort: hive.models.worker.effort,
    workerAdvisor: hive.settings?.worker_advisor || 'follow',
    hosts: [...(hive.settings?.worker_hosts ?? [])],
    sockets: [...(hive.settings?.worker_sockets ?? [])],
    pushAfterLand: hive.push_after_land,
    phone: hive.settings?.workers_on_phone ? 'all' : 'coordinator',
  };
}

/** A host typed into the list, as it would be kept, or why not. */
export function addHost(hosts: string[], raw: string): { hosts: string[] } | { error: string } {
  const n = normalizeHost(raw);
  if ('error' in n) return n;
  if (hosts.includes(n.host)) return { error: `${n.host} is already on the list.` };
  if (hosts.length >= MAX_WORKER_HOSTS) return { error: `Workers may reach at most ${MAX_WORKER_HOSTS} hosts beyond the package registries.` };
  return { hosts: [...hosts, n.host] };
}

/** `change::is_database_socket_name`: a Postgres, MySQL or Redis socket's name, and only that. */
export function isDatabaseSocketName(name: string): boolean {
  const pg = /^\.s\.PGSQL\.(\d{1,5})$/.exec(name);
  if (pg) return true;
  return /^(mysqld|mysql|redis)[A-Za-z0-9_-]*\.sock$/i.test(name);
}

/** A socket path typed into the list, as it would be kept, or why not (`change::check_socket`). */
export function addSocket(sockets: string[], raw: string): { sockets: string[] } | { error: string } {
  const s = raw.trim();
  const parts = s.split('/');
  if (!s.startsWith('/') || /["\\\u0000-\u001f\u007f-\u009f]/.test(s) || parts.some((p) => p === '..' || p === '.')) return { error: `"${s}" isn't a full path to a socket, such as /tmp/.s.PGSQL.5432.` };
  if (!isDatabaseSocketName(parts[parts.length - 1] ?? '')) {
    return { error: `${s} isn't named like a database's socket (.s.PGSQL.<port>, mysql.sock, redis.sock), so workers can't be given it: other local sockets reach far more than a database.` };
  }
  if (sockets.includes(s)) return { error: `${s} is already on the list.` };
  if (sockets.length >= MAX_WORKER_SOCKETS) return { error: `Workers may reach at most ${MAX_WORKER_SOCKETS} local database sockets.` };
  return { sockets: [...sockets, s] };
}

export function validateForm(f: ChangeForm): string | null {
  if (!Number.isInteger(f.cap) || f.cap < 1 || f.cap > MAX_CAP) return `Workers at once is 1 to ${MAX_CAP}.`;
  return null;
}

/** The change to send: only the fields that differ from the record. Null when nothing does. */
export function toChange(hive: Fields, f: ChangeForm): HiveSettingsChange | null {
  const c: HiveSettingsChange = {};
  const was = formFor(hive);
  if (f.cap !== was.cap) c.cap = f.cap;
  if (f.coordinatorModel !== was.coordinatorModel) c.coordinator_model = f.coordinatorModel;
  if (f.coordinatorEffort !== was.coordinatorEffort) c.coordinator_effort = f.coordinatorEffort;
  if (f.workerModel !== was.workerModel) c.worker_model = f.workerModel;
  if (f.workerEffort !== was.workerEffort) c.worker_effort = f.workerEffort;
  if (f.workerAdvisor !== was.workerAdvisor) c.worker_advisor = f.workerAdvisor;
  const added = f.hosts.filter((h) => !was.hosts.includes(h));
  const removed = was.hosts.filter((h) => !f.hosts.includes(h));
  if (added.length) c.add_hosts = added;
  if (removed.length) c.remove_hosts = removed;
  const addedSockets = f.sockets.filter((s) => !was.sockets.includes(s));
  const removedSockets = was.sockets.filter((s) => !f.sockets.includes(s));
  if (addedSockets.length) c.add_sockets = addedSockets;
  if (removedSockets.length) c.remove_sockets = removedSockets;
  if (f.pushAfterLand !== was.pushAfterLand) c.push_after_land = f.pushAfterLand;
  if (f.phone !== was.phone) c.workers_on_phone = f.phone === 'all';
  return Object.keys(c).length ? c : null;
}

/** The header's and the sheet's word for a model. */
export function modelLabel(m: { model: string; effort: string }): string {
  return `Claude ${m.model || 'default'}${m.effort ? `, ${m.effort} effort` : ''}`;
}
