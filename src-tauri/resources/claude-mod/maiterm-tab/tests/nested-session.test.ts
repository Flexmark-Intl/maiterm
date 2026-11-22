// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'

// A Claude in a maiTerm tab. `owner` is MAITERM_MOD_SESSION as this process inherited it (the
// tab's interactive agent sets it to its own session id), `surfaces` where this session draws.
function tab(on: On, posted: string[], owner: string | undefined, surfaces: string[], bodies: Record<string, unknown>[] = []) {
  on('env.get', (_$, e) => {
    const env: Record<string, string | undefined> = {
      MAITERM_TAB_ID: 'tab-1',
      MAITERM_PORT: '1',
      MAITERM_AUTH: 'x',
      MAITERM_MOD_SESSION: owner,
    }
    return { value: env[e.name] }
  })
  on('env.set', () => ({ value: undefined }))
  on('session.version', () => ({ value: { version: '2.1.330' } }))
  on('session.id', () => ({ value: 'session-child' }))
  on('session.surfaces', () => ({ value: surfaces }))
  on('http.fetch', (_$, e) => {
    if (e.url.includes('/hooks?')) {
      posted.push(e.url)
      bodies.push(JSON.parse(String(e.init?.body)))
    }
    return { value: { status: 204, ok: true, headers: {}, text: '' } }
  })
  on('classic.SessionStart', () => ({}))
  on('classic.Stop', () => ({}))
}

const start = { hook_event_name: 'SessionStart', session_id: 'session-child', source: 'startup' }

test('a claude -p the tab agent runs does not claim the tab, and says it is a child run', async ($, on) => {
  const posted: string[] = []
  const bodies: Record<string, unknown>[] = []
  tab(on, posted, 'session-parent', [], bodies)
  await $.classic.SessionStart(start)
  await $.classic.Stop({})
  // Only its SessionStart goes, naming no tab, so maiTerm drops the settings hook's anonymous copy;
  // under an event name an older maiTerm doesn't know, so it can't pool it as a SessionStart.
  expect(posted.length).toBe(1)
  expect(posted[0]).toContain('nested=1')
  expect(posted[0]).toContain('tab_id=&')
  expect(bodies[0]?.hook_event_name).toBe('MaitermChildRun')
  expect(bodies[0]?.child_of_event).toBe('SessionStart')
  expect(bodies[0]?.session_id).toBe('session-child')
})

test('the tab agent itself, started fresh, claims its tab', async ($, on) => {
  const posted: string[] = []
  tab(on, posted, undefined, ['terminal'])
  await $.classic.SessionStart(start)
  expect(posted.length).toBe(1)
  expect(posted[0]).toContain('tab_id=tab-1')
})

test('a REPL holding another session id still claims its tab', async ($, on) => {
  const posted: string[] = []
  tab(on, posted, 'session-parent', ['terminal'])
  await $.classic.SessionStart(start)
  expect(posted.length).toBe(1)
})

test('a headless run started straight from the tab shell still claims its tab', async ($, on) => {
  const posted: string[] = []
  tab(on, posted, undefined, [])
  await $.classic.SessionStart(start)
  expect(posted.length).toBe(1)
})
