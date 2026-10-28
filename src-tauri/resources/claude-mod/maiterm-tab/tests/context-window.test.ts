// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

// A maiTerm tab whose session Claude Code sizes at `window` (undefined: usage can't say).
function tab(on: On, posted: Record<string, unknown>[], window: number | undefined) {
  mock.env(on, { MAITERM_TAB_ID: 'tab-1', MAITERM_PORT: '1', MAITERM_AUTH: 'x' })
  on('env.set', () => ({ value: undefined }))
  on('session.version', () => ({ value: { version: '2.1.312' } }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.model', () => ({ value: 'claude-sonnet-5-5' }))
  on('session.usage', () => {
    if (window === undefined) throw new Error('no session bound')
    return { value: { startedAt: 0, context: { tokens: 30795, window, percent: 3 } } }
  })
  on('http.fetch', (_$, e) => {
    const ok = (status: number) => ({ value: { status, ok: status < 300, headers: {}, text: '' } })
    if (e.url.includes('/hooks?')) {
      posted.push(JSON.parse(String(e.init?.body)))
      return ok(200)
    }
    return ok(204)
  })
  // What runs beneath the mod: the settings hooks, which decide nothing here.
  on('classic.Stop', () => ({}))
}

test('a turn end tells maiTerm the window Claude Code sizes the model at, with the model', async ($, on) => {
  const posted: Record<string, unknown>[] = []
  tab(on, posted, 1_000_000)
  await $.classic.Stop({})
  const stop = posted.find(p => p.hook_event_name === 'Stop')
  expect(stop?.maiterm_context).toEqual({ model: 'claude-sonnet-5-5', window: 1_000_000 })
})

test('a turn end Claude Code can not size is still sent, just without a window', async ($, on) => {
  const posted: Record<string, unknown>[] = []
  tab(on, posted, undefined)
  await $.classic.Stop({})
  const stop = posted.find(p => p.hook_event_name === 'Stop')
  expect(stop).toBeDefined()
  expect(stop?.maiterm_context).toBeUndefined()
})
