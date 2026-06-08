import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Only ignore src-tauri; Vite already ignores node_modules/.git.
      // Note: editing project files in the maiTerm editor tab will trigger
      // HMR in dev mode — this is expected and harmless in production.
      ignored: ['**/src-tauri/**']
    }
  }
});
