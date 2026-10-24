import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// An isolated dev instance (`npm run tauri:dev:isolated`, scripts/dev-isolated.mjs) serves on
// its own port so it never meets the dev app's 1420.
const port = Number(process.env.MAITERM_VITE_PORT) || 1420;

export default defineConfig({
  plugins: [sveltekit()],
  clearScreen: false,
  server: {
    port,
    strictPort: true,
    watch: {
      // Only ignore src-tauri; Vite already ignores node_modules/.git.
      // Note: editing project files in the maiTerm editor tab will trigger
      // HMR in dev mode — this is expected and harmless in production.
      ignored: ['**/src-tauri/**']
    }
  }
});
