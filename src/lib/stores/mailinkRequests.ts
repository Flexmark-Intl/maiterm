/**
 * Phone actions that must run in THIS window's webview (docs/mailink-protocol.md §13).
 *
 * The Overlord engine is a per-window frontend store, so a phone dismissing an escalation or
 * approving a proposal reaches Rust, which emits `mailink-frontend-request` to the owning
 * window; this answers it through the same oneshot map the MCP tool path uses
 * (`claudeCodeRespond`). Deliberately its OWN event and its OWN dispatcher — not a case in the
 * `claude-code-tool` switch — because `tools/call` does not validate tool names, so a verb
 * reachable from that switch is reachable by every agent over MCP, and these act with the
 * human's authority.
 *
 * Every branch MUST respond, even on a thrown error: an unanswered request leaves the phone
 * waiting the full timeout and then told "not confirmed" for an action that in fact failed.
 */
import * as commands from '$lib/tauri/commands';
import { overlordStore } from '$lib/stores/overlord.svelte';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { error as logError } from '@tauri-apps/plugin-log';

export interface MailinkRequest {
  request_id: string;
  verb: string;
  args: Record<string, unknown>;
}

export async function handleMailinkRequest(req: MailinkRequest): Promise<void> {
  const { request_id, verb } = req;
  const a = (req.args ?? {}) as Record<string, string | number[] | undefined>;
  let result: unknown;
  try {
    switch (verb) {
      case 'tasks.start': {
        // The board's "Do it": lane AND notice. Deliberately NOT reachable from the plain
        // status patch — an agent marking its own row Active must never type a "please pick
        // this up" notice at itself. `told` says what actually reached the agent.
        if (typeof a.id !== 'string') result = { error: 'id is required' };
        else {
          const r = await overlordStore.startTask(a.id);
          // `started: false` means the row was gone by the time this window handled the
          // request — the route resolved it moments earlier. Reported as a REFUSAL rather
          // than a result, because `told: 'nobody'` is otherwise identical to a genuine
          // start of an unassigned row and a client reading `told` would show success.
          if (!r.started) result = { error: 'that task no longer exists' };
          else if (r.told !== 'nobody') result = r;
          else {
            // `nobody` has three causes with three different remedies, and a client that
            // renders one sentence for all of them tells someone to retry a thing that will
            // never work. Said in words rather than as a fourth enum value: the distinction is
            // known here, `reason` already exists in the envelope, and a client that ignores it
            // still behaves correctly — where a new enum member costs every client a branch
            // and an exhaustiveness check forever. (The maiLink agent's argument.)
            const tab = tasksStore.findAnywhere(a.id)?.task.tab_id ?? null;
            const reason = !tab
              ? 'nobody is carrying this task — claim it to a tab and the button means something'
              : overlordStore.isExemptTab(tab)
                ? 'that tab is exempt from the supervisor, so nothing will ever relay this — send it a message yourself'
                : 'the tab could not be reached just then, and there is no supervisor available to relay it';
            result = { ...r, reason };
          }
        }
        break;
      }
      case 'overlord.dismissEscalation': {
        if (typeof a.id !== 'string') result = { error: 'id is required' };
        else { overlordStore.dismissEscalation(a.id); result = { ok: true }; }
        break;
      }
      case 'overlord.approveProposal': {
        // 'started' | 'stale' | 'permission' — a proposal is a snapshot; stale is an outcome, not an error.
        if (typeof a.id !== 'string') result = { error: 'id is required' };
        else result = { outcome: overlordStore.approveProposal(a.id) };
        break;
      }
      case 'overlord.dismissProposal': {
        if (typeof a.id !== 'string') result = { error: 'id is required' };
        else { overlordStore.dismissProposal(a.id); result = { ok: true }; }
        break;
      }
      case 'overlord.resolveRuleChanges': {
        if (typeof a.batchId !== 'string') result = { error: 'batchId is required' };
        else {
          overlordStore.resolveRuleChanges(a.batchId, Array.isArray(a.approvedIdx) ? a.approvedIdx : []);
          result = { ok: true };
        }
        break;
      }
      case 'overlord.driveTab': {
        if (typeof a.tabId !== 'string' || typeof a.text !== 'string' || !a.text) result = { error: 'tabId and text are required' };
        else result = await overlordStore.driveTab(a.tabId, a.kind === 'slash' ? 'slash' : 'process', a.text);
        break;
      }
      case 'overlord.fireRule': {
        if (typeof a.tabId !== 'string' || typeof a.ruleId !== 'string') result = { error: 'tabId and ruleId are required' };
        else result = await overlordStore.fireRule(a.tabId, a.ruleId);
        break;
      }
      case 'overlord.recoverTab': {
        if (typeof a.tabId !== 'string') result = { error: 'tabId is required' };
        else result = await overlordStore.recoverTab(a.tabId);
        break;
      }
      default:
        result = { error: `Unknown maiLink request: ${verb}` };
    }
  } catch (err) {
    logError(`maiLink request ${verb} failed: ${err}`);
    result = { error: String(err) };
  }
  try {
    await commands.claudeCodeRespond(request_id, result);
  } catch (err) {
    logError(`maiLink request ${verb}: respond failed: ${err}`);
  }
}
