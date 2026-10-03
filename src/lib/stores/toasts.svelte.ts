import { preferencesStore } from './preferences.svelte';

export interface ToastSource {
  tabId: string;
}

export interface Toast {
  id: string;
  title: string;
  body: string;
  type: 'success' | 'error' | 'info';
  createdAt: number;
  duration: number;
  source?: ToastSource;
  /** Optional callback invoked when the toast is clicked. */
  action?: () => void;
  /** Progress toasts don't auto-dismiss; they show a determinate/indeterminate bar. */
  sticky?: boolean;
  /** 0–100 fill for the progress bar (null when not a progress toast). */
  progress?: number | null;
  /** Show an indeterminate (marquee) bar instead of a percentage. */
  indeterminate?: boolean;
  /** Optional callback for a Cancel button (replaces the close × on progress toasts). */
  onCancel?: () => void;
  /** A request waiting on the human (`addRequest`): sticky, with no bar of any kind. */
  request?: boolean;
  /** What the toast is about, for `removeByKey`. */
  key?: string;
}

const MAX_VISIBLE = 3;

interface TimerState {
  timer: ReturnType<typeof setTimeout>;
  remaining: number;
  pausedAt: number | null;
}

function createToastStore() {
  let toasts = $state<Toast[]>([]);
  let windowFocused = $state(true);
  const timers = new Map<string, TimerState>();
  // Track which toasts are hovered (to avoid resuming on focus if still hovered)
  const hoveredIds = new Set<string>();
  // Reactive signal bumped on every timer state change so isActive() triggers re-renders
  let timerVersion = $state(0);

  /** The toast whose countdown runs: the oldest one that HAS one. A sticky toast (progress, a
   *  request waiting on the human) has no timer, and as the head it used to stall every toast
   *  behind it — a request waits hours, so nothing else ever auto-dismissed. */
  function activeId(): string | null {
    return toasts.find(t => !t.sticky)?.id ?? null;
  }

  /** Keep the ordinary toasts to MAX_VISIBLE by dropping the oldest. Requests don't count and are
   *  never dropped: one is removed only by its owner or the human — evicted, it would be gone
   *  while its question still waits, and it is announced once. Counting them, three waiting
   *  requests made every new toast the only candidate, removed the moment it was added (review
   *  of 30b4844). Owners keep requests few: follow-ups use ONE for all its waiting scripts. */
  function evictOverflow() {
    while (toasts.filter(t => !t.request).length > MAX_VISIBLE) {
      removeToast(toasts.find(t => !t.request)!.id);
    }
  }

  function startTimer(id: string, ms: number) {
    const timer = setTimeout(() => {
      timers.delete(id);
      removeToast(id);
    }, ms);
    timers.set(id, { timer, remaining: ms, pausedAt: null });
    timerVersion++;
  }

  /** Create a timer entry in paused/waiting state (no setTimeout).
   *  pausedAt = -1 signals "never started" — resume uses full remaining time. */
  function createPausedTimer(id: string, ms: number) {
    timers.set(id, { timer: undefined as unknown as ReturnType<typeof setTimeout>, remaining: ms, pausedAt: -1 });
    timerVersion++;
  }

  /** Compute remaining ms, accounting for never-started timers (pausedAt === -1). */
  function computeRemaining(ts: TimerState): number {
    if (ts.pausedAt === null || ts.pausedAt === -1) return ts.remaining;
    const elapsed = Date.now() - ts.pausedAt;
    return Math.max(0, ts.remaining - elapsed);
  }

  /** Start a timer with the given remaining ms, updating the TimerState. */
  function fireTimer(id: string, ts: TimerState, ms: number) {
    ts.pausedAt = null;
    ts.remaining = ms;
    ts.timer = setTimeout(() => {
      timers.delete(id);
      removeToast(id);
    }, ms);
    timerVersion++;
  }

  /** Activate the next toast at index 0 after a removal, if conditions allow. */
  function activateNext() {
    const next = activeId();
    if (!next) return;
    const ts = timers.get(next);
    if (!ts) return;
    if (windowFocused && !hoveredIds.has(next) && ts.pausedAt !== null) {
      fireTimer(next, ts, computeRemaining(ts));
    }
  }

  function removeToast(id: string) {
    const wasActive = activeId() === id;
    const ts = timers.get(id);
    if (ts) {
      clearTimeout(ts.timer);
      timers.delete(id);
    }
    hoveredIds.delete(id);
    toasts = toasts.filter(t => t.id !== id);
    if (wasActive) activateNext();
  }

  function pauseToast(id: string) {
    hoveredIds.add(id);
    // Only the active toast has a running timer to pause
    if (id !== activeId()) return;
    const ts = timers.get(id);
    if (!ts || ts.pausedAt !== null) return;
    clearTimeout(ts.timer);
    ts.pausedAt = Date.now();
    timerVersion++;
  }

  function resumeToast(id: string) {
    hoveredIds.delete(id);
    // Only resume the active toast, and only if window is focused
    if (id !== activeId() || !windowFocused) return;
    const ts = timers.get(id);
    if (!ts || ts.pausedAt === null) return;
    fireTimer(id, ts, computeRemaining(ts));
  }

  function addToast(title: string, body: string, type: Toast['type'] = 'info', source?: ToastSource, focused?: boolean, action?: () => void) {
    const id = crypto.randomUUID();
    const durationMs = preferencesStore.toastDuration * 1000;
    const toast: Toast = { id, title, body, type, createdAt: Date.now(), duration: durationMs, source, action };
    toasts = [...toasts, toast];

    // If caller provides explicit focus state, trust it over our tracked state
    // (dispatch() already queries isFocused() and may know better than the
    // event-driven windowFocused flag, which can lag on startup)
    if (focused !== undefined) windowFocused = focused;
    const isFocused = windowFocused;

    // Only the active toast gets a running timer, and only if focused
    if (activeId() === id && isFocused) {
      startTimer(id, durationMs);
    } else {
      createPausedTimer(id, durationMs);
    }

    evictOverflow();
  }

  /** A request that waits for the human: no timer, no progress bar, and it stays until its
   *  owner removes it (`removeByKey`) or the human dismisses it. `key` names what it is about,
   *  so asking again replaces the old one instead of stacking a second. */
  function addRequest(opts: { key: string; title: string; body: string; source?: ToastSource; action?: () => void }): string {
    removeByKey(opts.key);
    const id = crypto.randomUUID();
    const toast: Toast = {
      id, key: opts.key, title: opts.title, body: opts.body, type: 'info', createdAt: Date.now(),
      duration: 0, sticky: true, request: true, source: opts.source, action: opts.action,
    };
    toasts = [...toasts, toast];
    evictOverflow();
    return id;
  }

  function removeByKey(key: string) {
    for (const t of toasts.filter((x) => x.key === key)) removeToast(t.id);
  }

  /** Is a toast with this key showing (the human may have dismissed it)? */
  function hasKey(key: string): boolean {
    return toasts.some((x) => x.key === key);
  }

  /** Re-word a showing toast in place, silently. */
  function updateByKey(key: string, patch: { title?: string; body?: string }) {
    for (const t of toasts.filter((x) => x.key === key)) {
      if (patch.title !== undefined) t.title = patch.title;
      if (patch.body !== undefined) t.body = patch.body;
    }
  }

  /** Add a sticky progress toast (no auto-dismiss). Returns its id for updateToast/removeToast. */
  function addProgressToast(opts: { title: string; body: string; onCancel?: () => void }): string {
    const id = crypto.randomUUID();
    const toast: Toast = {
      id,
      title: opts.title,
      body: opts.body,
      type: 'info',
      createdAt: Date.now(),
      duration: 0,
      sticky: true,
      progress: 0,
      indeterminate: false,
      onCancel: opts.onCancel,
    };
    toasts = [...toasts, toast];
    // Sticky toasts get no timer — they persist until updated/removed.
    evictOverflow();
    return id;
  }

  /** Patch an existing toast in place (used to stream progress updates). */
  function updateToast(
    id: string,
    patch: Partial<Pick<Toast, 'title' | 'body' | 'type' | 'progress' | 'indeterminate'>>,
  ) {
    const t = toasts.find((x) => x.id === id);
    if (!t) return;
    if (patch.title !== undefined) t.title = patch.title;
    if (patch.body !== undefined) t.body = patch.body;
    if (patch.type !== undefined) t.type = patch.type;
    if (patch.progress !== undefined) t.progress = patch.progress;
    if (patch.indeterminate !== undefined) t.indeterminate = patch.indeterminate;
  }

  function setWindowFocused(focused: boolean) {
    windowFocused = focused;
    const active = activeId();
    if (!active) return;

    if (!focused) {
      // Pause the active toast's timer
      const ts = timers.get(active);
      if (ts && ts.pausedAt === null) {
        clearTimeout(ts.timer);
        ts.pausedAt = Date.now();
        timerVersion++;
      }
    } else {
      // Resume the active toast if not hovered
      if (!hoveredIds.has(active)) {
        const ts = timers.get(active);
        if (ts && ts.pausedAt !== null) {
          fireTimer(active, ts, computeRemaining(ts));
        }
      }
    }
  }

  /**
   * Returns true if this toast's progress bar should be animating.
   * True only when: it's the active toast (index 0), not paused by hover or window blur.
   */
  function isActive(id: string): boolean {
    void timerVersion; // reactive dependency — re-evaluate when timer state changes
    if (id !== activeId()) return false;
    const ts = timers.get(id);
    return !!ts && ts.pausedAt === null;
  }

  return {
    get toasts() { return toasts; },
    addToast,
    addRequest,
    removeByKey,
    hasKey,
    updateByKey,
    addProgressToast,
    updateToast,
    removeToast,
    pauseToast,
    resumeToast,
    setWindowFocused,
    isActive,

    /** Diagnostic snapshot for getDiagnostics. */
    getInternalSizes() {
      return {
        toasts: toasts.length,
        timers: timers.size,
        hovered: hoveredIds.size,
      };
    },
  };
}

export const toastStore = createToastStore();
