import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

// Standalone Vitest config — deliberately does NOT extend vite.config.ts (no SvelteKit
// plugin), so pure-logic modules (e.g. agentDelivery.ts) run in plain Node with no runes
// compilation. Unit tests live next to their module as *.test.ts.
//
// `$lib` IS resolved, though. Leaving it out did not keep tests honest, it just made them
// arbitrary: a module was testable or not depending on whether its imports happened to be
// type-only (erased at transform) or runtime. `triggers/defaults.ts` fell the wrong side of
// that line over one `getResumeCommand` import, and its seeder is the one with a live
// data-loss path. Path resolution is not runes compilation — a test that reaches a
// `.svelte.ts` store still fails, exactly as before.
export default defineConfig({
  resolve: {
    alias: { $lib: fileURLToPath(new URL('./src/lib', import.meta.url)) },
  },
  test: {
    environment: 'node',
    include: ['src/**/*.{test,spec}.ts'],
  },
});
