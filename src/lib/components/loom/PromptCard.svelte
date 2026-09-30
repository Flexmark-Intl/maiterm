<!--
  The prompt a tab's agent is stopped at, answerable in place (docs/loom.md): a tool permission
  (the dialog's own rows, read off the screen by mailink/permission.rs), an AskUserQuestion, or
  Claude's workspace-trust dialog. Answers go through the phone's responder as the human
  (`answerTabPromptAsHuman`), with its `prompt_id` stale guard.

  The same two click guards as BlockerCard: nothing is answered in a prompt's first 1.5 s on
  screen (a card that appears under the pointer takes no click meant for what was there), and
  the second click of a double-click is never an answer.
-->
<script lang="ts">
  import { answerTabPromptAsHuman, getTabPrompt, type PromptAnswer, type TabPrompt } from '$lib/tauri/commands';
  import { info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';

  interface Props {
    tabId: string;
    /** Poll only while shown. */
    active: boolean;
    /** Something changed that may have opened or closed a prompt (the agent's state). */
    pulse: unknown;
  }
  let { tabId, active, pulse }: Props = $props();

  interface Question {
    question: string;
    header?: string;
    multiSelect?: boolean;
    options?: { label: string; description?: string }[];
  }

  let prompt = $state<TabPrompt | null>(null);
  /** When this card first showed the current prompt, by prompt_id. */
  let seenAt = $state<{ id: string; at: number }>({ id: '', at: 0 });
  let note = $state('');
  let sending = $state(false);

  async function refresh() {
    const forTab = tabId;
    const p = await getTabPrompt(forTab).catch(() => null);
    // A read started for the previous chat must not paint its prompt into this one.
    if (forTab !== tabId) return;
    prompt = p;
    if (p && p.prompt_id !== seenAt.id) {
      seenAt = { id: p.prompt_id, at: Date.now() };
      picks = {};
      others = {};
    }
  }

  // A new chat starts clean; the prompt is polled while shown.
  $effect(() => {
    void tabId;
    prompt = null;
    note = '';
    if (!active) return;
    void refresh();
    const t = setInterval(refresh, 2000);
    return () => clearInterval(t);
  });
  // And read at once when the agent's state moves, rather than on the next poll.
  $effect(() => {
    void pulse;
    if (active) void refresh();
  });

  const FRESH_MS = 1500;
  const guarded = (e: MouseEvent) => e.detail > 1 || Date.now() - seenAt.at < FRESH_MS;

  const questions = $derived<Question[]>(
    prompt?.kind === 'question' && Array.isArray(prompt.questions) ? (prompt.questions as Question[]) : [],
  );
  /** Chosen labels and Other text per question index, for the prompt on screen. */
  let picks = $state<Record<number, string[]>>({});
  let others = $state<Record<number, string>>({});

  function toggle(qi: number, label: string, multi: boolean) {
    const cur = picks[qi] ?? [];
    picks = { ...picks, [qi]: multi ? (cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]) : [label] };
    if (!multi) others = { ...others, [qi]: '' };
  }
  const complete = $derived(
    questions.length > 0 && questions.every((_, i) => (picks[i]?.length ?? 0) > 0 || (others[i] ?? '').trim()),
  );

  /** An answer maiTerm hasn't replied to by now is reported rather than left spinning. */
  const REPLY_MS = 10_000;

  async function answer(e: MouseEvent, choice: string | null, answers: PromptAnswer[] | null) {
    if (!prompt || e.detail > 1) return;
    // Every click is logged with what became of it: "the answer didn't go through" must be
    // traceable from the log alone.
    if (sending) {
      logInfo(`Loom prompt card: click on ${tabId} ignored, an answer is still being sent`);
      note = 'Still sending the last answer…';
      return;
    }
    // Said, not swallowed: a click that did nothing and said nothing reads as a broken card.
    if (guarded(e)) {
      logInfo(`Loom prompt card: click on ${tabId} ignored, prompt ${prompt.prompt_id} appeared under 1.5 s ago`);
      note = 'That prompt only just appeared. Check it, then click again.';
      return;
    }
    sending = true;
    note = 'Sending…';
    const forTab = tabId;
    const pid = prompt.prompt_id;
    logInfo(`Loom prompt card: answering ${prompt.kind} ${pid} on ${forTab} with ${choice ?? 'the question answers'}`);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const r = await Promise.race([
        answerTabPromptAsHuman(forTab, pid, choice, answers),
        new Promise<never>((_, rej) => { timer = setTimeout(() => rej(new Error('no reply from maiTerm after 10 s')), REPLY_MS); }),
      ]);
      logInfo(`Loom prompt card: ${pid} on ${forTab} → ${JSON.stringify(r)}`);
      if (forTab !== tabId) return;
      note = r.ok
        ? 'Sent.'
        : r.reason === 'stale'
          ? (r.detail ? `Not sent: ${r.detail}.` : 'Not sent: that prompt has changed. Here is the current one.')
          : `Not sent (${r.detail ?? r.reason ?? 'unknown'}).`;
    } catch (err) {
      logWarn(`Loom prompt card: ${pid} on ${forTab} failed: ${err}`);
      if (forTab === tabId) note = `Not sent: ${err instanceof Error ? err.message : err}. Open the tab to answer it.`;
    } finally {
      clearTimeout(timer);
      sending = false;
      void refresh();
    }
  }

  function submitQuestions(e: MouseEvent) {
    const answers = questions.map((q, i) => {
      const other = (others[i] ?? '').trim();
      return { selected: other && !q.multiSelect ? [] : (picks[i] ?? []), other: other || null };
    });
    void answer(e, null, answers);
  }

  /** A refusal is styled as one by its label, the way the phone does. */
  const refusal = (label: string) => /^(no\b|deny|tell claude)/i.test(label.trim());
</script>

{#if prompt}
  <section class="prompt" data-kind={prompt.kind} aria-live="polite">
    {#if prompt.kind === 'trust'}
      <h5>Trust this folder?</h5>
      <p class="what">{prompt.path}</p>
      <p class="why">Trusting a folder lets the agent read, edit and run everything in it.</p>
      <div class="opts">
        {#each prompt.options ?? [] as o (o)}
          <button class:no={refusal(o)} disabled={sending} onclick={(e) => answer(e, o, null)}>{o}</button>
        {/each}
      </div>
    {:else if prompt.kind === 'permission'}
      <h5>Wants to run</h5>
      <p class="what">{prompt.tool ?? 'A tool'}{prompt.detail ? `: ${prompt.detail}` : ''}</p>
      <div class="opts">
        {#each prompt.options ?? ['Yes', 'No'] as o (o)}
          <button class:no={refusal(o)} disabled={sending} onclick={(e) => answer(e, o, null)}>{o}</button>
        {/each}
      </div>
    {:else if questions.length}
      {#each questions as q, qi (qi)}
        <div class="q">
          {#if q.header}<span class="hdr">{q.header}</span>{/if}
          <h5>{q.question}</h5>
          <div class="opts col">
            {#each q.options ?? [] as o (o.label)}
              <button
                class="pick"
                aria-pressed={(picks[qi] ?? []).includes(o.label)}
                onclick={(e) => { if (e.detail <= 1) toggle(qi, o.label, !!q.multiSelect); }}
              >
                <b>{o.label}</b>{#if o.description}<span>{o.description}</span>{/if}
              </button>
            {/each}
            <input
              class="other"
              placeholder="Or answer in your own words"
              value={others[qi] ?? ''}
              oninput={(e) => {
                const v = (e.currentTarget as HTMLInputElement).value;
                others = { ...others, [qi]: v };
                if (v.trim() && !q.multiSelect) picks = { ...picks, [qi]: [] };
              }}
            />
          </div>
        </div>
      {/each}
      <div class="opts">
        <button class="send" disabled={!complete || sending} onclick={submitQuestions}>Send answer{questions.length > 1 ? 's' : ''}</button>
      </div>
    {:else}
      <h5>The agent is asking a question</h5>
      <p class="why">Its options couldn't be read. Open the tab to answer it.</p>
    {/if}
    {#if note}<p class="note">{note}</p>{/if}
  </section>
{:else if note}
  <p class="note">{note}</p>
{/if}

<style>
  .prompt {
    --c: var(--red);
    display: flex;
    flex-direction: column;
    gap: 8px;
    border: 1px solid color-mix(in srgb, var(--c) 45%, var(--bg-light));
    border-left: 3px solid var(--c);
    background: color-mix(in srgb, var(--c) 6%, var(--bg-medium));
    border-radius: 8px;
    padding: 10px 12px;
  }
  .prompt[data-kind='question'] { --c: var(--orange, #ff9e64); }
  .prompt[data-kind='trust'] { --c: var(--yellow); }
  h5 { margin: 0; font-size: 12.5px; font-weight: 600; overflow-wrap: anywhere; }
  .hdr { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--c); }
  .what { margin: 0; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11.5px; overflow-wrap: anywhere; white-space: pre-wrap; }
  .why, .note { margin: 0; font-size: 11.5px; color: var(--fg-dim); }
  .q { display: flex; flex-direction: column; gap: 6px; }
  .opts { display: flex; flex-wrap: wrap; gap: 6px; }
  .opts.col { flex-direction: column; }
  .opts button {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    color: var(--fg);
    border-radius: 6px;
    padding: 5px 11px;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .opts button:hover:not(:disabled) { border-color: var(--accent); }
  .opts button:disabled { opacity: 0.5; cursor: default; }
  .opts button.no { color: var(--red); }
  .pick { display: flex; flex-direction: column; gap: 2px; }
  .pick span { font-size: 11px; color: var(--fg-dim); }
  .pick[aria-pressed='true'] { border-color: var(--c); box-shadow: 0 0 0 1px var(--c); }
  .send { background: var(--accent) !important; color: var(--bg-dark) !important; border-color: var(--accent) !important; font-weight: 600; }
  .other {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    padding: 5px 9px;
  }
</style>
