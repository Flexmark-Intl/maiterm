// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const SUBAGENT = { id: 'a0123456789abcdef', name: 'reviewer', description: 'review', type: 'general-purpose', status: 'running' as const }
const TEAMMATE = { id: 'a1111111111111111', teammateId: 'researcher@crew', name: 'researcher', description: 'research', type: 'teammate', status: 'idle' as const }

// A maiTerm tab: the link's three variables, a Claude new enough for the mod, two agents of its
// own, and an engine beneath that delivers whatever reaches it.
function inMaitermTab(on: On, delivered: string[]) {
  mock.env(on, { MAITERM_TAB_ID: 'tab-1', MAITERM_PORT: '1', MAITERM_AUTH: 'x' })
  on('env.set', () => ({ value: undefined })) // the mod marks the tab as its own (MAITERM_VIA_MOD)
  on('session.version', () => ({ value: { version: '2.1.296' } }))
  on('agent.list', () => ({ value: [SUBAGENT, TEAMMATE] }))
  on('session.send', (_$, e) => {
    delivered.push(e.to)
    return { isDelivered: true }
  })
}

test('another session is refused, with the way to the mesh', async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  const r = await $.session.send({ to: 'maistarter-41', text: 'hi' })
  expect(r.isDelivered).toBe(false)
  expect(r.reason).toContain('sendToBridgedAgent')
  expect(delivered).toEqual([])
})

test('a socket address is another session too', async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  const r = await $.session.send({ to: 'uds:/tmp/cc-socks/21790.sock', text: 'hi' })
  expect(r.isDelivered).toBe(false)
  expect(delivered).toEqual([])
})

test("the session's own subagent, by id or by name, and its parent are delivered", async ($, on) => {
  const delivered: string[] = []
  inMaitermTab(on, delivered)
  for (const to of [SUBAGENT.id, 'reviewer', 'main', 'researcher@crew', 'researcher']) {
    expect((await $.session.send({ to, text: 'go on' })).isDelivered).toBe(true)
  }
  expect(delivered).toEqual([SUBAGENT.id, 'reviewer', 'main', 'researcher@crew', 'researcher'])
})

test('outside a maiTerm tab nothing is refused', async ($, on) => {
  const delivered: string[] = []
  on('session.version', () => ({ value: { version: '2.1.296' } }))
  on('agent.list', () => ({ value: [] }))
  on('session.send', (_$, e) => {
    delivered.push(e.to)
    return { isDelivered: true }
  })
  expect((await $.session.send({ to: 'maistarter-41', text: 'hi' })).isDelivered).toBe(true)
  expect(delivered).toEqual(['maistarter-41'])
})
