import { describe, expect, it } from 'vitest';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildSshCommand, cleanSshCommand, isInteractiveSshSession } from './commands';
import { remoteCodexAlias } from './codexShell';

describe.skipIf(process.platform === 'win32')('Codex in a bridged login shell', () => {
  for (const shell of ['/bin/bash', '/bin/zsh']) {
    it.skipIf(shell.endsWith('zsh') && process.platform !== 'darwin')(`routes through the launcher after ${shell} login`, () => {
      const home = mkdtempSync(join(tmpdir(), 'codex-shell-'));
      try {
        const dir = join(home, '.maiterm', 'codex');
        mkdirSync(dir, { recursive: true });
        copyFileSync('src-tauri/resources/codex/launch.sh', join(dir, 'launch.sh'));
        writeFileSync(join(home, 'codex'), '#!/bin/sh\nif [ "$1" = --help ]; then echo "  --no-daemon"; else printf "<%s>" "$@"; fi\n', { mode: 0o755 });
        const line = buildSshCommand('test-host', home, 'ab-12', { port: 1234, auth: 'scratch', codex: true });
        // Parse the command once in the local shell, then once on the SSH host.
        const remote = execFileSync('/bin/sh', ['-c', 'ssh() { shift 4; printf "%s" "$1"; }; ' + line], { encoding: 'utf8' });
        expect(cleanSshCommand(line)).toBe('test-host');
        expect(cleanSshCommand('ssh -t -o ControlMaster=no test-host ' + remote)).toBe('test-host');
        expect(isInteractiveSshSession(line)).toBe(true);
        expect(isInteractiveSshSession('ssh -t -o ControlMaster=no test-host ' + remote)).toBe(true);
        const result = execFileSync('/bin/sh', ['-c', remote], {
          env: { HOME: home, SHELL: shell, PATH: process.env.PATH },
          input: 'export PATH="$HOME:$PATH"\ncodex resume "two words"\n',
          encoding: 'utf8',
        });
        expect(result).toContain('<--no-daemon><resume><two words>');
      } finally {
        rmSync(home, { recursive: true, force: true });
      }
    });
  }

  it('passes through older codex without --no-daemon and skips non-TUI subcommands', () => {
    const home = mkdtempSync(join(tmpdir(), 'codex-shell-old-'));
    try {
      const dir = join(home, '.maiterm', 'codex');
      mkdirSync(dir, { recursive: true });
      copyFileSync('src-tauri/resources/codex/launch.sh', join(dir, 'launch.sh'));
      // Older codex (e.g. 0.161.0 on ews@nova) does not advertise --no-daemon in --help.
      writeFileSync(join(home, 'codex'), '#!/bin/sh\nif [ "$1" = --help ]; then echo "Usage: codex"; else printf "<%s>" "$@"; fi\n', { mode: 0o755 });
      const oldOut = execFileSync('/bin/sh', [join(dir, 'launch.sh'), 'resume', 'sid-1'], {
        env: { HOME: home, PATH: `${home}:${process.env.PATH}`, MAITERM_TAB_ID: 'ab-12' },
        encoding: 'utf8',
      });
      expect(oldOut).toBe('<resume><sid-1>');

      // Newer codex with --no-daemon still skips non-TUI subcommands like exec and mcp.
      writeFileSync(join(home, 'codex'), '#!/bin/sh\nif [ "$1" = --help ]; then echo "  --no-daemon"; else printf "<%s>" "$@"; fi\n', { mode: 0o755 });
      const execOut = execFileSync('/bin/sh', [join(dir, 'launch.sh'), 'exec', 'say ok'], {
        env: { HOME: home, PATH: `${home}:${process.env.PATH}`, MAITERM_TAB_ID: 'ab-12' },
        encoding: 'utf8',
      });
      expect(execOut).toBe('<exec><say ok>');
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it('remoteCodexAlias falls back to real codex when launch.sh has not been installed yet', () => {
    const home = mkdtempSync(join(tmpdir(), 'codex-shell-fallback-'));
    try {
      writeFileSync(join(home, 'codex'), '#!/bin/sh\nprintf "FALLBACK:"; printf "<%s>" "$@"\n', { mode: 0o755 });
      const out = execFileSync('/bin/bash', ['-c', `${remoteCodexAlias}; codex resume abc`], {
        env: { HOME: home, PATH: `${home}:${process.env.PATH}`, MAITERM_TAB_ID: 'ab-12' },
        encoding: 'utf8',
      });
      expect(out).toBe('FALLBACK:<resume><abc>');
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });
});
