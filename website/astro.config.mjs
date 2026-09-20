import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://maiterm.dev',
  // Old single-agent URL → renamed agent-neutral page (v1.17.0 Codex support).
  redirects: {
    '/features/claude-code/': '/features/agents/',
  },
  integrations: [
    starlight({
      title: 'maiTerm',
      logo: {
        light: './src/assets/icon-light.png',
        dark: './src/assets/icon-dark.png',
      },
      favicon: '/favicon.png',
      components: {
        // Duo-tone wordmark in the header — see the component for why an
        // override is needed rather than CSS.
        SiteTitle: './src/components/SiteTitle.astro',
      },
      social: {
        github: 'https://github.com/Flexmark-Intl/maiterm',
      },
      expressiveCode: {
        themes: ['tokyo-night'],
      },
      // No theme head script. Starlight's own ThemeProvider already resolves
      // `storedTheme || prefers-color-scheme`, so a first-time visitor follows
      // their system. We used to override that to force dark and, worse, WRITE
      // 'dark' into localStorage — which pinned the visitor to dark forever as
      // if they had chosen it, on a machine set to light. 37449f4 (2026-06-08)
      // was a deliberate decision at the time; the new site drops it.
      customCss: ['./src/styles/custom.css'],
      sidebar: [
        { label: 'Download', slug: 'download' },
        {
          label: 'Features',
          items: [
            { label: 'Terminal', slug: 'features/terminal' },
            { label: 'Workspaces & Panes', slug: 'features/workspaces' },
            { label: 'Workspace Stack', slug: 'features/stack' },
            { label: 'Code Editor', slug: 'features/editor' },
            { label: 'Agent Integration', slug: 'features/agents' },
            { label: 'Agent Accounts', slug: 'features/accounts' },
            { label: 'Deshittification', slug: 'features/deshittification' },
            { label: 'Tasks', slug: 'features/tasks' },
            { label: 'Overlord', slug: 'features/overlord' },
            { label: 'Agent Bridge', slug: 'features/agent-bridge' },
            { label: 'Mesh Workspace', slug: 'features/mesh-workspace' },
            { label: 'maiLink Companion', slug: 'features/mailink' },
            { label: 'Chat Threads', slug: 'features/comms' },
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
