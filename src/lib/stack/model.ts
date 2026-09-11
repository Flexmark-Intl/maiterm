/** Pure helpers for the workspace stack (docs/stack.md) — no Svelte, no Tauri, so they
 *  are unit-testable. The store in `stores/stack.svelte.ts` wires them to live state. */

import type { Service } from '$lib/tauri/types';

export type ServiceStatus = 'stopped' | 'starting' | 'running' | 'ready' | 'crashed';

/** Restart-on-crash backoff and ceiling (docs/stack.md §5). */
export const RESTART_BACKOFF_MS = [1000, 2000, 4000, 8000, 16000, 30000];
export const RESTART_CEILING = 5;
export const RESTART_WINDOW_MS = 10 * 60 * 1000;

/** POSIX single-quote. Works in sh/bash/zsh and fish. */
export function shq(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

/** The line typed into the service tab. `env` prefix rather than `K=V cmd` so fish
 *  works too; `cd` first so a relative command resolves where the human expects. */
export function startLine(service: Pick<Service, 'cwd' | 'env' | 'command'>): string {
  const parts: string[] = [];
  if (service.cwd) parts.push(`cd ${shq(service.cwd)}`);
  const env = service.env ?? [];
  const cmd = env.length
    ? `env ${env.map(([k, v]) => `${k}=${shq(v)}`).join(' ')} ${service.command}`
    : service.command;
  parts.push(cmd);
  return parts.join(' && ');
}

/** SIGINT (130) and a clean exit both mean "it stopped"; anything else crashed. */
export function exitIsCrash(code: number): boolean {
  return code !== 0 && code !== 130;
}

/** Whether another automatic restart is allowed: fewer than RESTART_CEILING inside the
 *  window. Pure, so it is testable without a clock. */
export function restartAllowed(restarts: number[], now: number): boolean {
  return restarts.filter((t) => now - t < RESTART_WINDOW_MS).length < RESTART_CEILING;
}

/** Delay before the next automatic restart, growing with the recent count. */
export function restartDelay(recentRestarts: number): number {
  return RESTART_BACKOFF_MS[Math.min(recentRestarts, RESTART_BACKOFF_MS.length - 1)];
}

/** Rollup for the sidebar dot: null when the stack is empty or fully stopped. */
export function rollupStatus(statuses: ServiceStatus[]): 'ready' | 'starting' | 'crashed' | null {
  if (statuses.length === 0) return null;
  if (statuses.some((s) => s === 'crashed')) return 'crashed';
  if (statuses.some((s) => s === 'starting')) return 'starting';
  if (statuses.some((s) => s === 'running' || s === 'ready')) return 'ready';
  return null;
}
