export interface UiColors {
  bg_dark: string;
  bg_medium: string;
  bg_light: string;
  fg: string;
  fg_dim: string;
  accent: string;
  accent_hover: string;
  green: string;
  red: string;
  yellow: string;
  cyan: string;
  magenta: string;
  tab_border: string;
  tab_border_active: string;
  tab_border_activity: string;
}

export interface TerminalColors {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  selectionForeground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export interface Theme {
  id: string;
  name: string;
  ui: UiColors;
  terminal: TerminalColors;
}

export const builtinThemes: Theme[] = [
  {
    id: 'tokyo-night',
    name: 'Tokyo Night',
    ui: {
      bg_dark: '#1a1b26',
      bg_medium: '#24283b',
      bg_light: '#414868',
      fg: '#c0caf5',
      fg_dim: '#565f89',
      accent: '#7aa2f7',
      accent_hover: '#89b4fa',
      green: '#9ece6a',
      red: '#f7768e',
      yellow: '#e0af68',
      cyan: '#7dcfff',
      magenta: '#bb9af7',
      tab_border: 'transparent',
      tab_border_active: '#7aa2f7',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#1a1b26',
      foreground: '#c0caf5',
      cursor: '#c0caf5',
      cursorAccent: '#1a1b26',
      selectionBackground: '#33467c',
      selectionForeground: '#c0caf5',
      black: '#15161e',
      red: '#f7768e',
      green: '#9ece6a',
      yellow: '#e0af68',
      blue: '#7aa2f7',
      magenta: '#bb9af7',
      cyan: '#7dcfff',
      white: '#a9b1d6',
      brightBlack: '#414868',
      brightRed: '#f7768e',
      brightGreen: '#9ece6a',
      brightYellow: '#e0af68',
      brightBlue: '#7aa2f7',
      brightMagenta: '#bb9af7',
      brightCyan: '#7dcfff',
      brightWhite: '#c0caf5',
    },
  },
  {
    id: 'dracula',
    name: 'Dracula',
    ui: {
      bg_dark: '#282a36',
      bg_medium: '#2d303e',
      bg_light: '#44475a',
      fg: '#f8f8f2',
      fg_dim: '#6272a4',
      accent: '#bd93f9',
      accent_hover: '#caa9fa',
      green: '#50fa7b',
      red: '#ff5555',
      yellow: '#f1fa8c',
      cyan: '#8be9fd',
      magenta: '#ff79c6',
      tab_border: 'transparent',
      tab_border_active: '#bd93f9',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#282a36',
      foreground: '#f8f8f2',
      cursor: '#f8f8f2',
      cursorAccent: '#282a36',
      selectionBackground: '#44475a',
      selectionForeground: '#f8f8f2',
      black: '#21222c',
      red: '#ff5555',
      green: '#50fa7b',
      yellow: '#f1fa8c',
      blue: '#bd93f9',
      magenta: '#ff79c6',
      cyan: '#8be9fd',
      white: '#f8f8f2',
      brightBlack: '#6272a4',
      brightRed: '#ff6e6e',
      brightGreen: '#69ff94',
      brightYellow: '#ffffa5',
      brightBlue: '#d6acff',
      brightMagenta: '#ff92df',
      brightCyan: '#a4ffff',
      brightWhite: '#ffffff',
    },
  },
  {
    id: 'solarized-dark',
    name: 'Solarized Dark',
    ui: {
      bg_dark: '#002b36',
      bg_medium: '#073642',
      bg_light: '#586e75',
      fg: '#839496',
      fg_dim: '#657b83',
      accent: '#268bd2',
      accent_hover: '#2aa1f5',
      green: '#859900',
      red: '#dc322f',
      yellow: '#b58900',
      cyan: '#2aa198',
      magenta: '#d33682',
      tab_border: 'transparent',
      tab_border_active: 'transparent',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#002b36',
      foreground: '#839496',
      cursor: '#839496',
      cursorAccent: '#002b36',
      selectionBackground: '#073642',
      selectionForeground: '#93a1a1',
      black: '#073642',
      red: '#dc322f',
      green: '#859900',
      yellow: '#b58900',
      blue: '#268bd2',
      magenta: '#d33682',
      cyan: '#2aa198',
      white: '#eee8d5',
      brightBlack: '#586e75',
      brightRed: '#cb4b16',
      brightGreen: '#586e75',
      brightYellow: '#657b83',
      brightBlue: '#839496',
      brightMagenta: '#6c71c4',
      brightCyan: '#93a1a1',
      brightWhite: '#fdf6e3',
    },
  },
  {
    id: 'solarized-light',
    name: 'Solarized Light',
    ui: {
      bg_dark: '#fdf6e3',
      bg_medium: '#eee8d5',
      bg_light: '#d3cdb8',
      fg: '#657b83',
      fg_dim: '#839496',
      accent: '#268bd2',
      accent_hover: '#2aa1f5',
      green: '#859900',
      red: '#dc322f',
      yellow: '#b58900',
      cyan: '#2aa198',
      magenta: '#d33682',
      tab_border: 'transparent',
      tab_border_active: '#268bd2',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#fdf6e3',
      foreground: '#657b83',
      cursor: '#657b83',
      cursorAccent: '#fdf6e3',
      selectionBackground: '#eee8d5',
      selectionForeground: '#586e75',
      black: '#073642',
      red: '#dc322f',
      green: '#859900',
      yellow: '#b58900',
      blue: '#268bd2',
      magenta: '#d33682',
      cyan: '#2aa198',
      white: '#eee8d5',
      brightBlack: '#586e75',
      brightRed: '#cb4b16',
      brightGreen: '#586e75',
      brightYellow: '#657b83',
      brightBlue: '#839496',
      brightMagenta: '#6c71c4',
      brightCyan: '#93a1a1',
      brightWhite: '#fdf6e3',
    },
  },
  {
    id: 'nord',
    name: 'Nord',
    ui: {
      bg_dark: '#2e3440',
      bg_medium: '#3b4252',
      bg_light: '#4c566a',
      fg: '#d8dee9',
      fg_dim: '#7b88a1',
      accent: '#88c0d0',
      accent_hover: '#8fbcbb',
      green: '#a3be8c',
      red: '#bf616a',
      yellow: '#ebcb8b',
      cyan: '#88c0d0',
      magenta: '#b48ead',
      tab_border: 'transparent',
      tab_border_active: 'transparent',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#2e3440',
      foreground: '#d8dee9',
      cursor: '#d8dee9',
      cursorAccent: '#2e3440',
      selectionBackground: '#434c5e',
      selectionForeground: '#d8dee9',
      black: '#3b4252',
      red: '#bf616a',
      green: '#a3be8c',
      yellow: '#ebcb8b',
      blue: '#81a1c1',
      magenta: '#b48ead',
      cyan: '#88c0d0',
      white: '#e5e9f0',
      brightBlack: '#4c566a',
      brightRed: '#bf616a',
      brightGreen: '#a3be8c',
      brightYellow: '#ebcb8b',
      brightBlue: '#81a1c1',
      brightMagenta: '#b48ead',
      brightCyan: '#8fbcbb',
      brightWhite: '#eceff4',
    },
  },
  {
    id: 'gruvbox-dark',
    name: 'Gruvbox Dark',
    ui: {
      bg_dark: '#282828',
      bg_medium: '#3c3836',
      bg_light: '#504945',
      fg: '#ebdbb2',
      fg_dim: '#928374',
      accent: '#458588',
      accent_hover: '#83a598',
      green: '#b8bb26',
      red: '#fb4934',
      yellow: '#fabd2f',
      cyan: '#8ec07c',
      magenta: '#d3869b',
      tab_border: 'transparent',
      tab_border_active: 'transparent',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#282828',
      foreground: '#ebdbb2',
      cursor: '#ebdbb2',
      cursorAccent: '#282828',
      selectionBackground: '#504945',
      selectionForeground: '#ebdbb2',
      black: '#282828',
      red: '#cc241d',
      green: '#98971a',
      yellow: '#d79921',
      blue: '#458588',
      magenta: '#b16286',
      cyan: '#689d6a',
      white: '#a89984',
      brightBlack: '#928374',
      brightRed: '#fb4934',
      brightGreen: '#b8bb26',
      brightYellow: '#fabd2f',
      brightBlue: '#83a598',
      brightMagenta: '#d3869b',
      brightCyan: '#8ec07c',
      brightWhite: '#ebdbb2',
    },
  },
  {
    id: 'monokai',
    name: 'Monokai',
    ui: {
      bg_dark: '#272822',
      bg_medium: '#2d2e27',
      bg_light: '#49483e',
      fg: '#f8f8f2',
      fg_dim: '#75715e',
      accent: '#66d9ef',
      accent_hover: '#78e2f5',
      green: '#a6e22e',
      red: '#f92672',
      yellow: '#e6db74',
      cyan: '#66d9ef',
      magenta: '#ae81ff',
      tab_border: 'transparent',
      tab_border_active: '#66d9ef',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#272822',
      foreground: '#f8f8f2',
      cursor: '#f8f8f2',
      cursorAccent: '#272822',
      selectionBackground: '#49483e',
      selectionForeground: '#f8f8f2',
      black: '#272822',
      red: '#f92672',
      green: '#a6e22e',
      yellow: '#f4bf75',
      blue: '#66d9ef',
      magenta: '#ae81ff',
      cyan: '#a1efe4',
      white: '#f8f8f2',
      brightBlack: '#75715e',
      brightRed: '#f92672',
      brightGreen: '#a6e22e',
      brightYellow: '#f4bf75',
      brightBlue: '#66d9ef',
      brightMagenta: '#ae81ff',
      brightCyan: '#a1efe4',
      brightWhite: '#f9f8f5',
    },
  },
  {
    id: 'catppuccin-mocha',
    name: 'Catppuccin Mocha',
    ui: {
      bg_dark: '#1e1e2e',
      bg_medium: '#313244',
      bg_light: '#45475a',
      fg: '#cdd6f4',
      fg_dim: '#6c7086',
      accent: '#89b4fa',
      accent_hover: '#b4d0fb',
      green: '#a6e3a1',
      red: '#f38ba8',
      yellow: '#f9e2af',
      cyan: '#89dceb',
      magenta: '#cba6f7',
      tab_border: 'transparent',
      tab_border_active: 'transparent',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#1e1e2e',
      foreground: '#cdd6f4',
      cursor: '#f5e0dc',
      cursorAccent: '#1e1e2e',
      selectionBackground: '#45475a',
      selectionForeground: '#cdd6f4',
      black: '#45475a',
      red: '#f38ba8',
      green: '#a6e3a1',
      yellow: '#f9e2af',
      blue: '#89b4fa',
      magenta: '#cba6f7',
      cyan: '#89dceb',
      white: '#bac2de',
      brightBlack: '#585b70',
      brightRed: '#f38ba8',
      brightGreen: '#a6e3a1',
      brightYellow: '#f9e2af',
      brightBlue: '#89b4fa',
      brightMagenta: '#cba6f7',
      brightCyan: '#89dceb',
      brightWhite: '#a6adc8',
    },
  },
  {
    id: 'one-dark',
    name: 'One Dark',
    ui: {
      bg_dark: '#282c34',
      bg_medium: '#2c313c',
      bg_light: '#3e4452',
      fg: '#abb2bf',
      fg_dim: '#5c6370',
      accent: '#61afef',
      accent_hover: '#74b9f0',
      green: '#98c379',
      red: '#e06c75',
      yellow: '#e5c07b',
      cyan: '#56b6c2',
      magenta: '#c678dd',
      tab_border: 'transparent',
      tab_border_active: '#61afef',
      tab_border_activity: 'transparent',
    },
    terminal: {
      background: '#282c34',
      foreground: '#abb2bf',
      cursor: '#528bff',
      cursorAccent: '#282c34',
      selectionBackground: '#3e4452',
      selectionForeground: '#abb2bf',
      black: '#282c34',
      red: '#e06c75',
      green: '#98c379',
      yellow: '#e5c07b',
      blue: '#61afef',
      magenta: '#c678dd',
      cyan: '#56b6c2',
      white: '#abb2bf',
      brightBlack: '#5c6370',
      brightRed: '#be5046',
      brightGreen: '#98c379',
      brightYellow: '#d19a66',
      brightBlue: '#61afef',
      brightMagenta: '#c678dd',
      brightCyan: '#56b6c2',
      brightWhite: '#ffffff',
    },
  },
  {
    id: 'macos-pro',
    name: 'macOS Pro',
    ui: {
      bg_dark: '#1e1e1e',
      bg_medium: '#252525',
      bg_light: '#3a3a3a',
      fg: '#f2f2f2',
      fg_dim: '#808080',
      accent: '#2997ff',
      accent_hover: '#4dacff',
      green: '#30d158',
      red: '#ff453a',
      yellow: '#ffd60a',
      cyan: '#64d2ff',
      magenta: '#bf5af2',
      tab_border: '#3a3a3a',
      tab_border_active: '#2997ff',
      tab_border_activity: '#30d158',
    },
    terminal: {
      background: '#1e1e1e',
      foreground: '#f2f2f2',
      cursor: '#f2f2f2',
      cursorAccent: '#1e1e1e',
      selectionBackground: '#3a3a3a',
      selectionForeground: '#f2f2f2',
      black: '#1e1e1e',
      red: '#ff6b6b',
      green: '#67f86f',
      yellow: '#fffc67',
      blue: '#6a76fb',
      magenta: '#fa73fd',
      cyan: '#4fcdb9',
      white: '#f2f2f2',
      brightBlack: '#808080',
      brightRed: '#ff8a80',
      brightGreen: '#87fc70',
      brightYellow: '#fffd7c',
      brightBlue: '#8a8aff',
      brightMagenta: '#fc8aff',
      brightCyan: '#6be5d0',
      brightWhite: '#ffffff',
    },
  },
];

/** @deprecated Use builtinThemes instead */
export const themes = builtinThemes;

export function isBuiltinTheme(id: string): boolean {
  return builtinThemes.some((t) => t.id === id);
}

export function getTheme(id: string, customThemes: Theme[] = []): Theme {
  return customThemes.find((t) => t.id === id)
    ?? builtinThemes.find((t) => t.id === id)
    ?? builtinThemes[0];
}

/** Relative luminance (0 = black, 1 = white) */
function luminance(hex: string): number {
  const h = hex.replace('#', '');
  const [r, g, b] = [
    parseInt(h.substring(0, 2), 16) / 255,
    parseInt(h.substring(2, 4), 16) / 255,
    parseInt(h.substring(4, 6), 16) / 255,
  ].map((c) => (c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4)));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function applyUiTheme(ui: UiColors): void {
  const root = document.documentElement;
  root.style.setProperty('--bg-dark', ui.bg_dark);
  root.style.setProperty('--bg-medium', ui.bg_medium);
  root.style.setProperty('--bg-light', ui.bg_light);
  root.style.setProperty('--fg', ui.fg);
  root.style.setProperty('--fg-dim', ui.fg_dim);
  root.style.setProperty('--accent', ui.accent);
  root.style.setProperty('--accent-hover', ui.accent_hover);
  root.style.setProperty('--green', ui.green);
  root.style.setProperty('--red', ui.red);
  root.style.setProperty('--yellow', ui.yellow);
  root.style.setProperty('--cyan', ui.cyan);
  root.style.setProperty('--magenta', ui.magenta);
  root.style.setProperty('--tab-border', ui.tab_border);
  root.style.setProperty('--tab-border-active', ui.tab_border_active);
  root.style.setProperty('--tab-border-activity', ui.tab_border_activity);

  // Logo adapts to theme: black "mai" wordmark on light themes, white on dark.
  // Two real assets (the wordmark is two-tone — "Term" stays periwinkle — so a
  // brightness() filter can't recolor it correctly).
  const isLight = luminance(ui.bg_dark) > 0.2;
  root.style.setProperty('--logo-url', isLight ? 'url(/logo-dark.png)' : 'url(/logo-light.png)');
  // Compact "m" mark (sidebar) — monochrome, so a matching black/white asset per theme.
  root.style.setProperty('--logo-mark-url', isLight ? 'url(/logo-mark-dark.png)' : 'url(/logo-mark-light.png)');
}
