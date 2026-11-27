import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

type StartFn = (tabId: string) => void;
type CompleteFn = (tabId: string, exitCode: number) => void | Promise<void>;

let onStart: StartFn | null = null;
let onComplete: CompleteFn | null = null;

const dispatch = vi.fn();
const oscByTab = new Map<string, { title?: string }>();
let notificationMode: 'auto' | 'in_app' | 'native' | 'disabled' = 'auto';
let notifyMinDuration = 30;

interface MockTab {
  id: string;
  name: string;
  service_id?: string | null;
  hive_role?: string | null;
}

interface MockPane {
  id: string;
  active_tab_id: string | null;
  tabs: MockTab[];
}

interface MockWorkspace {
  id: string;
  active_pane_id: string | null;
  panes: MockPane[];
}

let workspaces: MockWorkspace[] = [];
let activeWorkspace: MockWorkspace | null = null;

vi.mock('./activity.svelte', () => ({
  activityStore: {
    onCommandStart: (fn: StartFn) => { onStart = fn; return () => {}; },
    onCommandComplete: (fn: CompleteFn) => { onComplete = fn; return () => {}; },
  },
}));

vi.mock('./preferences.svelte', () => ({
  preferencesStore: {
    get notificationMode() { return notificationMode; },
    get notifyMinDuration() { return notifyMinDuration; },
  },
}));

vi.mock('./workspaces.svelte', () => ({
  workspacesStore: {
    get workspaces() { return workspaces; },
    get activeWorkspace() { return activeWorkspace; },
  },
}));

vi.mock('./hiveView.svelte', () => ({
  hiveView: {
    tabColumnOpen: () => false,
  },
}));

vi.mock('./terminals.svelte', () => ({
  terminalsStore: {
    getOsc: (tabId: string) => oscByTab.get(tabId),
  },
}));

vi.mock('./notificationDispatch', () => ({ dispatch }));

await import('./notifications.svelte');

describe('Command Completed notifications', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-11-26T18:40:00Z'));
    dispatch.mockReset();
    oscByTab.clear();
    notificationMode = 'auto';
    notifyMinDuration = 30;
    const ws: MockWorkspace = {
      id: 'ws-1',
      active_pane_id: 'pane-1',
      panes: [{
        id: 'pane-1',
        active_tab_id: 'tab-active',
        tabs: [
          { id: 'tab-active', name: 'Active' },
          { id: 'tab-bg', name: 'Build' },
          { id: 'tab-worker', name: 'worker-1', hive_role: 'worker' },
          { id: 'tab-job', name: 'pnpm install', service_id: 'j-096a7f76' },
          { id: 'tab-svc', name: 'web', service_id: 'svc-web' },
        ],
      }],
    };
    workspaces = [ws];
    activeWorkspace = ws;
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('notifies for a background user tab once elapsed time exceeds notifyMinDuration', async () => {
    oscByTab.set('tab-bg', { title: 'cargo test' });
    onStart!('tab-bg');
    vi.advanceTimersByTime(31_000);
    await onComplete!('tab-bg', 0);

    expect(dispatch).toHaveBeenCalledWith(
      'Command Completed',
      '"cargo test" has finished',
      'success',
      { tabId: 'tab-bg', kind: 'completion' },
    );
  });

  it('suppresses notifications for Hive agent tabs (hive_role)', async () => {
    onStart!('tab-worker');
    vi.advanceTimersByTime(60_000);
    await onComplete!('tab-worker', 0);

    expect(dispatch).not.toHaveBeenCalled();
  });

  it('suppresses notifications for Hive run-queue job tabs (service_id)', async () => {
    oscByTab.set('tab-job', { title: 'dprusak@dMac:~/Library/Application Support/com.aiterm.app/hive/3fa31a77/wt.noindex/slice-d1/maiDoc' });
    onStart!('tab-job');
    vi.advanceTimersByTime(70_000);
    await onComplete!('tab-job', 0);

    expect(dispatch).not.toHaveBeenCalled();
  });

  it('suppresses notifications for workspace stack service tabs (service_id)', async () => {
    oscByTab.set('tab-svc', { title: 'dprusak@dMac:~/DATA/IDE/maiSoft/maiProjects' });
    onStart!('tab-svc');
    vi.advanceTimersByTime(120_000);
    await onComplete!('tab-svc', 130);

    expect(dispatch).not.toHaveBeenCalled();
  });

  it('suppresses notifications if a tab is bound to a service after its command started', async () => {
    onStart!('tab-bg');
    workspaces[0].panes[0].tabs.find(t => t.id === 'tab-bg')!.service_id = 'svc-adopted';
    vi.advanceTimersByTime(60_000);
    await onComplete!('tab-bg', 0);

    expect(dispatch).not.toHaveBeenCalled();
  });

  it('suppresses notifications for active or dropped tabs and clears stale start times on short commands', async () => {
    onStart!('tab-active');
    vi.advanceTimersByTime(60_000);
    await onComplete!('tab-active', 0);

    onStart!('tab-missing');
    vi.advanceTimersByTime(60_000);
    await onComplete!('tab-missing', 0);

    // Short command completes, then a stray duplicate completion arrives later without a new start
    onStart!('tab-bg');
    vi.advanceTimersByTime(5_000);
    await onComplete!('tab-bg', 0);
    vi.advanceTimersByTime(60_000);
    await onComplete!('tab-bg', 0);

    expect(dispatch).not.toHaveBeenCalled();
  });
});
