/**
 * File and folder icons for the Files tree (docs/file-tree.md §3.2): one inline SVG sprite,
 * drawn once per window, and a lookup from a name to a glyph and a colour.
 *
 * - A sprite (`<symbol>` + `<use href>`) so 60 rows cost 60 tiny `<use>` nodes, not 60 copies
 *   of the path data. No third-party icon theme and no Nerd Font.
 * - Lookup is by exact file name first (Dockerfile, package.json), then by extension.
 * - Colours are theme tokens mixed toward `--fg` (the `--ft-ic-*` variables FilesColumn
 *   defines), so a glyph is always readable on the theme's background: QuickOpen's
 *   near-invisible `#292929` json icon is the failure this avoids.
 */

export type IconColor = 'blue' | 'cyan' | 'green' | 'yellow' | 'red' | 'magenta' | 'orange' | 'dim' | 'fg';

export interface IconRef {
  /** Symbol id in the sprite (`fti-…`). */
  id: string;
  color: IconColor;
}

// ── Glyphs ─────────────────────────────────────────────────────────────────────────────────
// All drawn on a 16×16 grid. Documents share one outline with a folded corner; most
// languages are told apart by a two- or three-letter label, which is what survives at 14 px.

const DOC = '<path d="M3.5 1.5h6l3 3v10h-9z" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/><path d="M9.5 1.5v3h3" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>';

function label(text: string): string {
  const size = text.length >= 3 ? 4.6 : 5.6;
  return `<rect x="1" y="7.6" width="${text.length >= 3 ? 12.6 : 10.4}" height="6.4" rx="1.2" fill="currentColor"/>`
    + `<text x="${text.length >= 3 ? 7.3 : 6.2}" y="12.4" text-anchor="middle" font-family="-apple-system,Segoe UI,sans-serif" font-weight="700" font-size="${size}" style="fill:var(--bg-dark)">${text}</text>`;
}

const docWith = (inner: string) => DOC + inner;
const lettered = (text: string) => DOC + label(text);

const GLYPHS: Record<string, string> = {
  folder: '<path d="M1.5 3.5h4.2l1.5 1.5h7.3v8.5h-13z" fill="currentColor" fill-opacity=".85"/>',
  'folder-open': '<path d="M1.5 3.5h4.2l1.5 1.5h6.3v2H4l-2.5 6.5z" fill="currentColor" fill-opacity=".6"/><path d="M3.6 7.5h11.4l-2.4 6H1.2z" fill="currentColor" fill-opacity=".9"/>',
  'folder-link': '<path d="M1.5 3.5h4.2l1.5 1.5h7.3v8.5h-13z" fill="currentColor" fill-opacity=".55"/><path d="M6 11.5l3-3m-2.4 0H9v2.4" fill="none" style="stroke:var(--bg-dark)" stroke-width="1.3" stroke-linecap="round"/>',
  file: DOC,
  text: docWith('<path d="M5.5 7.5h5M5.5 9.5h5M5.5 11.5h3.5" stroke="currentColor" stroke-width="1" stroke-linecap="round"/>'),
  link: docWith('<path d="M5.8 12l3.4-3.4M6.8 8.6h2.4V11" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>'),
  other: docWith('<circle cx="8" cy="10" r="1.6" fill="currentColor"/>'),
  ts: lettered('TS'),
  js: lettered('JS'),
  jsx: docWith('<ellipse cx="8" cy="10" rx="4" ry="1.5" fill="none" stroke="currentColor" stroke-width=".9"/><ellipse cx="8" cy="10" rx="4" ry="1.5" fill="none" stroke="currentColor" stroke-width=".9" transform="rotate(60 8 10)"/><ellipse cx="8" cy="10" rx="4" ry="1.5" fill="none" stroke="currentColor" stroke-width=".9" transform="rotate(-60 8 10)"/>'),
  svelte: '<path d="M11.8 2.6c-1.3-1.2-3.4-1.3-4.8-.3L4.3 4.1c-1.4 1-1.8 2.9-1 4.4-.4.6-.5 1.4-.4 2.1.3 1.9 2 3.2 3.9 3 .6-.1 1.2-.3 1.7-.6l2.7-1.8c1.4-1 1.8-2.9 1-4.4.4-.6.5-1.4.4-2.1-.1-.8-.5-1.5-.8-2.1z" fill="currentColor"/><path d="M6.4 11.3c-.8.3-1.7-.1-2-.9a1.4 1.4 0 0 1 .6-1.6l2.7-1.8M9.6 4.7c.8-.3 1.7.1 2 .9.2.6 0 1.3-.6 1.6L8.3 9" fill="none" style="stroke:var(--bg-dark)" stroke-width="1"/>',
  vue: '<path d="M1.5 2.5h3L8 8.6l3.5-6.1h3L8 13.8z" fill="currentColor"/><path d="M4.5 2.5H6.6L8 5l1.4-2.5h2.1L8 8.6z" style="fill:var(--bg-dark)" fill-opacity=".45"/>',
  html: docWith('<path d="M6.5 8l-2 2 2 2M9.5 8l2 2-2 2" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>'),
  css: lettered('CSS'),
  scss: lettered('SC'),
  json: docWith('<path d="M6.6 7.4c-1 0-1 .7-1 1.4s-.3 1.2-.9 1.2c.6 0 .9.5.9 1.2s0 1.4 1 1.4M9.4 7.4c1 0 1 .7 1 1.4s.3 1.2.9 1.2c-.6 0-.9.5-.9 1.2s0 1.4-1 1.4" fill="none" stroke="currentColor" stroke-width="1" stroke-linecap="round"/>'),
  md: '<rect x="1" y="3.5" width="14" height="9" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.1"/><path d="M3.5 10.5v-5l2 2.2 2-2.2v5M10.5 5.5v5m-1.8-1.8l1.8 1.8 1.8-1.8" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"/>',
  rust: lettered('RS'),
  go: lettered('GO'),
  py: lettered('PY'),
  rb: lettered('RB'),
  java: lettered('JV'),
  kt: lettered('KT'),
  swift: lettered('SW'),
  c: lettered('C'),
  cpp: lettered('C++'),
  h: lettered('H'),
  cs: lettered('C#'),
  php: lettered('PHP'),
  lua: lettered('LUA'),
  dart: lettered('DT'),
  ex: lettered('EX'),
  sql: lettered('SQL'),
  sh: docWith('<path d="M5 8l2 1.8L5 11.6M8 12h3" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>'),
  yaml: lettered('YML'),
  toml: lettered('TML'),
  xml: lettered('XML'),
  csv: docWith('<path d="M5 7.5h6v5H5zM5 10h6M8 7.5v5" fill="none" stroke="currentColor" stroke-width=".9"/>'),
  image: '<rect x="1.5" y="2.5" width="13" height="11" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.1"/><circle cx="5.2" cy="6" r="1.3" fill="currentColor"/><path d="M2 12.5l3.8-3.8 2.4 2.4 2.3-2.6 3.5 4" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>',
  svg: docWith('<circle cx="6.3" cy="10.5" r="1.7" fill="none" stroke="currentColor" stroke-width="1"/><path d="M8.6 12.3l2-3.6 1.4 3.6z" fill="currentColor"/>'),
  pdf: lettered('PDF'),
  archive: docWith('<path d="M7 2v1.3M8 3.3v1.3M7 4.6v1.3M8 5.9v1.3" stroke="currentColor" stroke-width="1"/><rect x="6.3" y="8" width="2.4" height="3" rx=".5" fill="currentColor"/>'),
  lock: '<rect x="3" y="7" width="10" height="7.5" rx="1.4" fill="currentColor"/><path d="M5.2 7V5.1a2.8 2.8 0 0 1 5.6 0V7" fill="none" stroke="currentColor" stroke-width="1.4"/>',
  env: docWith('<circle cx="6.4" cy="10.2" r="1.5" fill="none" stroke="currentColor" stroke-width="1"/><path d="M7.8 10.2h3.4m-1 0v1.4" stroke="currentColor" stroke-width="1" stroke-linecap="round"/>'),
  git: '<path d="M8 1.2l6.8 6.8L8 14.8 1.2 8z" fill="currentColor"/><circle cx="8" cy="5" r="1" style="fill:var(--bg-dark)"/><circle cx="8" cy="11" r="1" style="fill:var(--bg-dark)"/><circle cx="10.6" cy="8" r="1" style="fill:var(--bg-dark)"/><path d="M8 5v6M8 5.6l2.6 2.4" style="stroke:var(--bg-dark)" stroke-width=".9"/>',
  docker: '<path d="M1.2 8.3h11.5c.6-1 1.6-1.4 2.6-1.1-.3 3.4-3 6.3-7.6 6.3-3.6 0-5.8-2-6.5-5.2z" fill="currentColor"/><path d="M3 5.6h2v2H3zM5.4 5.6h2v2h-2zM7.8 5.6h2v2h-2zM5.4 3.2h2v2h-2z" fill="currentColor"/>',
  npm: '<rect x="1.5" y="4.5" width="13" height="7" fill="currentColor"/><path d="M3.3 10V6.2h2.5V10M4.6 6.2V10M6.8 10V6.2h2.4v2.6H8M10.1 10V6.2h2.6V10m-1.3-3.8V10" fill="none" style="stroke:var(--bg-dark)" stroke-width=".9"/>',
  license: docWith('<circle cx="8" cy="9.4" r="2" fill="none" stroke="currentColor" stroke-width="1"/><path d="M6.9 11.1L6.3 13.6l1.7-.8 1.7.8-.6-2.5" fill="currentColor"/>'),
  readme: '<circle cx="8" cy="8" r="6.5" fill="none" stroke="currentColor" stroke-width="1.2"/><circle cx="8" cy="4.9" r=".95" fill="currentColor"/><path d="M8 7.2v4.6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>',
  config: '<circle cx="8" cy="8" r="2.1" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M8 1.6v2M8 12.4v2M1.6 8h2M12.4 8h2M3.5 3.5l1.4 1.4M11.1 11.1l1.4 1.4M3.5 12.5l1.4-1.4M11.1 4.9l1.4-1.4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>',
  test: docWith('<path d="M6.5 7.3v2.2l-2 3.2h7l-2-3.2V7.3M6 7.3h4" fill="none" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/>'),
  font: docWith('<path d="M5.3 12.5L8 7l2.7 5.5M6.2 10.7h3.6" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"/>'),
  audio: docWith('<path d="M7.3 12V7.6l3.4-.8v4.4" fill="none" stroke="currentColor" stroke-width="1"/><circle cx="6.4" cy="12" r="1.1" fill="currentColor"/><circle cx="9.8" cy="11.2" r="1.1" fill="currentColor"/>'),
  video: docWith('<path d="M6.5 7.8v4.4l3.8-2.2z" fill="currentColor"/>'),
  binary: docWith('<path d="M5.4 7.6v2.4M10.6 7.6v2.4M7 10.4h2v2.4H7z" fill="none" stroke="currentColor" stroke-width="1"/>'),
  log: docWith('<path d="M5.5 7.5h1M7.5 7.5h3M5.5 9.5h1M7.5 9.5h3M5.5 11.5h1M7.5 11.5h2" stroke="currentColor" stroke-width="1" stroke-linecap="round"/>'),
  make: lettered('MK'),
  editorconfig: lettered('EC'),
};

/** The sprite, drawn once per window (hidden, zero-size). */
export const ICON_SPRITE = `<svg xmlns="http://www.w3.org/2000/svg" style="position:absolute;width:0;height:0;overflow:hidden" aria-hidden="true">${
  Object.entries(GLYPHS).map(([id, body]) => `<symbol id="fti-${id}" viewBox="0 0 16 16">${body}</symbol>`).join('')
}</svg>`;

// ── Lookup ─────────────────────────────────────────────────────────────────────────────────

const BY_NAME: Record<string, IconRef> = {
  'package.json': { id: 'fti-npm', color: 'red' },
  'package-lock.json': { id: 'fti-lock', color: 'dim' },
  'yarn.lock': { id: 'fti-lock', color: 'dim' },
  'pnpm-lock.yaml': { id: 'fti-lock', color: 'dim' },
  'cargo.lock': { id: 'fti-lock', color: 'dim' },
  'bun.lockb': { id: 'fti-lock', color: 'dim' },
  'cargo.toml': { id: 'fti-rust', color: 'orange' },
  dockerfile: { id: 'fti-docker', color: 'cyan' },
  'docker-compose.yml': { id: 'fti-docker', color: 'cyan' },
  'docker-compose.yaml': { id: 'fti-docker', color: 'cyan' },
  '.gitignore': { id: 'fti-git', color: 'orange' },
  '.gitattributes': { id: 'fti-git', color: 'orange' },
  '.gitmodules': { id: 'fti-git', color: 'orange' },
  '.dockerignore': { id: 'fti-docker', color: 'dim' },
  '.editorconfig': { id: 'fti-editorconfig', color: 'dim' },
  '.npmrc': { id: 'fti-npm', color: 'dim' },
  makefile: { id: 'fti-make', color: 'orange' },
  license: { id: 'fti-license', color: 'yellow' },
  'license.md': { id: 'fti-license', color: 'yellow' },
  'license.txt': { id: 'fti-license', color: 'yellow' },
  readme: { id: 'fti-readme', color: 'blue' },
  'readme.md': { id: 'fti-readme', color: 'blue' },
  'claude.md': { id: 'fti-readme', color: 'orange' },
  'agents.md': { id: 'fti-readme', color: 'orange' },
  'tsconfig.json': { id: 'fti-config', color: 'blue' },
  'vite.config.ts': { id: 'fti-config', color: 'magenta' },
  'vitest.config.ts': { id: 'fti-config', color: 'green' },
  'svelte.config.js': { id: 'fti-config', color: 'orange' },
};

const BY_EXT: Record<string, IconRef> = {
  ts: { id: 'fti-ts', color: 'blue' }, mts: { id: 'fti-ts', color: 'blue' }, cts: { id: 'fti-ts', color: 'blue' },
  tsx: { id: 'fti-jsx', color: 'blue' }, jsx: { id: 'fti-jsx', color: 'cyan' },
  js: { id: 'fti-js', color: 'yellow' }, mjs: { id: 'fti-js', color: 'yellow' }, cjs: { id: 'fti-js', color: 'yellow' },
  svelte: { id: 'fti-svelte', color: 'orange' }, vue: { id: 'fti-vue', color: 'green' },
  html: { id: 'fti-html', color: 'orange' }, htm: { id: 'fti-html', color: 'orange' },
  css: { id: 'fti-css', color: 'blue' }, scss: { id: 'fti-scss', color: 'magenta' }, sass: { id: 'fti-scss', color: 'magenta' }, less: { id: 'fti-css', color: 'blue' },
  json: { id: 'fti-json', color: 'yellow' }, jsonc: { id: 'fti-json', color: 'yellow' }, json5: { id: 'fti-json', color: 'yellow' },
  md: { id: 'fti-md', color: 'blue' }, mdx: { id: 'fti-md', color: 'blue' }, markdown: { id: 'fti-md', color: 'blue' },
  txt: { id: 'fti-text', color: 'dim' }, rtf: { id: 'fti-text', color: 'dim' },
  rs: { id: 'fti-rust', color: 'orange' }, go: { id: 'fti-go', color: 'cyan' }, py: { id: 'fti-py', color: 'yellow' },
  rb: { id: 'fti-rb', color: 'red' }, java: { id: 'fti-java', color: 'red' }, kt: { id: 'fti-kt', color: 'magenta' },
  kts: { id: 'fti-kt', color: 'magenta' }, swift: { id: 'fti-swift', color: 'orange' },
  c: { id: 'fti-c', color: 'blue' }, cc: { id: 'fti-cpp', color: 'blue' }, cpp: { id: 'fti-cpp', color: 'blue' }, cxx: { id: 'fti-cpp', color: 'blue' },
  h: { id: 'fti-h', color: 'magenta' }, hpp: { id: 'fti-h', color: 'magenta' }, cs: { id: 'fti-cs', color: 'green' },
  php: { id: 'fti-php', color: 'magenta' }, lua: { id: 'fti-lua', color: 'blue' }, dart: { id: 'fti-dart', color: 'cyan' },
  ex: { id: 'fti-ex', color: 'magenta' }, exs: { id: 'fti-ex', color: 'magenta' },
  sql: { id: 'fti-sql', color: 'orange' },
  sh: { id: 'fti-sh', color: 'green' }, bash: { id: 'fti-sh', color: 'green' }, zsh: { id: 'fti-sh', color: 'green' }, fish: { id: 'fti-sh', color: 'green' },
  ps1: { id: 'fti-sh', color: 'blue' }, bat: { id: 'fti-sh', color: 'dim' }, cmd: { id: 'fti-sh', color: 'dim' },
  yml: { id: 'fti-yaml', color: 'magenta' }, yaml: { id: 'fti-yaml', color: 'magenta' }, toml: { id: 'fti-toml', color: 'orange' },
  xml: { id: 'fti-xml', color: 'orange' }, plist: { id: 'fti-xml', color: 'dim' }, csv: { id: 'fti-csv', color: 'green' }, tsv: { id: 'fti-csv', color: 'green' },
  ini: { id: 'fti-config', color: 'dim' }, conf: { id: 'fti-config', color: 'dim' }, cfg: { id: 'fti-config', color: 'dim' },
  env: { id: 'fti-env', color: 'yellow' },
  png: { id: 'fti-image', color: 'magenta' }, jpg: { id: 'fti-image', color: 'magenta' }, jpeg: { id: 'fti-image', color: 'magenta' },
  gif: { id: 'fti-image', color: 'magenta' }, webp: { id: 'fti-image', color: 'magenta' }, ico: { id: 'fti-image', color: 'magenta' },
  bmp: { id: 'fti-image', color: 'magenta' }, avif: { id: 'fti-image', color: 'magenta' }, icns: { id: 'fti-image', color: 'magenta' },
  svg: { id: 'fti-svg', color: 'yellow' }, pdf: { id: 'fti-pdf', color: 'red' },
  zip: { id: 'fti-archive', color: 'yellow' }, gz: { id: 'fti-archive', color: 'yellow' }, tgz: { id: 'fti-archive', color: 'yellow' },
  tar: { id: 'fti-archive', color: 'yellow' }, bz2: { id: 'fti-archive', color: 'yellow' }, xz: { id: 'fti-archive', color: 'yellow' },
  '7z': { id: 'fti-archive', color: 'yellow' }, rar: { id: 'fti-archive', color: 'yellow' }, dmg: { id: 'fti-archive', color: 'dim' },
  lock: { id: 'fti-lock', color: 'dim' },
  ttf: { id: 'fti-font', color: 'red' }, otf: { id: 'fti-font', color: 'red' }, woff: { id: 'fti-font', color: 'red' }, woff2: { id: 'fti-font', color: 'red' },
  mp3: { id: 'fti-audio', color: 'cyan' }, wav: { id: 'fti-audio', color: 'cyan' }, flac: { id: 'fti-audio', color: 'cyan' }, ogg: { id: 'fti-audio', color: 'cyan' },
  mp4: { id: 'fti-video', color: 'magenta' }, mov: { id: 'fti-video', color: 'magenta' }, webm: { id: 'fti-video', color: 'magenta' }, mkv: { id: 'fti-video', color: 'magenta' },
  exe: { id: 'fti-binary', color: 'dim' }, dll: { id: 'fti-binary', color: 'dim' }, so: { id: 'fti-binary', color: 'dim' },
  dylib: { id: 'fti-binary', color: 'dim' }, wasm: { id: 'fti-binary', color: 'magenta' }, bin: { id: 'fti-binary', color: 'dim' }, o: { id: 'fti-binary', color: 'dim' },
  log: { id: 'fti-log', color: 'dim' }, mk: { id: 'fti-make', color: 'orange' },
};

const TEST_RE = /\.(test|spec)\.[a-z]+$/i;

/**
 * The glyph for a row. `kind` is the entry kind ('f' file, 'd' folder, 'l' link, 'L' link to
 * a folder, 'o' other); `open` is whether a folder is expanded.
 */
export function iconFor(name: string, kind: string, open: boolean): IconRef {
  if (kind === 'd') return { id: open ? 'fti-folder-open' : 'fti-folder', color: 'blue' };
  if (kind === 'L') return { id: open ? 'fti-folder-open' : 'fti-folder-link', color: 'cyan' };
  if (kind === 'o') return { id: 'fti-other', color: 'dim' };
  const lower = name.toLowerCase();
  const exact = BY_NAME[lower];
  if (exact) return exact;
  if (lower.startsWith('.env')) return { id: 'fti-env', color: 'yellow' };
  if (TEST_RE.test(lower)) return { id: 'fti-test', color: 'green' };
  const dot = lower.lastIndexOf('.');
  const ref = dot > 0 ? BY_EXT[lower.slice(dot + 1)] : undefined;
  if (ref) return ref;
  if (kind === 'l') return { id: 'fti-link', color: 'cyan' };
  return { id: 'fti-file', color: 'dim' };
}

/** How many glyphs the sprite carries (asserted in tests: the spec budgets about 45). */
export const GLYPH_COUNT = Object.keys(GLYPHS).length;
