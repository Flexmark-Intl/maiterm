// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const QUESTIONS = [
  { question: 'Which path?', header: 'Path', multiSelect: false, options: [{ label: 'A', description: 'a' }, { label: 'B', description: 'b' }] },
]

// A maiTerm tab whose AskUserQuestion selector stays open (nobody at the desktop) while maiTerm
// answers it, as the phone does. Every event the mod posts is collected.
function phoneAnswers(on: On, posted: Record<string, unknown>[]) {
  mock.env(on, { MAITERM_TAB_ID: 'tab-1', MAITERM_PORT: '1', MAITERM_AUTH: 'x' })
  on('env.set', () => ({ value: undefined }))
  on('session.version', () => ({ value: { version: '2.1.296' } }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.cwd', () => ({ value: '/tmp' }))
  on('http.fetch', (_$, e) => {
    const ok = (status: number, text = '') => ({ value: { status, ok: status < 300, headers: {}, text } })
    if (e.url.includes('/hooks/ask?')) return ok(200, JSON.stringify({ answers: { 'Which path?': 'B' } }))
    if (e.url.includes('/hooks?')) {
      posted.push(JSON.parse(String(e.init?.body)))
      return ok(200)
    }
    return ok(404)
  })
  on('tool.call', { tool: 'AskUserQuestion' }, () => new Promise(() => {}))
}

test("an answer from maiTerm ends the call for maiTerm too: the selector's own PostToolUse never runs", async ($, on) => {
  const posted: Record<string, unknown>[] = []
  phoneAnswers(on, posted)
  const r = await $.tool.call({ tool: 'AskUserQuestion', questions: QUESTIONS })
  expect(JSON.stringify(r)).toContain('"Which path?":"B"')
  const events = posted.map(p => p.hook_event_name)
  expect(events).toEqual(['PreToolUse', 'PostToolUse'])
  const [pre, post] = posted
  expect(post.tool_name).toBe('AskUserQuestion')
  expect(post.session_id).toBe('session-1')
  // The call's own id, which the server's permission ledger ends it by.
  expect(post.tool_use_id).toBe(pre.tool_use_id)
})
