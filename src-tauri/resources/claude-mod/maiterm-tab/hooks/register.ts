import type { EngineInterface, PermissionRequestDecision, Register } from 'claude-code'

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

// --- Answers from maiTerm (claude_code/mod_asks.rs) ---------------------------------------
//
// A permission dialog or an AskUserQuestion selector can be answered from maiTerm (the phone,
// the Loom, the Overlord) as well as at the desktop. The hook behind it registers an ask with
// the event it sends, then waits on GET /hooks/ask in short rounds: one fetch dies at about 30 s,
// and a round this short also lets maiTerm tell a waiting hook from a gone one. A key pressed at
// the desktop still wins; Claude then abandons the hook, which aborts its fetch.

let askSeq = 0

function newAskId(): string {
  askSeq += 1
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}-${askSeq}`
}

// Resolves maiTerm's answer once it gives one, or undefined when maiTerm stops holding the ask
// or can't be reached. `isDone` ends the wait between rounds once the prompt is settled.
async function awaitAnswer($: EngineInterface, to: Link, ask: string, isDone: () => boolean): Promise<unknown> {
  const url = `http://127.0.0.1:${to.port}/hooks/ask?id=${encodeURIComponent(ask)}&wait=10`
  while (!isDone()) {
    let r
    try {
      r = await $.http.fetch(url, { headers: { 'x-claude-code-ide-authorization': to.auth } })
    } catch {
      return undefined
    }
    if (r.status === 204) continue
    if (r.status !== 200) return undefined
    try {
      return JSON.parse(r.text)
    } catch {
      return undefined
    }
  }
  return undefined
}

async function cancelAsk($: EngineInterface, to: Link, ask: string): Promise<void> {
  try {
    await $.http.fetch(`http://127.0.0.1:${to.port}/hooks/ask?id=${encodeURIComponent(ask)}&cancel=1`, {
      headers: { 'x-claude-code-ide-authorization': to.auth },
    })
  } catch {
    // Unanswered, the ask is pruned once nothing polls it.
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
  // Runs while the dialog is on screen (Claude draws it and raises this together). The decision
  // maiTerm hands back closes the dialog as if its row had been pressed.
  on('classic.PermissionRequest', async ($, e, next) => {
    const to = await linkOf($)
    if (!to) return next(e)
    // AskUserQuestion's own permission is the selector itself, answered through tool.call below.
    if (e.tool_name === 'AskUserQuestion') return relay($, e, next)
    const ask = newAskId()
    await send($, to, e, `&ask=${encodeURIComponent(ask)}`)
    const result = await next(e)
    // A settings hook of the user's own already decided: that stands.
    if (result.decision) return result
    const answer = (await awaitAnswer($, to, ask, () => false)) as { decision?: PermissionRequestDecision } | undefined
    const decision = answer?.decision
    if (!decision) return result
    if (decision.behavior === 'deny' && decision.interrupt && !e.agent_id) {
      // A deny fires no hook, and an interrupted turn sends no Stop, so maiTerm would leave the
      // tab waiting on a dialog that is gone. The turn has stopped and the agent waits on its
      // human, which is what idle_prompt tells maiTerm; sent once the dialog has closed. Only
      // for the main thread's own dialog: a subagent's deny says nothing about whether the
      // main thread is still working, and idle_prompt would release its calls too.
      const session_id = e.session_id
      const cwd = e.cwd
      $.clock.after(300, () => {
        void send($, to, { hook_event_name: 'Notification', notification_type: 'idle_prompt', session_id, cwd })
      })
    }
    return { ...result, decision }
  })
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
    const isQuestion = e.tool === 'AskUserQuestion'
    const ask = isQuestion ? newAskId() : undefined
    await send($, to, {
      hook_event_name: 'PreToolUse',
      session_id: await $.session.id(),
      cwd: await $.session.cwd(),
      tool_name: tool,
      tool_input,
      tool_use_id,
      ...(agentId ? { agent_id: agentId } : {}),
    }, ask ? `&ask=${encodeURIComponent(ask)}` : '')
    if (!ask || e.tool !== 'AskUserQuestion') return next(e)

    // The selector (`next`) and maiTerm race; whichever answers first is the tool's result.
    type First =
      | { from: 'desktop'; r: Awaited<ReturnType<typeof next>> }
      | { from: 'maiterm'; answers: Record<string, string> }
    let settled = false
    const local: Promise<First> = next(e).then(r => {
      settled = true
      return { from: 'desktop', r }
    })
    const remote: Promise<First> = awaitAnswer($, to, ask, () => settled).then(a => {
      const answers = (a as { answers?: Record<string, string> } | undefined)?.answers
      // Nothing from maiTerm: the desktop's answer is the only one coming.
      return answers ? { from: 'maiterm', answers } : local
    })
    const first = await Promise.race([local, remote])
    if (first.from === 'desktop') {
      void cancelAsk($, to, ask)
      return first.r
    }
    settled = true
    return { result: { questions: e.questions, answers: first.answers } }
  })
}
