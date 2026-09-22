---
title: Themes
description: 15 built-in themes plus custom theme support with separate UI and terminal colors.
---

maiTerm ships with 15 built-in themes and supports fully custom themes.

## Built-in Themes

1. **Tokyo Night** (default)
2. **Dracula**
3. **Solarized Dark**
4. **Solarized Light**
5. **Tokyo Night Day**
6. **Catppuccin Latte**
7. **Gruvbox Light**
8. **One Light**
9. **GitHub Light**
10. **Nord**
11. **Gruvbox Dark**
12. **Monokai**
13. **Catppuccin Mocha**
14. **One Dark**
15. **macOS Pro**

## Custom Themes

Create and edit custom themes via the theme editor in Preferences. Each theme has two parts:

### UI Colors

CSS variables that control the application interface:

| Variable | Purpose |
|----------|---------|
| `--bg-dark` | Main background |
| `--bg-medium` | Elevated surfaces |
| `--bg-light` | Borders, hover states |
| `--fg` | Primary text |
| `--fg-dim` | Secondary text |
| `--accent` | Interactive elements |

### Terminal Colors

Full ANSI 16-color palette plus cursor and selection colors. These are applied directly to the xterm.js terminal instance.

## How Themes Work

Themes are defined in `src/lib/themes/index.ts`. When applied:

1. UI colors are set as CSS custom properties on `document.documentElement`
2. Terminal colors are applied to each xterm.js instance
3. The CodeMirror editor takes its colours from the same theme, light or dark
