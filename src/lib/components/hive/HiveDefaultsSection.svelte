<!--
  Preferences → Hive: defaults for new Hives (docs/hive.md §3). The Make this a Hive wizard starts
  from these when there's no draft; each Hive can still change them. `hive_defaults` is not in
  `preference_meta`, so no agent can change what a new Hive may spend or use.
-->
<script lang="ts">
  import { error as logError } from '@tauri-apps/plugin-log';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { hiveDetectTools } from '$lib/tauri/commands';
  import type { HiveDefaults, HiveDetectedTool } from '$lib/tauri/types';
  import { BUILTIN, clampDefault, packsToStore, HIVE_EFFORTS, HIVE_MODELS, MAX_BUDGET, wizardStart } from '$lib/hive/defaults';
  import { MAX_CAP } from '$lib/hive/change';
  import { pickerTools } from '$lib/hive/media';
  import { MAX_INSTRUCTIONS, charCount } from '$lib/hive/model';
  import HiveToolPicker from './HiveToolPicker.svelte';

  const d = $derived(preferencesStore.hiveDefaults);
  /** What the wizard will show: an unset field reads as its built-in. */
  const start = $derived(wizardStart(d));

  let tools = $state<HiveDetectedTool[] | null>(null);
  hiveDetectTools().then((t) => (tools = pickerTools(t))).catch((e) => { tools = []; logError(`hive defaults: tool detection failed: ${e}`); });

  // A stored pack counts its members detected since as picked, as the wizard does.
  const picked = $derived(new Set([...d.tools, ...(tools ?? []).filter((t) => t.pack && d.packs.includes(t.pack)).map((t) => t.id)]));
  /** Saved picks this computer doesn't have: kept (they may come back), and said. */
  const missing = $derived(tools ? d.tools.filter((id) => !tools!.some((t) => t.id === id)) : []);

  /** A write that failed is rolled back by the store; this says so. */
  let saveError = $state<string | null>(null);
  async function save(patch: Partial<HiveDefaults>) {
    saveError = null;
    try {
      await preferencesStore.setHiveDefaults(patch);
    } catch (e) {
      saveError = `Couldn't save that: ${String(e).replace(/^Error: /, '')}`;
      logError(`hive defaults: save failed: ${e}`);
    }
  }

  /** Within bounds (Rust keeps a u32), and the field shows what was kept. */
  async function setNumber(el: HTMLInputElement, field: 'cap' | 'budget_dollars') {
    const s = el.value.trim();
    const v = s ? (field === 'cap' ? clampDefault(Number(s), 1, MAX_CAP) : clampDefault(Number(s), 0, MAX_BUDGET)) : null;
    if (s && v === null) { el.value = String(d[field] ?? ''); return; }
    await save({ [field]: v });
    el.value = String(preferencesStore.hiveDefaults[field] ?? '');
  }

  // ── Rules: typed into a local draft, saved shortly after typing stops and on blur. Closing
  // the Preferences window doesn't blur the field first, so a close flushes too (as the
  // playbook and the comms fields do). ──
  let rulesDraft = $state(preferencesStore.hiveDefaults.rules);
  let rulesFocused = false;
  let rulesTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const stored = preferencesStore.hiveDefaults.rules;
    if (!rulesFocused) rulesDraft = stored;
  });
  function flushRules() {
    if (rulesTimer) { clearTimeout(rulesTimer); rulesTimer = null; }
    if (rulesDraft !== preferencesStore.hiveDefaults.rules) void save({ rules: rulesDraft });
  }
  function onRulesInput() {
    if (rulesTimer) clearTimeout(rulesTimer);
    rulesTimer = setTimeout(flushRules, 400);
  }
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' || (e.key === 'w' && (e.metaKey || e.ctrlKey))) flushRules();
    };
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('pagehide', flushRules);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('pagehide', flushRules);
      flushRules();
    };
  });
  const rulesCount = $derived(charCount(rulesDraft));
</script>

<div class="hive-defaults">
  <h3 class="heading">Defaults for new Hives</h3>
  <p class="desc">The Make this a Hive wizard starts from these. Each Hive can still change them.</p>
  {#if saveError}<p class="error" role="alert">{saveError}</p>{/if}

  <div class="block">
    <div class="copy">
      <span class="label">Tools</span>
      <p class="hint">Ticked tools start ticked in the wizard. A tool this computer doesn't have is skipped there.</p>
    </div>
    {#if tools === null}
      <p class="hint">Looking for tools…</p>
    {:else if tools.length === 0}
      <p class="hint">Nothing detected: no MCP servers, ffmpeg, ImageMagick, SoX, Blender, skills or ComfyUI.</p>
    {:else}
      <!-- A pack picked whole is stored by name too, so a skill it adds later comes with it. -->
      <HiveToolPicker {tools} {picked} onchange={(next) => void save({ tools: [...next], packs: packsToStore(next, tools!, d.packs) })} />
    {/if}
    {#if missing.length}
      <p class="hint">
        {missing.length} saved tool{missing.length === 1 ? " isn't" : "s aren't"} detected on this computer.
        <button type="button" class="link" onclick={() => void save({ tools: d.tools.filter((id) => !missing.includes(id)) })}>Remove {missing.length === 1 ? 'it' : 'them'}</button>
      </p>
    {/if}
  </div>

  <div class="block">
    <span class="label">Models</span>
    <div class="models">
      <span>Queen</span>
      <select aria-label="Queen model" value={start.coordModel} onchange={(e) => void save({ coordinator_model: e.currentTarget.value })}>
        {#each HIVE_MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}
      </select>
      <select aria-label="Queen effort" value={start.coordEffort} onchange={(e) => void save({ coordinator_effort: e.currentTarget.value })}>
        {#each HIVE_EFFORTS as x (x)}<option value={x}>{x} effort</option>{/each}
      </select>
      <span>Workers</span>
      <select aria-label="Worker model" value={start.workerModel} onchange={(e) => void save({ worker_model: e.currentTarget.value })}>
        {#each HIVE_MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}
      </select>
      <select aria-label="Worker effort" value={start.workerEffort} onchange={(e) => void save({ worker_effort: e.currentTarget.value })}>
        {#each HIVE_EFFORTS as x (x)}<option value={x}>{x} effort</option>{/each}
      </select>
    </div>
  </div>

  <div class="row">
    <div class="copy">
      <label for="hive-def-cap" class="label">Workers at once</label>
      <p class="hint">At most this many workers run at a time. Leave it empty for maiTerm's own default.</p>
    </div>
    <input id="hive-def-cap" class="num" type="number" min="1" max={MAX_CAP} placeholder="default" value={d.cap ?? ''} onchange={(e) => void setNumber(e.currentTarget,'cap')} />
  </div>

  <div class="row">
    <div class="copy">
      <label for="hive-def-budget" class="label">Budget for paid generation</label>
      <p class="hint">Dollars a Hive may spend on paid generation. Empty is ${BUILTIN.budgetDollars}.</p>
    </div>
    <div class="inline"><span>$</span><input id="hive-def-budget" class="num" type="number" min="0" max={MAX_BUDGET} step="1" placeholder="10" value={d.budget_dollars ?? ''} onchange={(e) => void setNumber(e.currentTarget,'budget_dollars')} /></div>
  </div>

  <div class="row">
    <div class="copy">
      <label for="hive-def-phone" class="label">Chats on your phone</label>
      <p class="hint">Which of a Hive's agents are chats in maiLink.</p>
    </div>
    <select id="hive-def-phone" value={start.phone} onchange={(e) => void save({ phone: e.currentTarget.value })}>
      <option value="coordinator">The queen only</option>
      <option value="all">The queen and its workers</option>
      <option value="none">Nobody</option>
    </select>
  </div>

  <div class="row">
    <div class="copy">
      <label for="hive-def-push" class="label">Push after each land</label>
      <p class="hint">Each land to Base also pushes that repo's Base. Only for Hives that land straight into Base.</p>
    </div>
    <button
      id="hive-def-push"
      class="toggle"
      class:active={d.push_after_land}
      onclick={() => void save({ push_after_land: !d.push_after_land })}
      aria-pressed={d.push_after_land}
      aria-label="Toggle push after each land"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>

  <div class="block">
    <div class="copy">
      <label for="hive-def-rules" class="label">Rules for every agent</label>
      <p class="hint">Prefilled in the wizard. Every worker gets these too, so keep them short.</p>
    </div>
    <textarea
      id="hive-def-rules"
      rows="4"
      placeholder="Rules for all work, whatever the task: “use pnpm”, “don't touch legacy/”, “British English”"
      bind:value={rulesDraft}
      oninput={onRulesInput}
      onfocus={() => (rulesFocused = true)}
      onblur={() => { rulesFocused = false; flushRules(); }}
    ></textarea>
    <span class="hint count" class:over={rulesCount > MAX_INSTRUCTIONS}>{rulesCount.toLocaleString()} / 16,000 characters</span>
  </div>
</div>

<style>
  .hive-defaults { display: flex; flex-direction: column; gap: 18px; max-width: 720px; }
  .heading {
    font-size: 0.846rem;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: 0;
    padding: 6px 0 6px 10px;
    border-left: 2px solid var(--accent);
  }
  .desc { font-size: 0.923rem; color: var(--fg-dim); margin: -8px 0 0; line-height: 1.5; }
  .block { display: flex; flex-direction: column; gap: 8px; min-width: 0; }
  .row { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; }
  .copy { min-width: 0; flex: 1; }
  .label { font-size: 1rem; color: var(--fg); }
  .hint { font-size: 0.846rem; color: var(--fg-dim); line-height: 1.5; margin: 3px 0 0; overflow-wrap: anywhere; max-width: 62ch; }
  .error { margin: 0; font-size: 0.923rem; color: var(--red, #f7768e); overflow-wrap: anywhere; }
  .count { align-self: flex-end; font-variant-numeric: tabular-nums; margin: 0; }
  .count.over { color: var(--red, #f7768e); }
  .models { display: grid; grid-template-columns: auto 1fr 1fr; gap: 6px 8px; align-items: center; font-size: 0.923rem; color: var(--fg); max-width: 480px; }
  .inline { display: flex; align-items: center; gap: 6px; color: var(--fg); }
  select, .num, textarea {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font: inherit;
    font-size: 0.923rem;
    padding: 6px 8px;
    min-width: 0;
  }
  select { cursor: pointer; }
  select:hover { border-color: var(--accent); }
  select:focus, .num:focus, textarea:focus { outline: none; border-color: var(--accent); }
  .num { width: 80px; }
  textarea { width: 100%; box-sizing: border-box; resize: vertical; line-height: 1.45; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: inherit; }
  .link:hover { text-decoration: underline; }
  .toggle {
    position: relative;
    width: 40px;
    height: 22px;
    background: var(--bg-light);
    border-radius: 11px;
    border: none;
    cursor: pointer;
    flex-shrink: 0;
    margin-top: 1px;
    transition: background-color 0.2s;
  }
  .toggle.active { background: var(--accent); }
  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    background: white;
    border-radius: 50%;
    transition: transform 0.2s;
  }
  .toggle.active .toggle-knob { transform: translateX(18px); }
  @media (prefers-reduced-motion: reduce) {
    .toggle, .toggle-knob { transition: none; }
  }
</style>
