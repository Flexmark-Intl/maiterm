// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const SUBAGENT = { id: 'a0123456789abcdef', name: 'reviewer', description: 'review', type: 'general-purpose', status: 'running' as const }
const TEAMMATE = { id: 'a1111111111111111', teammateId: 'researcher@crew', name: 'researcher', description: 'research', type: 'teammate', status: 'idle' as const }

// What ListAgents answered on a real machine (2.1.29x): other local sessions under `sessions`.
const LISTING = {
  listing: '',
  sections: [
    { kind: 'sessions', total: 1, rows: [{ name: 'maistarter-41', ref: 'ab12cd', type: 'interactive', status: 'idle' }] },
    { kind: 'subagents', total: 1, rows: [{ name: 'reviewer', id: SUBAGENT.id }] },
  ],
}

// A maiTerm tab: the link's three variables, a Claude new enough for the mod, two agents of its
// own, a ListAgents beneath that lists one other session, and an engine that delivers whatever
// reaches it.
function inMaitermTab(on: On, delivered: string[]) {
  mock.env(on, { MAITERM_TAB_ID: 'tab-1', MAITERM_PORT: '1', MAITERM_AUTH: 'x' })
  on('env.set', () => ({ value: undefined })) // the mod marks the tab as its own (MAITERM_VIA_MOD)
  on('session.version', () => ({ value: { version: '2.1.296' } }))
  // What the mod's PreToolUse relay reads before it posts (the post itself fails quietly here).
  on('session.id', () => ({ value: 'session-1' }))
  on('session.cwd', () => ({ value: '/tmp' }))
  on('agent.list', () => ({ value: [SUBAGENT, TEAMMATE] }))
  on('tool.call', { tool: 'ListAgents' }, () => ({ result: LISTING }))
  on('session.send', (_$, e) => {
    delivered.push(e.to)
    return { isDelivered: true }
  })
}

test('a session ListAgents listed is refused, by name and by name [ref], with the way to the mesh', async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  await $.tool.call({ tool: 'ListAgents' })
  for (const to of ['maistarter-41', 'maistarter-41 [ab12cd]']) {
    const r = await $.session.send({ to, text: 'hi' })
    expect(r.isDelivered).toBe(false)
    expect(r.reason).toContain('sendToBridgedAgent')
  }
  expect(delivered).toEqual([])
})

test("a session's socket or bridge address is refused without any listing", async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  for (const to of ['uds:/tmp/cc-socks/21790.sock', 'bridge:session_01abc']) {
    expect((await $.session.send({ to, text: 'hi' })).isDelivered).toBe(false)
  }
  expect(delivered).toEqual([])
})

test("the session's own agents, its parent and its team lead are delivered", async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  await $.tool.call({ tool: 'ListAgents' })
  const to = [SUBAGENT.id, 'reviewer', 'main', 'team-lead', 'researcher@crew', 'researcher']
  for (const t of to) expect((await $.session.send({ to: t, text: 'go on' })).isDelivered).toBe(true)
  expect(delivered).toEqual(to)
})

test('a name nothing proves is a session is delivered — a finished subagent the list dropped', async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  await $.tool.call({ tool: 'ListAgents' })
  expect((await $.session.send({ to: 'a9999999999999999', text: 'also fix X' })).isDelivered).toBe(true)
  expect(delivered).toEqual(['a9999999999999999'])
})

test('outside a maiTerm tab nothing is refused', async ($, on) => {
  const delivered: string[] = []
  on('session.version', () => ({ value: { version: '2.1.296' } }))
  on('agent.list', () => ({ value: [] }))
  on('session.send', (_$, e) => {
    delivered.push(e.to)
    return { isDelivered: true }
  })
  expect((await $.session.send({ to: 'uds:/tmp/cc-socks/21790.sock', text: 'hi' })).isDelivered).toBe(true)
  expect(delivered).toEqual(['uds:/tmp/cc-socks/21790.sock'])
})
