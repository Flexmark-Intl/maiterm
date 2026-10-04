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
      case 'tasks.answer': {
        // v0.13: the phone answers a task's blocker (docs/tasks.md §3.1). Same verb as the
        // desktop's BlockerCard, human-only for the same reason as tasks.start. The phone has
        // no freshness pause of its own, so askedAt is its whole stale guard, and required.
        const opt = (req.args ?? {}).option;
        const text = (req.args ?? {}).text;
        if (typeof a.id !== 'string' || typeof a.askedAt !== 'string') {
          result = { error: 'id and askedAt are required' };
        } else if (opt !== undefined && opt !== null && typeof opt !== 'number') {
          result = { error: 'option must be a number' };
        } else {
          const r = await overlordStore.answerBlocker(a.id, {
            asked_at: a.askedAt,
            option: typeof opt === 'number' ? opt : undefined,
            text: typeof text === 'string' ? text : undefined,
          });
          // A refusal (stale, no longer blocked, a bad option) is the §13.4 envelope's
          // `accepted:false` with the sentence as `reason`, like every other refusal. Returned as
          // a result it read as accepted, and a client following the envelope rule showed a stale
          // answer as delivered.
          result = r.answered ? r : { error: r.detail ?? 'That answer was not accepted.' };
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
      // Move project (docs/relocate.md): Rust's relocate_project, not the phone, asks these —
      // this is the one channel that reaches a window's webview and waits for its answer.
      case 'relocate.suspend': {
        const roots = (req.args ?? {}).roots;
        if (!Array.isArray(roots) || !roots.every((r) => typeof r === 'string')) result = { error: 'roots is required' };
        else {
          const { suspendUnder } = await import('$lib/stores/relocate.svelte');
          result = await suspendUnder(roots as string[]);
        }
        break;
      }
      case 'relocate.apply': {
        const { applyPatch } = await import('$lib/stores/relocate.svelte');
        result = applyPatch(req.args as Parameters<typeof applyPatch>[0]);
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
