import { EditorView } from '@codemirror/view';
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { tags } from '@lezer/highlight';
import type { Extension } from '@codemirror/state';
import type { Theme } from '$lib/themes';

/** Parse a hex color to [r, g, b] (0-255) */
function hexToRgb(hex: string): [number, number, number] {
  const h = hex.replace('#', '');
  return [
    parseInt(h.substring(0, 2), 16),
    parseInt(h.substring(2, 4), 16),
    parseInt(h.substring(4, 6), 16),
  ];
}

/** Relative luminance (0 = black, 1 = white) */
function luminance(hex: string): number {
  const [r, g, b] = hexToRgb(hex).map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/**
 * Editor theme using CSS variables — automatically updates when applyUiTheme()
 * changes the root CSS custom properties. No need to recreate the editor.
 *
 * Uses color-mix() for derived colors (selection, line highlights, etc.) so they
 * also react to variable changes.
 */
function buildCssVarEditorTheme(isDark: boolean): Extension {
  return EditorView.theme({
    '&': {
      color: 'var(--fg)',
      backgroundColor: 'var(--bg-dark)',
    },
    '.cm-content': {
      caretColor: 'var(--fg)',
    },
    '.cm-cursor, .cm-dropCursor': {
      borderLeftColor: 'var(--fg)',
    },
    '.cm-content ::selection': {
      backgroundColor: 'color-mix(in srgb, var(--accent) 40%, transparent) !important',
    },
    '.cm-panels': {
      backgroundColor: 'var(--bg-medium)',
      color: 'var(--fg)',
    },
    '.cm-panels.cm-panels-top': {
      borderBottom: '1px solid var(--bg-light)',
    },
    '.cm-panels.cm-panels-bottom': {
      borderTop: '1px solid var(--bg-light)',
    },
    '.cm-searchMatch': {
      backgroundColor: 'color-mix(in srgb, var(--accent) 35%, var(--bg-dark))',
      outline: '1px solid color-mix(in srgb, var(--accent) 60%, var(--bg-dark))',
    },
    '.cm-searchMatch.cm-searchMatch-selected': {
      backgroundColor: 'color-mix(in srgb, var(--yellow, #e0af68) 50%, var(--bg-dark))',
      outline: '2px solid var(--yellow, #e0af68)',
    },
    '.cm-activeLine': {
      backgroundColor: 'color-mix(in srgb, var(--bg-medium) 30%, var(--bg-dark))',
    },
    '.cm-selectionMatch': {
      backgroundColor: 'color-mix(in srgb, var(--accent) 15%, var(--bg-dark))',
    },
    '&.cm-focused .cm-matchingBracket, &.cm-focused .cm-nonmatchingBracket': {
      backgroundColor: 'color-mix(in srgb, var(--accent) 25%, var(--bg-dark))',
      outline: '1px solid color-mix(in srgb, var(--accent) 50%, var(--bg-dark))',
    },
    '.cm-gutters': {
      backgroundColor: 'var(--bg-dark)',
      color: 'var(--fg-dim)',
      borderRight: '1px solid var(--bg-light)',
    },
    '.cm-activeLineGutter': {
      backgroundColor: 'color-mix(in srgb, var(--bg-medium) 30%, var(--bg-dark))',
      color: 'var(--fg)',
    },
    '.cm-foldPlaceholder': {
      backgroundColor: 'var(--bg-medium)',
      color: 'var(--fg-dim)',
      border: 'none',
    },
    '.cm-tooltip': {
      backgroundColor: 'var(--bg-medium)',
      border: '1px solid var(--bg-light)',
      color: 'var(--fg)',
    },
    '.cm-tooltip .cm-tooltip-arrow:before': {
      borderTopColor: 'var(--bg-light)',
      borderBottomColor: 'var(--bg-light)',
    },
    '.cm-tooltip .cm-tooltip-arrow:after': {
      borderTopColor: 'var(--bg-medium)',
      borderBottomColor: 'var(--bg-medium)',
    },
    '.cm-tooltip-autocomplete': {
      '& > ul > li[aria-selected]': {
        backgroundColor: 'color-mix(in srgb, var(--accent) 25%, var(--bg-dark))',
      },
    },
  }, { dark: isDark });
}

/**
 * Build a CodeMirror 6 theme + syntax highlighting from an maiTerm Theme.
 *
 * The editor chrome (backgrounds, gutters, selection) uses CSS variables so it
 * automatically updates when the theme changes via applyUiTheme().
 *
 * Syntax highlighting uses resolved hex values from the theme — these won't
 * auto-update, but the editor would need to be recreated for a full theme
 * switch anyway (same as terminal tabs).
 */
export function buildEditorExtension(theme: Theme): Extension[] {
  const ui = theme.ui;
  const t = theme.terminal;
  const isDark = luminance(ui.bg_dark) < 0.2;

  const editorTheme = buildCssVarEditorTheme(isDark);

  const highlighting = syntaxHighlighting(HighlightStyle.define([
    { tag: tags.keyword, color: ui.magenta },
    { tag: [tags.name, tags.deleted, tags.character, tags.propertyName, tags.macroName], color: ui.fg },
    { tag: [tags.function(tags.variableName), tags.labelName], color: ui.accent },
    { tag: [tags.color, tags.constant(tags.name), tags.standard(tags.name)], color: t.yellow },
    { tag: [tags.definition(tags.name), tags.separator], color: ui.fg },
    { tag: [tags.typeName, tags.className, tags.number, tags.changed, tags.annotation, tags.modifier, tags.self, tags.namespace], color: ui.yellow },
    { tag: [tags.operator, tags.operatorKeyword, tags.url, tags.escape, tags.regexp, tags.link, tags.special(tags.string)], color: ui.cyan },
    { tag: [tags.meta, tags.comment], color: ui.fg_dim, fontStyle: 'italic' },
    { tag: tags.strong, fontWeight: 'bold' },
    { tag: tags.emphasis, fontStyle: 'italic' },
    { tag: tags.strikethrough, textDecoration: 'line-through' },
    { tag: tags.link, color: ui.cyan, textDecoration: 'underline' },
    { tag: tags.heading, fontWeight: 'bold', color: ui.accent },
    { tag: [tags.atom, tags.bool, tags.special(tags.variableName)], color: t.yellow },
    { tag: [tags.processingInstruction, tags.string, tags.inserted], color: ui.green },
    { tag: tags.invalid, color: ui.red },
  ]));

  return [editorTheme, highlighting];
}
