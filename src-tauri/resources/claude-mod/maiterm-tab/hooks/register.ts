import type { EngineInterface, Register } from 'claude-code'

// maiTerm's link to the Claude Code session running in one of its tabs.
//
// maiTerm writes this folder into its data directory and names it in every tab's
// CLAUDE_CODE_PLUGIN_DIRS (claude_code/claude_mod.rs). It forwards the same hook events
// maiTerm's settings.json hooks send to POST /hooks, with two differences that are the point:
// every event names its tab (`tab_id=`), so nothing has to be inferred from a session id, and
// it says it came from here (`via=mod`), so the server drops the settings hooks' anonymous
// copy of each one. A Claude Code too old for mods never loads this, and its settings hooks
// run exactly as before.

// Tested against 2.1.295; the API is early access and may move between releases.
const MIN_VERSION = [2, 1, 290]

type Link = { tab: string; port: string; auth: string }

let link: Promise<Link | null> | undefined

function isAtLeast(version: string, min: number[]): boolean {
  const parts = version.split(/[.-]/).map(n => Number.parseInt(n, 10))
  for (let i = 0; i < min.length; i++) {
    const have = parts[i] ?? 0
    const want = min[i] ?? 0
    if (Number.isNaN(have) || have < want) return false
    if (have > want) return true
  }
  return true
}

async function load($: EngineInterface): Promise<Link | null> {
  const [tab, port, auth] = await Promise.all([
    $.env.get('MAITERM_TAB_ID'),
    $.env.get('MAITERM_PORT'),
    $.env.get('MAITERM_AUTH'),
  ])
  if (!tab || !port || !auth) return null
  const { version } = await $.session.version()
  if (!isAtLeast(version, MIN_VERSION)) return null
  // Tells maiTerm's SessionStart/SessionEnd command hooks, which run beneath this mod and
  // inherit this process's environment, that this mod speaks for the tab.
  await $.env.set('MAITERM_VIA_MOD', '1')
  return { tab, port, auth }
}

// Resolved once per module load. A load that fails is retried by the next event rather than
// leaving the tab unlinked for the rest of the session.
function linkOf($: EngineInterface): Promise<Link | null> {
  link ??= load($).catch(() => {
    link = undefined
    return null
  })
  return link
}

// POSTs one hook event; resolves the reply body, or undefined when maiTerm did not answer.
// Never rejects: a hook that throws would be skipped, and its event lost.
async function send($: EngineInterface, to: Link, event: unknown, query = ''): Promise<string | undefined> {
  try {
    const r = await $.http.fetch(
      `http://127.0.0.1:${to.port}/hooks?via=mod&tab_id=${encodeURIComponent(to.tab)}${query}`,
      {
        method: 'POST',
        headers: { 'x-claude-code-ide-authorization': to.auth, 'content-type': 'application/json' },
        body: JSON.stringify(event),
      },
    )
    return r.ok ? r.text : undefined
  } catch {
    return undefined
  }
}

// The events maiTerm's server reads, forwarded as the settings hooks would have sent them.
// Sent BEFORE `next(e)`: the settings hooks run beneath, so maiTerm hears this copy first
// and knows to drop theirs.
async function relay<E, R>($: EngineInterface, e: E, next: (e: E) => Promise<R>): Promise<R> {
  const to = await linkOf($)
  if (to) await send($, to, e)
  return next(e)
}

export const register: Register = on => {
  on('classic.SessionStart', async ($, e, next) => {
    const to = await linkOf($)
    if (!to) return next(e)
    // The server's reply is the session's standing instructions: the tab and session ids and
    // whatever this tab's features need (the Overlord, tasks, the stack, follow-ups).
    const priming = await send($, to, e, '&prime=1')
    const result = await next(e)
    if (!priming) return result
    return { ...result, additionalContext: [...(result.additionalContext ?? []), priming] }
  })

  on('classic.SessionEnd', relay)
  on('classic.Notification', relay)
  on('classic.Stop', relay)
  on('classic.UserPromptSubmit', relay)
  on('classic.PostToolUse', relay)
  on('classic.PostToolUseFailure', relay)
  on('classic.PermissionRequest', relay)
  on('classic.SubagentStop', relay)
  on('classic.PreCompact', relay)

  // The one event whose reply matters: maiTerm allows a model switch it asked for, which
  // skips Claude's "Switch model?" cache-miss confirm.
  on('classic.PreModelSwitch', async ($, e, next) => {
    const to = await linkOf($)
    if (!to) return next(e)
    const reply = await send($, to, e)
    const result = await next(e)
    try {
      const decision = reply ? JSON.parse(reply)?.hookSpecificOutput : undefined
      if (decision?.permissionDecision === 'allow') {
        return { ...result, permissionDecision: 'allow', permissionDecisionReason: decision.permissionDecisionReason }
      }
    } catch {
      // An empty or non-JSON reply decides nothing.
    }
    return result
  })

  // PreToolUse comes from `tool.call` rather than `classic.PreToolUse`, whose input is the bare
  // tool call: it lacks the session id and, in a subagent, the agent's id, both of which the
  // permission ledger keys on (claude_code/gate.rs). This is the same moment, one level up.
  on('tool.call', async ($, e, next) => {
    const to = await linkOf($)
    if (!to) return next(e)
    const { tool, tool_use_id, agentId, consent: _consent, ...tool_input } = e as Record<string, unknown>
    await send($, to, {
      hook_event_name: 'PreToolUse',
      session_id: await $.session.id(),
      cwd: await $.session.cwd(),
      tool_name: tool,
      tool_input,
      tool_use_id,
      ...(agentId ? { agent_id: agentId } : {}),
    })
    return next(e)
  })
}
