import { writeTerminal } from '$lib/tauri/commands';

const enc = (s: string) => Array.from(new TextEncoder().encode(s));
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/**
 * Settle window (ms) to wait after closing a bracketed paste, before sending the
 * submitting CR.
 *
 * Claude Code (and Codex) collapse a multi-line / large / image-bearing paste
 * into a `[Pasted text #N]` / `[Image #N]` chip via ASYNCHRONOUS processing
 * (buffering the text, reading attachment files, re-rendering the placeholder). A
 * CR that arrives before that settles is swallowed — the prompt stages in the
 * input but never submits. A short typed line has nothing to buffer and settles
 * instantly, which is why small prompts always submitted but big pastes, image
 * sends, and 20-line agent-bridge replies didn't.
 *
 * There's no readable "paste settled" signal from the TUI, so we scale a delay
 * with payload size (and attachment count) and cap it. Empirical, not exact — if
 * a very large paste ever still fails to submit, widen this.
 */
export function pasteSettleMs(textLength: number, attachmentCount = 0): number {
  return Math.min(1200, 100 + Math.floor(textLength / 5) + attachmentCount * 180);
}

/**
 * Inject `text` into a PTY as a bracketed paste, then submit it with a CR sent as
 * a SEPARATE keystroke after a settle delay (see {@link pasteSettleMs} for why
 * the CR can't ride in the same write).
 *
 * The single submit path for interactive-agent prompts — both the composer dock
 * and the agent-to-agent bridge route through here so the timing can't drift
 * apart. Assumes the foreground app has bracketed paste on (every target is a
 * Claude/Codex TUI); callers that may face a plain shell handle that themselves.
 */
export async function bracketedPasteSubmit(
  ptyId: string,
  text: string,
  attachmentCount = 0,
): Promise<void> {
  await writeTerminal(ptyId, enc(`\x1b[200~${text}\x1b[201~`));
  await sleep(pasteSettleMs(text.length, attachmentCount));
  await writeTerminal(ptyId, enc('\r'));
}
