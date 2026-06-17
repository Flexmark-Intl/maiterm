import { listen } from '@tauri-apps/api/event';
import { scpUploadFiles, cancelScpUpload } from '$lib/tauri/commands';
import { toastStore } from '$lib/stores/toasts.svelte';
import type { ScpProgress } from '$lib/tauri/types';

function fmtBytes(n: number): string {
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/**
 * Remote staging directory for files uploaded into an agent session (Claude
 * Code et al.) over SSH. Single source of truth so the rebranded path can't
 * drift across the call sites that reference it.
 */
export const AGENT_UPLOAD_DIR = '/tmp/maiterm-uploads';

export type UploadOutcome = { status: 'done' | 'cancelled' | 'error'; error?: string };

/**
 * Run an SCP upload with a live progress toast (and a Cancel button).
 *
 * Shows a sticky progress toast, streams `scp-progress-{id}` events into it,
 * and lets the user cancel the transfer. Resolves with an outcome — the caller
 * handles success (e.g. writing the uploaded paths to the terminal) and decides
 * how to surface errors. Cancellation is surfaced by the helper itself.
 */
export async function uploadWithProgress(
  sshCommand: string,
  paths: string[],
  remoteDir: string,
  opts?: { titlePrefix?: string },
): Promise<UploadOutcome> {
  const uploadId = crypto.randomUUID();
  const count = paths.length;
  const label = opts?.titlePrefix ?? 'SCP Upload';
  const fileWord = `${count} file${count > 1 ? 's' : ''}`;

  let cancelled = false;
  const toastId = toastStore.addProgressToast({
    title: label,
    body: `Uploading ${fileWord}…`,
    onCancel: () => {
      cancelled = true;
      cancelScpUpload(uploadId).catch(() => {});
    },
  });

  const unlisten = await listen<ScpProgress>(`scp-progress-${uploadId}`, (e) => {
    const p = e.payload;
    // The terminal "done" frame is handled when the command promise settles.
    if (p.done) return;
    if (p.indeterminate) {
      toastStore.updateToast(toastId, { body: `Uploading ${fileWord}…`, indeterminate: true });
    } else {
      const pct = Math.round(p.percent);
      const rate = p.rate_bps > 0 ? ` · ${fmtBytes(p.rate_bps)}/s` : '';
      toastStore.updateToast(toastId, {
        body: `${fmtBytes(p.bytes_sent)} / ${fmtBytes(p.total_bytes)} (${pct}%)${rate}`,
        progress: p.percent,
        indeterminate: false,
      });
    }
  });

  try {
    await scpUploadFiles(sshCommand, paths, remoteDir, uploadId);
    toastStore.removeToast(toastId);
    return { status: 'done' };
  } catch (err) {
    toastStore.removeToast(toastId);
    const msg = String(err);
    if (cancelled || /cancel/i.test(msg)) {
      toastStore.addToast(label, 'Upload cancelled', 'info');
      return { status: 'cancelled' };
    }
    return { status: 'error', error: msg };
  } finally {
    unlisten();
  }
}
