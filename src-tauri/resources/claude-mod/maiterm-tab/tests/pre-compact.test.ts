// `claude plugin test src-tauri/resources/claude-mod/maiterm-tab`. Not shipped: claude_mod.rs
// embeds only the manifest, hooks.json and register.ts.
import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

// A maiTerm tab whose server answers every event with `reply`; `urls` collects where events went
// and `beneath` counts the settings hooks that ran.
function tab(on: On, urls: string[], reply: string, beneath: { runs: number }, modSession = 'session-1') {
  mock.env(on, { MAITERM_TAB_ID: 'tab-1', MAITERM_PORT: '1', MAITERM_AUTH: 'x', MAITERM_MOD_SESSION: modSession })
  on('env.set', () => ({ value: undefined }))
  on('session.version', () => ({ value: { version: '2.1.312' } }))
  on('session.id', () => ({ value: 'session-1' }))
  on('http.fetch', (_$, e) => {
    if (e.url.includes('/hooks?')) {
      urls.push(e.url)
      return { value: { status: 200, ok: true, headers: {}, text: reply } }
    }
    return { value: { status: 204, ok: true, headers: {}, text: '' } }
  })
  on('classic.PreCompact', () => {
    beneath.runs += 1
    return {}
  })
}

const BLOCK = JSON.stringify({ decision: 'block', reason: 'maiTerm is preparing this session first' })

test('an automatic compaction maiTerm wants to prepare for is blocked', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, BLOCK, beneath)
  const result = await $.classic.PreCompact({ trigger: 'auto', custom_instructions: null })
  expect(result.block).toBe('maiTerm is preparing this session first')
  expect(urls.some(u => u.includes('&prepare=1'))).toBe(true)
  expect(beneath.runs).toBe(0)
})

test('an empty answer lets the automatic compaction go ahead', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, '', beneath)
  const result = await $.classic.PreCompact({ trigger: 'auto', custom_instructions: null })
  expect(result.block).toBeUndefined()
  expect(urls.some(u => u.includes('&prepare=1'))).toBe(true)
  expect(beneath.runs).toBe(1)
})

test('a reply that is not JSON decides nothing', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, 'not json', beneath)
  const result = await $.classic.PreCompact({ trigger: 'auto', custom_instructions: null })
  expect(result.block).toBeUndefined()
  expect(urls.some(u => u.includes('&prepare=1'))).toBe(true)
  expect(beneath.runs).toBe(1)
})

test('the human or the ritual compacting is never asked about or blocked', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, BLOCK, beneath)
  const result = await $.classic.PreCompact({ trigger: 'manual', custom_instructions: null })
  expect(result.block).toBeUndefined()
  expect(urls.length).toBeGreaterThan(0)
  expect(urls.some(u => u.includes('prepare'))).toBe(false)
  expect(beneath.runs).toBe(1)
})

test('a claude -p run inside the tab is never asked about or blocked', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, BLOCK, beneath, 'the-tabs-own-session')
  const result = await $.classic.PreCompact({ trigger: 'auto', custom_instructions: null })
  expect(result.block).toBeUndefined()
  expect(urls.length).toBeGreaterThan(0)
  expect(urls.some(u => u.includes('prepare'))).toBe(false)
})

test('a subagent compacting its own conversation is never asked about or blocked', async ($, on) => {
  const urls: string[] = []
  const beneath = { runs: 0 }
  tab(on, urls, BLOCK, beneath)
  const result = await $.classic.PreCompact({ trigger: 'auto', custom_instructions: null, agent_id: 'a1' })
  expect(result.block).toBeUndefined()
  expect(urls.length).toBeGreaterThan(0)
  expect(urls.some(u => u.includes('prepare'))).toBe(false)
})
