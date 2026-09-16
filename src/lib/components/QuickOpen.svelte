<script lang="ts">
  import { untrack } from 'svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { getPtyInfo, listFiles, sshListFiles, isDirectory, sshIsDirectory } from '$lib/tauri/commands';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    open: boolean;
    onclose: () => void;
    onselect: (filePath: string) => void;
  }

  let { open, onclose, onselect }: Props = $props();

  let query = $state('');
  let files = $state<string[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let selectedIndex = $state(0);
  let capped = $state(false);
  let basePath = $state('');
  let isRemote = $state(false);
  let showHidden = $state(false);
  let showIgnored = $state(false);
  let inputRef = $state<HTMLInputElement | null>(null);
  let listRef = $state<HTMLDivElement | null>(null);

  // Cache: reuse file list if same CWD and within TTL
  let cache: { cwd: string; files: string[]; capped: boolean; hidden: boolean; ignored: boolean; time: number } | null = null;
  const CACHE_TTL = 30_000;

  const MAX_FILES = 10_000;
  const MAX_RESULTS = 100;

  // ── File type icon colors ──────────────────────────────────────────
  const EXT_COLORS: Record<string, string> = {
    ts: '#3178c6', tsx: '#3178c6', js: '#f7df1e', jsx: '#f7df1e', mjs: '#f7df1e',
    svelte: '#ff3e00', vue: '#42b883', rs: '#dea584', go: '#00add8',
    py: '#3776ab', rb: '#cc342d', java: '#b07219', kt: '#a97bff',
    c: '#555555', cpp: '#f34b7d', h: '#555555', hpp: '#f34b7d',
    css: '#563d7c', scss: '#c6538c', less: '#1d365d', html: '#e34c26',
    json: '#292929', yaml: '#cb171e', yml: '#cb171e', toml: '#9c4121',
    md: '#083fa1', mdx: '#083fa1', txt: '#6a737d',
    sh: '#89e051', bash: '#89e051', zsh: '#89e051', fish: '#89e051',
    sql: '#e38c00', graphql: '#e10098', gql: '#e10098',
    png: '#a074c4', jpg: '#a074c4', jpeg: '#a074c4', gif: '#a074c4',
    webp: '#a074c4', svg: '#ffb13b', ico: '#a074c4',
    wasm: '#654ff0', docker: '#384d54', dockerfile: '#384d54',
    lock: '#6a737d', xml: '#0060ac', csv: '#237346',
  };

  function getFileColor(name: string): string | null {
    const dot = name.lastIndexOf('.');
    if (dot === -1) return null;
    const ext = name.slice(dot + 1).toLowerCase();
    return EXT_COLORS[ext] ?? null;
  }

  // ── Fuzzy matching with highlight indices ──────────────────────────
  interface FuzzyResult {
    path: string;
    score: number;
    indices: number[];
  }

  function fuzzyMatch(q: string, path: string): FuzzyResult | null {
    const lq = q.toLowerCase();
    const lp = path.toLowerCase();
    let qi = 0;
    let score = 0;
    let lastMatch = -1;
    const indices: number[] = [];

    for (let pi = 0; pi < lp.length && qi < lq.length; pi++) {
      if (lp[pi] === lq[qi]) {
        score += (pi === lastMatch + 1) ? 10 : 1;
        if (pi === 0 || lp[pi - 1] === '/') score += 5;
        lastMatch = pi;
        indices.push(pi);
        qi++;
      }
    }

    if (qi < lq.length) return null;
    score -= path.length * 0.01;
    return { path, score, indices };
  }

  /** Convert a glob pattern (with * and ?) into a RegExp. */
  function globToRegex(pattern: string): RegExp {
    const escaped = pattern
      .replace(/[.+^${}()|[\]\\]/g, '\\$&')
      .replace(/\*\*/g, '\u0000')
      .replace(/\*/g, '[^/]*')
      .replace(/\u0000/g, '.*')
      .replace(/\?/g, '[^/]');
    return new RegExp(escaped, 'i');
  }

  // ── Recently opened files ──────────────────────────────────────────
  function getRecentFiles(): string[] {
    const recent: string[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.editor_file) {
            const fp = tab.editor_file.remote_path ?? tab.editor_file.file_path;
            if (fp && !recent.includes(fp)) recent.push(fp);
          }
        }
      }
    }
    return recent;
  }

  // ── Filtered results ───────────────────────────────────────────────
  interface FilteredItem {
    path: string;
    indices: number[] | null; // null = no highlight (recent/glob/default)
    isRecent?: boolean;
  }

  const isGlob = $derived(query.includes('*') || query.includes('?'));

  // ── Targeted subdirectory search for glob patterns ─────────────────
  // When a glob has a directory prefix (e.g. "Downloads/*.webp"), the
  // pre-loaded file list may not contain those files (10k cap reached
  // before that directory). We do a targeted backend search instead.

  /** Extract the static directory prefix before the first glob character. */
  function getGlobDirPrefix(q: string): string | null {
    const firstGlob = Math.min(
      q.includes('*') ? q.indexOf('*') : Infinity,
      q.includes('?') ? q.indexOf('?') : Infinity,
    );
    const prefixPart = q.slice(0, firstGlob);
    const lastSlash = prefixPart.lastIndexOf('/');
    if (lastSlash <= 0) return null; // no directory component
    return prefixPart.slice(0, lastSlash);
  }

  let targetedFiles = $state<string[] | null>(null);
  let targetedLoading = $state(false);
  let targetedDir = $state<string | null>(null);
  let targetedTimer: ReturnType<typeof setTimeout> | null = null;

  // Trigger targeted search when glob query has a directory prefix
  $effect(() => {
    const q = query.trim();
    if (!isGlob || !lastCtx) {
      targetedFiles = null;
      targetedDir = null;
      return;
    }

    const dirPrefix = getGlobDirPrefix(q);
    if (!dirPrefix) {
      targetedFiles = null;
      targetedDir = null;
      return;
    }

    // Debounce the targeted search
    if (targetedTimer) clearTimeout(targetedTimer);
    targetedTimer = setTimeout(() => {
      doTargetedSearch(dirPrefix);
    }, 200);

    return () => {
      if (targetedTimer) { clearTimeout(targetedTimer); targetedTimer = null; }
    };
  });

  async function doTargetedSearch(dirPrefix: string) {
    const ctx = lastCtx;
    if (!ctx) return;
    const subPath = ctx.cwd.endsWith('/')
      ? ctx.cwd + dirPrefix
      : ctx.cwd + '/' + dirPrefix;

    targetedLoading = true;
    try {
      let result: string[];
      if (isRemote && ctx.sshCommand) {
        result = await sshListFiles(ctx.sshCommand, subPath, 5000, showHidden, showIgnored);
      } else {
        result = await listFiles(subPath, MAX_FILES, showHidden, showIgnored);
      }
      // Prefix results with the directory so they match the full relative path
      targetedFiles = result.map(f => `${dirPrefix}/${f}`);
      targetedDir = dirPrefix;
    } catch {
      targetedFiles = null;
      targetedDir = null;
    } finally {
      targetedLoading = false;
    }
  }

  const filtered = $derived.by((): FilteredItem[] => {
    const q = query.trim();

    if (!q) {
      // Show recently opened files first, then mtime-sorted files
      const recent = getRecentFiles();
      const recentRelative = recent
        .map(fp => {
          if (basePath && fp.startsWith(basePath)) {
            const rel = fp.slice(basePath.length).replace(/^\//, '');
            if (rel) return rel;
          }
          return null;
        })
        .filter((r): r is string => r !== null && files.includes(r));

      const items: FilteredItem[] = [];
      const seen = new Set<string>();

      for (const r of recentRelative) {
        if (items.length >= MAX_RESULTS) break;
        items.push({ path: r, indices: null, isRecent: true });
        seen.add(r);
      }
      for (const f of files) {
        if (items.length >= MAX_RESULTS) break;
        if (!seen.has(f)) items.push({ path: f, indices: null });
      }
      return items;
    }

    if (isGlob) {
      const re = globToRegex(q);
      // Search both the pre-loaded files and targeted subdirectory files
      const searchPool = targetedFiles
        ? [...new Set([...files, ...targetedFiles])]
        : files;
      const matched: FilteredItem[] = [];
      for (const f of searchPool) {
        if (matched.length >= MAX_RESULTS) break;
        if (re.test(f)) matched.push({ path: f, indices: null });
      }
      return matched;
    }

    const scored: FuzzyResult[] = [];
    for (const f of files) {
      const result = fuzzyMatch(q, f);
      if (result) scored.push(result);
    }
    scored.sort((a, b) => b.score - a.score);
    return scored.slice(0, MAX_RESULTS).map(s => ({ path: s.path, indices: s.indices }));
  });

  // ── Terminal context detection ─────────────────────────────────────
  function findTerminalContext(): { tabId: string; ptyId: string } | null {
    const activeTab = workspacesStore.activeTab;
    if (activeTab?.tab_type === 'terminal') {
      const inst = terminalsStore.get(activeTab.id);
      if (inst) return { tabId: activeTab.id, ptyId: inst.ptyId };
    }
    const pane = workspacesStore.activePane;
    if (pane) {
      for (const tab of pane.tabs) {
        // Never a stack service tab (docs/stack.md §7): running a `cd` in one would type
        // over a dev server, and the human would not even see where it went.
        if (tab.service_id) continue;
        if (tab.tab_type === 'terminal') {
          const inst = terminalsStore.get(tab.id);
          if (inst) return { tabId: tab.id, ptyId: inst.ptyId };
        }
      }
    }
    return null;
  }

  // ── Saved context for reload ───────────────────────────────────────
  let lastCtx: { tabId: string; ptyId: string; sshCommand: string | null; cwd: string } | null = null;
  /** The original terminal CWD (for resolving relative navigations) */
  let originalCwd = $state('');
  /** Navigation history for the back button */
  let navStack = $state<string[]>([]);

  async function loadFiles(forceRefresh = false, targetDir?: string) {
    // On first call, detect terminal context
    if (!lastCtx) {
      const ctx = findTerminalContext();
      if (!ctx) {
        error = 'No terminal context available';
        return;
      }
      loading = true;
      error = null;

      try {
        const ptyInfo = await getPtyInfo(ctx.ptyId);
        const sshCommand = ptyInfo.foreground_command;
        isRemote = !!sshCommand;

        let cwd: string;
        if (isRemote) {
          const oscState = terminalsStore.getOsc(ctx.tabId);
          const remoteCwd = oscState?.cwd ?? oscState?.promptCwd;
          if (!remoteCwd) {
            error = 'Cannot determine remote working directory';
            loading = false;
            return;
          }
          cwd = remoteCwd;
        } else {
          if (!ptyInfo.cwd) {
            error = 'Cannot determine working directory';
            loading = false;
            return;
          }
          cwd = ptyInfo.cwd;
        }

        lastCtx = { tabId: ctx.tabId, ptyId: ctx.ptyId, sshCommand, cwd };
        originalCwd = cwd;
        if (!targetDir) targetDir = cwd;
      } catch (e) {
        error = String(e);
        logError(`QuickOpen: ${e}`);
        loading = false;
        return;
      }
    }

    const cwd = targetDir ?? basePath;
    basePath = cwd;
    loading = true;
    error = null;

    try {
      // Check cache
      if (!forceRefresh && cache && cache.cwd === cwd && cache.hidden === showHidden && cache.ignored === showIgnored && Date.now() - cache.time < CACHE_TTL) {
        files = cache.files;
        capped = cache.capped;
        loading = false;
        return;
      }

      let result: string[];
      const maxLimit = isRemote ? 5000 : MAX_FILES;
      const ctx = lastCtx;

      if (isRemote && ctx?.sshCommand) {
        result = await sshListFiles(ctx.sshCommand, cwd, maxLimit, showHidden, showIgnored);
      } else {
        result = await listFiles(cwd, maxLimit, showHidden, showIgnored);
      }

      if (!open) return;

      files = result;
      capped = result.length >= maxLimit;
      cache = { cwd, files: result, capped, hidden: showHidden, ignored: showIgnored, time: Date.now() };
    } catch (e) {
      if (!open) return;
      error = String(e);
      logError(`QuickOpen: ${e}`);
    } finally {
      loading = false;
    }
  }

  // ── Directory navigation ───────────────────────────────────────────
  // Press Tab when query contains "/" or starts with "~" to navigate into it.
  // Supports: "Downloads", "Downloads/", "../", "~/scripts", "/tmp"

  function resolveDirPath(input: string): string | null {
    const trimmed = input.replace(/\/+$/, '');
    if (!trimmed) return null;

    // Absolute path: ~/... or /...
    if (trimmed === '~' || trimmed.startsWith('~/')) {
      return trimmed; // backend handles tilde expansion
    }
    if (trimmed.startsWith('/')) {
      return trimmed;
    }

    // Relative to current basePath
    const base = basePath.endsWith('/') ? basePath.slice(0, -1) : basePath;

    if (trimmed === '..') {
      const lastSlash = base.lastIndexOf('/');
      return lastSlash > 0 ? base.slice(0, lastSlash) : '/';
    }

    if (trimmed.startsWith('../')) {
      let resolved = base;
      let rest = trimmed;
      while (rest.startsWith('../')) {
        const lastSlash = resolved.lastIndexOf('/');
        resolved = lastSlash > 0 ? resolved.slice(0, lastSlash) : '/';
        rest = rest.slice(3);
      }
      return rest ? `${resolved}/${rest}` : resolved;
    }

    return `${base}/${trimmed}`;
  }

  /** Validated: true only when the resolved path is a real directory. */
  let queryLooksLikeDir = $state(false);
  let dirCheckTimer: ReturnType<typeof setTimeout> | null = null;
  let dirCheckVersion = 0; // guard against stale results

  $effect(() => {
    const q = query.trim();
    const ctx = lastCtx;

    // Quick reject: no path-like characters
    if (!q || isGlob || !ctx || (!q.includes('/') && !q.startsWith('~'))) {
      queryLooksLikeDir = false;
      return;
    }

    const resolved = resolveDirPath(q);
    if (!resolved) {
      queryLooksLikeDir = false;
      return;
    }

    // Debounce the directory check
    queryLooksLikeDir = false;
    if (dirCheckTimer) clearTimeout(dirCheckTimer);
    const version = ++dirCheckVersion;
    dirCheckTimer = setTimeout(async () => {
      try {
        let exists: boolean;
        if (isRemote && ctx.sshCommand) {
          exists = await sshIsDirectory(ctx.sshCommand, resolved);
        } else {
          exists = await isDirectory(resolved);
        }
        // Only apply if query hasn't changed since we started
        if (version === dirCheckVersion) {
          queryLooksLikeDir = exists;
        }
      } catch {
        if (version === dirCheckVersion) {
          queryLooksLikeDir = false;
        }
      }
    }, 150);

    return () => {
      if (dirCheckTimer) { clearTimeout(dirCheckTimer); dirCheckTimer = null; }
    };
  });

  async function navigateToDir(dir: string) {
    navStack = [...navStack, basePath];
    query = '';
    selectedIndex = 0;
    targetedFiles = null;
    targetedDir = null;
    await loadFiles(false, dir);
  }

  function navigateBack() {
    if (navStack.length === 0) return;
    const prev = navStack[navStack.length - 1];
    navStack = navStack.slice(0, -1);
    query = '';
    selectedIndex = 0;
    targetedFiles = null;
    targetedDir = null;
    loadFiles(false, prev);
  }

  // Load files when modal opens.
  // Only `open` is a reactive dependency — the body is untracked so that writing
  // the showHidden/showIgnored preferences from the toggle buttons doesn't re-run
  // this effect (which would wipe the search query and reload). See the
  // "$effect reactive loops with stores" pitfall in CLAUDE.md.
  $effect(() => {
    if (!open) return;
    untrack(() => {
      query = '';
      selectedIndex = 0;
      error = null;
      lastCtx = null; // force re-detection of terminal context
      navStack = [];
      palettePos = (savedPos && validatePosition(savedPos)) ?? { x: 0, y: 0 };
      // Sync toggle state from persisted preferences (loaded async after app start)
      showHidden = preferencesStore.quickOpenShowHidden;
      showIgnored = preferencesStore.quickOpenShowIgnored;
      loadFiles();
      requestAnimationFrame(() => inputRef?.focus());
    });
  });

  function toggleHidden() {
    showHidden = !showHidden;
    preferencesStore.setQuickOpenShowHidden(showHidden);
    cache = null;
    loadFiles(true);
  }

  function toggleIgnored() {
    showIgnored = !showIgnored;
    preferencesStore.setQuickOpenShowIgnored(showIgnored);
    cache = null;
    loadFiles(true);
  }

  // Reset selection when query changes
  $effect(() => {
    void query;
    selectedIndex = 0;
  });

  // Scroll selected item into view
  $effect(() => {
    if (!listRef) return;
    const item = listRef.children[selectedIndex] as HTMLElement | undefined;
    item?.scrollIntoView({ block: 'nearest' });
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
      return;
    }
    // Backspace on empty query navigates back
    if (e.key === 'Backspace' && query === '' && navStack.length > 0) {
      e.preventDefault();
      navigateBack();
      return;
    }
    // Tab navigates into directory when query looks like a path
    if (e.key === 'Tab' && queryLooksLikeDir) {
      e.preventDefault();
      const resolved = resolveDirPath(query.trim());
      if (resolved) navigateToDir(resolved);
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (filtered.length > 0) {
        selectedIndex = (selectedIndex + 1) % filtered.length;
      }
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (filtered.length > 0) {
        selectedIndex = (selectedIndex - 1 + filtered.length) % filtered.length;
      }
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const selected = filtered[selectedIndex];
      if (selected) {
        onselect(resolveFullPath(selected.path));
      }
      return;
    }
  }

  /** Resolve a relative file path against the current basePath to get a full path. */
  function resolveFullPath(relPath: string): string {
    if (relPath.startsWith('/') || relPath.startsWith('~')) return relPath;
    if (!basePath) return relPath;
    const base = basePath.endsWith('/') ? basePath.slice(0, -1) : basePath;
    return `${base}/${relPath}`;
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onclose();
    }
  }

  function splitPath(filePath: string): { name: string; dir: string } {
    const lastSlash = filePath.lastIndexOf('/');
    if (lastSlash === -1) return { name: filePath, dir: '' };
    return {
      name: filePath.slice(lastSlash + 1),
      dir: filePath.slice(0, lastSlash),
    };
  }

  // ── Dragging ────────────────────────────────────────────────────────
  // Position persists across open/close; validated against viewport on open.
  let dragOffset = $state<{ x: number; y: number } | null>(null);
  let palettePos = $state<{ x: number; y: number }>({ x: 0, y: 0 });
  let savedPos: { x: number; y: number } | null = null;
  let paletteRef = $state<HTMLDivElement | null>(null);

  /** Check if saved position keeps the palette reasonably visible. */
  function validatePosition(pos: { x: number; y: number }): { x: number; y: number } | null {
    // The palette is centered at ~50% horizontally, 15vh vertically via CSS.
    // The transform offsets from that default position.
    // Just check the offset isn't so extreme the palette is off-screen.
    const maxX = window.innerWidth / 2;
    const maxY = window.innerHeight * 0.7;
    if (Math.abs(pos.x) > maxX || Math.abs(pos.y) > maxY) return null;
    return pos;
  }

  function onDragStart(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target.tagName === 'INPUT' || target.tagName === 'BUTTON' || target.closest('button')) return;
    e.preventDefault();
    dragOffset = { x: e.clientX - palettePos.x, y: e.clientY - palettePos.y };
    window.addEventListener('mousemove', onDragMove);
    window.addEventListener('mouseup', onDragEnd);
  }

  function onDragMove(e: MouseEvent) {
    if (!dragOffset) return;
    palettePos = { x: e.clientX - dragOffset.x, y: e.clientY - dragOffset.y };
  }

  function onDragEnd() {
    dragOffset = null;
    savedPos = { ...palettePos };
    window.removeEventListener('mousemove', onDragMove);
    window.removeEventListener('mouseup', onDragEnd);
  }

  /** Build highlighted spans for a filename with matched indices. */
  function highlightChars(text: string, indices: number[], offset: number): Array<{ text: string; highlight: boolean }> {
    const indexSet = new Set(indices.filter(i => i >= offset && i < offset + text.length).map(i => i - offset));
    const spans: Array<{ text: string; highlight: boolean }> = [];
    let current = '';
    let currentHighlight = false;

    for (let i = 0; i < text.length; i++) {
      const isMatch = indexSet.has(i);
      if (i === 0) {
        currentHighlight = isMatch;
        current = text[i];
      } else if (isMatch === currentHighlight) {
        current += text[i];
      } else {
        spans.push({ text: current, highlight: currentHighlight });
        current = text[i];
        currentHighlight = isMatch;
      }
    }
    if (current) spans.push({ text: current, highlight: currentHighlight });
    return spans;
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="backdrop"
    onclick={handleBackdropClick}
    onkeydown={handleKeydown}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    <div class="palette" style={palettePos.x || palettePos.y ? `transform: translate(${palettePos.x}px, ${palettePos.y}px)` : ''}>
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="input-row" onmousedown={onDragStart}>
        <div class="input-wrapper">
          {#if navStack.length > 0}
            <button
              class="back-btn"
              title="Go back (or press Backspace when empty)"
              onclick={navigateBack}
            >←</button>
          {/if}
          <input
            bind:this={inputRef}
            bind:value={query}
            type="text"
            placeholder={navStack.length > 0 ? 'Search here… (⌫ to go back)' : 'Search files… (type path/ to navigate, *glob)'}
            spellcheck="false"
            autocomplete="off"
          />
        </div>
        {#if basePath}
          <div class="input-meta">
            <div class="meta-left">
              <Tooltip text="Refresh file list">
                <button
                  class="refresh-btn"
                  onclick={() => { cache = null; loadFiles(true); }}
                >↻</button>
              </Tooltip>
              {#if basePath !== originalCwd}
                <Tooltip text="Return to terminal CWD">
                  <button
                    class="home-btn"
                    onclick={() => { navStack = []; query = ''; loadFiles(false, originalCwd); }}
                  >⌂</button>
                </Tooltip>
              {/if}
              <span class="base-path" title={basePath}>{basePath}</span>
            </div>
            <div class="input-actions">
              <Tooltip text={showHidden ? 'Dotfiles: shown · click to hide' : 'Dotfiles: hidden · click to show (.env always shows)'}>
                <button
                  class="toggle-btn"
                  class:active={showHidden}
                  onclick={toggleHidden}
                >.*</button>
              </Tooltip>
              <Tooltip text={showIgnored ? 'Gitignored files: shown · click to hide' : 'Gitignored files: hidden · click to show (.env always shows)'}>
                <button
                  class="toggle-btn"
                  class:active={showIgnored}
                  onclick={toggleIgnored}
                >.gi</button>
              </Tooltip>
            </div>
          </div>
        {/if}
      </div>

      {#if queryLooksLikeDir}
        <div class="nav-hint">
          Press <kbd>Tab</kbd> to navigate into <strong>{query.trim()}</strong>
        </div>
      {/if}

      <div class="results" bind:this={listRef}>
        {#if loading && files.length === 0}
          <div class="status">Loading files…</div>
        {:else if error}
          <div class="status error">
            {error}
            <button class="retry-btn" onclick={() => loadFiles(true)}>Retry</button>
          </div>
        {:else if filtered.length === 0}
          <div class="status">
            {query ? 'No matching files' : 'No files found'}
          </div>
        {:else}
          {#each filtered as item, i}
            {@const { name, dir } = splitPath(item.path)}
            {@const color = getFileColor(name)}
            {@const nameOffset = dir ? dir.length + 1 : 0}
            <button
              class="result-item"
              class:selected={i === selectedIndex}
              onclick={() => onselect(resolveFullPath(item.path))}
              onmouseenter={() => { selectedIndex = i; }}
            >
              <span class="file-icon" style={color ? `background: ${color}` : ''}></span>
              <span class="file-name">
                {#if item.indices}
                  {#each highlightChars(name, item.indices, nameOffset) as span}
                    {#if span.highlight}<mark>{span.text}</mark>{:else}{span.text}{/if}
                  {/each}
                {:else}
                  {name}
                {/if}
              </span>
              {#if dir}
                <span class="file-dir">
                  {#if item.indices}
                    {#each highlightChars(dir, item.indices, 0) as span}
                      {#if span.highlight}<mark>{span.text}</mark>{:else}{span.text}{/if}
                    {/each}
                  {:else}
                    {dir}
                  {/if}
                </span>
              {/if}
              {#if item.isRecent}
                <span class="recent-badge">recent</span>
              {/if}
            </button>
          {/each}
        {/if}
      </div>

      {#if !error && files.length > 0}
        <div class="footer">
          <span class="count">
            {files.length} files{capped ? ' (limit reached)' : ''}
            {#if loading} · refreshing…{/if}
            {#if targetedLoading} · searching…{/if}
          </span>
          <span class="shortcut-hint">↑↓ navigate · ↵ open · ⇥ enter dir · esc close</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 15vh;
    z-index: 1000;
  }

  .palette {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: 560px;
    max-height: 440px;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    align-self: flex-start;
  }

  .input-row {
    display: flex;
    flex-direction: column;
    padding: 10px 12px 8px;
    border-bottom: 1px solid var(--bg-light);
    cursor: grab;
  }

  .input-row:active {
    cursor: grabbing;
  }

  .input-wrapper {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .back-btn {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 1rem;
    padding: 6px 8px;
    flex-shrink: 0;
    line-height: 1;
  }

  .back-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 8px 10px;
    font-size: 1rem;
    color: var(--fg);
    outline: none;
    font-family: inherit;
    flex: 1;
    min-width: 0;
  }

  .refresh-btn {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 0.769rem;
    padding: 1px 5px;
    line-height: 1.4;
    flex-shrink: 0;
    font-family: inherit;
  }

  .refresh-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  input:focus {
    border-color: var(--accent);
  }

  input::placeholder {
    color: var(--fg-dim);
  }

  .input-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 4px;
  }

  .meta-left {
    display: flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    overflow: hidden;
  }

  .base-path {
    font-size: 0.769rem;
    color: var(--fg-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .home-btn {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    color: var(--accent);
    cursor: pointer;
    font-size: 0.769rem;
    padding: 1px 5px;
    line-height: 1.4;
    flex-shrink: 0;
    font-family: inherit;
  }

  .home-btn:hover {
    background: var(--bg-light);
    filter: brightness(1.2);
  }

  .input-actions {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }

  .toggle-btn {
    font-size: 0.769rem;
    padding: 1px 6px;
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    background: none;
    color: var(--fg-dim);
    cursor: pointer;
    font-family: inherit;
    line-height: 1.4;
  }

  .toggle-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .toggle-btn.active {
    background: var(--accent);
    color: var(--bg-dark);
    border-color: var(--accent);
  }

  .nav-hint {
    padding: 6px 12px;
    font-size: 0.769rem;
    color: var(--fg-dim);
    background: var(--bg-dark);
    border-bottom: 1px solid var(--bg-light);
  }

  .nav-hint kbd {
    background: var(--bg-light);
    border: 1px solid var(--fg-dim);
    border-radius: 3px;
    padding: 0 4px;
    font-size: 0.692rem;
    font-family: inherit;
  }

  .nav-hint strong {
    color: var(--accent);
  }

  .results {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .status {
    padding: 16px 12px;
    color: var(--fg-dim);
    font-size: 0.923rem;
    text-align: center;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }

  .status.error {
    color: var(--red, #f7768e);
  }

  .retry-btn {
    font-size: 0.846rem;
    padding: 3px 10px;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    background: var(--bg-dark);
    color: var(--fg);
    cursor: pointer;
  }

  .retry-btn:hover {
    background: var(--bg-light);
  }

  .result-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 12px;
    width: 100%;
    border: none;
    background: none;
    color: var(--fg);
    font-size: 0.923rem;
    font-family: inherit;
    cursor: pointer;
    text-align: left;
  }

  .result-item:hover,
  .result-item.selected {
    background: var(--bg-light);
  }

  .file-icon {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--fg-dim);
  }

  .file-name {
    font-weight: 600;
    flex-shrink: 0;
  }

  .file-dir {
    color: var(--fg-dim);
    font-size: 0.846rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .recent-badge {
    font-size: 0.692rem;
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 3px;
    padding: 0 4px;
    flex-shrink: 0;
    line-height: 1.5;
    margin-left: auto;
  }

  mark {
    background: none;
    color: var(--accent);
    font-weight: 700;
  }

  .footer {
    padding: 6px 12px;
    border-top: 1px solid var(--bg-light);
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .count {
    font-size: 0.769rem;
    color: var(--fg-dim);
  }

  .shortcut-hint {
    font-size: 0.692rem;
    color: var(--fg-dim);
  }
</style>
