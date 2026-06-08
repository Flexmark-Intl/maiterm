import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://maiterm.dev',
  integrations: [
    starlight({
      title: 'maiTerm',
      logo: {
        src: './src/assets/icon.png',
      },
      favicon: '/favicon.png',
      social: {
        github: 'https://github.com/Flexmark-Intl/maiterm',
      },
      expressiveCode: {
        themes: ['tokyo-night'],
      },
      head: [
        {
          tag: 'script',
          // Default first-time visitors to dark. Only act when no choice is
          // stored yet — Starlight stores "auto" as an empty string, so a
          // falsy check would clobber it back to dark on every navigation.
          content: `if (localStorage.getItem('starlight-theme') === null) { localStorage.setItem('starlight-theme', 'dark'); document.documentElement.dataset.theme = 'dark'; }`,
        },
      ],
      customCss: ['./src/styles/custom.css'],
      sidebar: [
        {
          label: 'Features',
          items: [
            { label: 'Terminal', slug: 'features/terminal' },
            { label: 'Workspaces & Panes', slug: 'features/workspaces' },
            { label: 'Code Editor', slug: 'features/editor' },
            { label: 'Claude Code Integration', slug: 'features/claude-code' },
            { label: 'Agent Bridge', slug: 'features/agent-bridge' },
            { label: 'Triggers & Automation', slug: 'features/triggers' },
            { label: 'Themes', slug: 'features/themes' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Getting Started', slug: 'guides/getting-started' },
            { label: 'Keyboard Shortcuts', slug: 'guides/keyboard-shortcuts' },
            { label: 'Building from Source', slug: 'guides/building' },
          ],
        },
      ],
    }),
  ],
});
