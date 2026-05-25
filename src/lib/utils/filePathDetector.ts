import type { ILink, ILinkProvider, Terminal } from '@xterm/xterm';

/** Extensions we recognize as files (to reduce false positives) */
const FILE_EXTENSIONS = new Set([
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'mts', 'cts',
  'rs', 'py', 'rb', 'go', 'java', 'c', 'cpp', 'h', 'hpp',
  'html', 'htm', 'css', 'scss', 'less', 'svelte', 'vue',
  'json', 'yaml', 'yml', 'toml', 'xml', 'svg',
  'md', 'mdx', 'txt', 'log', 'csv',
  'sql', 'sh', 'bash', 'zsh', 'fish',
  'conf', 'cfg', 'ini', 'env',
  'lock', 'dockerfile', 'makefile',
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'avif',
]);

/** Well-known extensionless filenames */
const KNOWN_FILENAMES = new Set([
  'Makefile', 'Dockerfile', 'Containerfile', 'Vagrantfile', 'Procfile',
  'Gemfile', 'Rakefile', 'Brewfile', 'Justfile', 'Taskfile',
  'LICENSE', 'LICENCE', 'COPYING', 'AUTHORS', 'CONTRIBUTORS',
  'CHANGELOG', 'CHANGES', 'HISTORY', 'NEWS',
  'README', 'INSTALL', 'TODO', 'HACKING',
  'CMakeLists.txt',
  'configure', 'gradlew',
]);

function hasKnownExtension(path: string): boolean {
  const dot = path.lastIndexOf('.');
  if (dot === -1) return false;
  const ext = path.slice(dot + 1).toLowerCase();
  return FILE_EXTENSIONS.has(ext);
}

function isDotfile(path: string): boolean {
  const name = path.includes('/') ? path.split('/').pop()! : path;
  return name.startsWith('.') && name.length > 1 && name !== '..';
}

function isKnownFilename(path: string): boolean {
  const name = path.includes('/') ? path.split('/').pop()! : path;
  return KNOWN_FILENAMES.has(name);
}

function isLikelyFile(path: string): boolean {
  if (hasKnownExtension(path)) return true;
  if (isDotfile(path)) return true;
  if (isKnownFilename(path)) return true;
  if (path.includes('/')) return true;
  return false;
}

/**
 * Single pre-compiled regex combining all file path patterns.
 * Uses alternation with named-style groups (all captured in group 1 via outer parens).
 * Order matters — more specific patterns first.
 *
 * Note: ls -l output is NOT handled here. The `l` shell function emits
 * OSC 8 hyperlinks which xterm.js handles natively via linkHandler.
 */
const COMBINED_PATTERN = new RegExp(
  '(?:^|[\\s\'"({\\[,:])(' +
    // Absolute paths: /foo/bar or ~/foo/bar
    '[~\\/][\\w.\\-\\/]+' +
    '|' +
    // Relative paths: ./foo, ../foo, or dir/file patterns
    '\\.\\.\\/.+?|\\.\\/.+?|[\\w][\\w.\\-]*\\/[\\w.\\-\\/]+' +
    '|' +
    // Bare filenames with extension: package.json, CHANGELOG.md
    '[.\\w][\\w.\\-]*\\.\\w+' +
    '|' +
    // Dotfiles without extension: .gitignore, .bashrc, .env
    '\\.[a-zA-Z][\\w.\\-]*' +
    '|' +
    // Known extensionless filenames
    '(?:Makefile|Dockerfile|Containerfile|Vagrantfile|Procfile|Gemfile|Rakefile|Brewfile|Justfile|Taskfile|LICENSE|LICENCE|COPYING|AUTHORS|CONTRIBUTORS|CHANGELOG|CHANGES|HISTORY|NEWS|README|INSTALL|TODO|HACKING|configure|gradlew)' +
  ')(?=[\\s\'"\\)\\],:;]|$)',
  'g'
);

/**
 * Creates a link provider that detects file paths in terminal output.
 * Registered once at terminal creation — the pre-compiled regex is cheap
 * enough to run on every hovered line (one translateToString + one exec).
 *
 * ls -l file linking is handled separately via OSC 8 hyperlinks
 * (the `l` shell function + xterm.js linkHandler).
 */
export function createFilePathLinkProvider(
  terminal: Terminal,
  onActivate: (path: string, event: MouseEvent) => void,
): { dispose: () => void } {
  const provider: ILinkProvider = {
    provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void) {
      const line = terminal.buffer.active.getLine(bufferLineNumber - 1);
      if (!line) {
        callback(undefined);
        return;
      }

      const text = line.translateToString(true);
      const links: ILink[] = [];
      const seen = new Set<string>();

      // Reset lastIndex for the global regex
      COMBINED_PATTERN.lastIndex = 0;
      let match: RegExpExecArray | null;

      while ((match = COMBINED_PATTERN.exec(text)) !== null) {
        const rawPath = match[1];
        if (!rawPath) continue;

        // Prose and LLM output routinely end a sentence right after a path
        // ("Saved to /opt/foo/bar.md."). The path char class needs '.' for
        // extensions, so it greedily swallows the trailing sentence period.
        // Strip trailing dots so the link (and its underline range) stop at
        // the real filename. Almost no real file ends in a dot.
        const filePath = rawPath.replace(/\.+$/, '');
        if (!filePath || !isLikelyFile(filePath)) continue;

        const startIndex = match.index + match[0].indexOf(rawPath);
        const key = `${startIndex}:${filePath.length}`;
        if (seen.has(key)) continue;
        seen.add(key);

        links.push({
          range: {
            start: { x: startIndex + 1, y: bufferLineNumber },
            end: { x: startIndex + filePath.length + 1, y: bufferLineNumber },
          },
          text: filePath,
          activate: (event: MouseEvent) => onActivate(filePath, event),
        });
      }

      callback(links.length > 0 ? links : undefined);
    },
  };

  return terminal.registerLinkProvider(provider);
}
