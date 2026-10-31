<!--
  One of the gallery bar's dropdowns (docs/hive.md §10.4): a button that opens a short list to
  pick one from (Made by, Task, Status, Tags, Collection, Group). A long list gets a field to
  narrow it. Arrows move through the list, Enter picks, Escape closes back onto the button; a
  pick hands the keyboard back to the gallery, so Space previews again rather than reopening this.
-->
<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';

  interface Option {
    key: string;
    label: string;
    count?: number;
  }
  interface Props {
    label: string;
    options: Option[];
    value: string;
    onpick: (key: string) => void;
    /** Something other than the first option is picked: the button shows it's in force. */
    active?: boolean;
    /** Show the picked option's label on the button ("Group: Task"). */
    showValue?: boolean;
    /** Links under the list ("Manage collections…"). */
    footer?: { label: string; action: () => void }[];
    /** Said in place of the list when it has nothing but the first option. */
    empty?: string;
    /** A field to narrow the list appears from this many options. */
    searchAt?: number;
  }
  let { label, options, value, onpick, active = false, showValue = false, footer = [], empty = '', searchAt = 12 }: Props = $props();

  let open = $state(false);
  let filter = $state('');
  let btn = $state<HTMLButtonElement | null>(null);
  let pop = $state<HTMLDivElement | null>(null);
  let at = $state({ left: 0, top: 0, maxH: 300 });

  const searchable = $derived(options.length >= searchAt);
  const shown = $derived.by(() => {
    const f = filter.trim().toLowerCase();
    return f ? options.filter((o) => o.label.toLowerCase().includes(f)) : options;
  });
  const valueLabel = $derived(options.find((o) => o.key === value)?.label ?? '');

  function place() {
    if (!btn) return;
    const r = btn.getBoundingClientRect();
    const width = 240;
    const left = Math.max(8, Math.min(r.left, window.innerWidth - width - 8));
    at = { left, top: r.bottom + 4, maxH: Math.max(160, window.innerHeight - r.bottom - 16) };
  }

  function toggle() {
    if (open) return close(false);
    filter = '';
    place();
    open = true;
    requestAnimationFrame(() => {
      const target =
        pop?.querySelector<HTMLElement>('input') ??
        pop?.querySelector<HTMLElement>('[data-opt][aria-selected="true"]') ??
        pop?.querySelector<HTMLElement>('[data-opt]');
      target?.focus({ preventScroll: true });
    });
  }

  /** `toButton`: Escape goes back to the button; a pick goes back to the gallery. */
  function close(toButton: boolean) {
    open = false;
    if (toButton) btn?.focus({ preventScroll: true });
    else btn?.closest<HTMLElement>('[data-gallery]')?.focus({ preventScroll: true });
  }

  function pick(key: string) {
    onpick(key);
    close(false);
  }

  $effect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      const t = e.target as Node;
      if (pop?.contains(t) || btn?.contains(t)) return;
      open = false;
    };
    const onResize = () => (open = false);
    window.addEventListener('mousedown', down, true);
    window.addEventListener('resize', onResize);
    return () => {
      window.removeEventListener('mousedown', down, true);
      window.removeEventListener('resize', onResize);
    };
  });

  function onPopKey(e: KeyboardEvent) {
    const opts = [...(pop?.querySelectorAll<HTMLElement>('[data-opt], [data-foot]') ?? [])];
    const i = opts.indexOf(document.activeElement as HTMLElement);
    const go = (j: number) => opts[Math.max(0, Math.min(opts.length - 1, j))]?.focus();
    switch (e.key) {
      case 'Escape':
        e.preventDefault();
        e.stopPropagation();
        close(true);
        return;
      case 'ArrowDown':
        go(i + 1);
        break;
      case 'ArrowUp':
        if (i <= 0) pop?.querySelector<HTMLElement>('input')?.focus();
        else go(i - 1);
        break;
      case 'Home':
        go(0);
        break;
      case 'End':
        go(opts.length - 1);
        break;
      case 'Enter':
        // In the field: the only match, or the first.
        if ((e.target as HTMLElement).tagName === 'INPUT' && shown.length) pick(shown[0].key);
        else return;
        break;
      case 'Tab':
        open = false;
        return;
      default:
        // Typing goes to the field; nothing here is the gallery's bare key.
        e.stopPropagation();
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<button
  type="button"
  class="fbtn"
  class:active
  class:open
  bind:this={btn}
  aria-haspopup="listbox"
  aria-expanded={open}
  onmousedown={(e) => e.preventDefault()}
  onclick={toggle}
>
  <span>{label}{#if showValue && valueLabel}<span class="val">: {valueLabel}</span>{/if}</span>
  <span class="caret" aria-hidden="true"><Icon name="chevron" size={9} /></span>
</button>

{#if open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="pop" bind:this={pop} style="left: {at.left}px; top: {at.top}px; max-height: {at.maxH}px" onkeydown={onPopKey}>
    {#if searchable}
      <input class="narrow" type="text" placeholder="Filter {label.toLowerCase()}" bind:value={filter} aria-label="Filter {label.toLowerCase()}" />
    {/if}
    <div class="list" role="listbox" aria-label={label}>
      {#each shown as o (o.key)}
        <button type="button" role="option" data-opt aria-selected={o.key === value} class:on={o.key === value} onclick={() => pick(o.key)}>
          <span class="tick" aria-hidden="true">{#if o.key === value}<Icon name="check" size={10} />{/if}</span>
          <span class="ol">{o.label}</span>
          {#if o.count}<em>{o.count}</em>{/if}
        </button>
      {/each}
      {#if !shown.length}<p class="none">Nothing matches.</p>{/if}
      {#if empty && options.length <= 1}<p class="none">{empty}</p>{/if}
    </div>
    {#if footer.length}
      <div class="foot">
        {#each footer as f (f.label)}
          <button type="button" data-foot onclick={() => { open = false; f.action(); }}>{f.label}</button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .fbtn { display: inline-flex; align-items: center; gap: 5px; height: 24px; box-sizing: border-box; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 0 7px 0 8px; font: inherit; font-size: 11.5px; cursor: pointer; white-space: nowrap; }
  .fbtn:hover, .fbtn.open { color: var(--fg); }
  .fbtn.active { color: var(--fg); border-color: color-mix(in srgb, var(--accent) 70%, var(--bg-light)); background: color-mix(in srgb, var(--accent) 12%, var(--bg-medium)); }
  .val { color: var(--fg); }
  .caret { display: inline-flex; transform: rotate(90deg); opacity: 0.7; }
  .pop { position: fixed; z-index: 1000; width: 240px; box-sizing: border-box; display: flex; flex-direction: column; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.4); padding: 4px; font-size: 12px; }
  .narrow { margin: 2px 2px 4px; background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 4px; color: var(--fg); font: inherit; padding: 3px 7px; }
  .narrow:focus { outline: none; border-color: var(--accent); }
  .list { overflow-y: auto; min-height: 0; display: flex; flex-direction: column; }
  .list button { display: flex; align-items: center; gap: 6px; width: 100%; background: none; border: none; border-radius: 4px; padding: 5px 8px 5px 4px; color: var(--fg); font: inherit; text-align: left; cursor: pointer; }
  .list button:hover, .list button:focus-visible { background: var(--bg-light); outline: none; }
  .tick { flex: none; width: 12px; display: inline-flex; color: var(--accent); }
  .ol { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  em { font-style: normal; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .none { margin: 0; padding: 6px 8px; color: var(--fg-dim); font-size: 11.5px; }
  .foot { border-top: 1px solid var(--bg-light); margin-top: 4px; padding-top: 4px; display: flex; flex-direction: column; }
  .foot button { background: none; border: none; border-radius: 4px; padding: 5px 8px 5px 22px; color: var(--accent); font: inherit; text-align: left; cursor: pointer; }
  .foot button:hover, .foot button:focus-visible { background: var(--bg-light); outline: none; }
  .fbtn:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
