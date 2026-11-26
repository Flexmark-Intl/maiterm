import { describe, expect, it } from 'vitest';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildSshCommand, cleanSshCommand, isInteractiveSshSession } from './commands';
import { remoteCodexAlias, remoteCodexLogin } from './codexShell';

describe.skipIf(process.platform === 'win32')('Codex in a bridged login shell', () => {
  it('remoteCodexLogin contains no single quotes or backslashes (safe for local fish)', () => {
    const login = remoteCodexLogin();
    expect(login).not.toContain("'");
    expect(login).not.toContain('\\');
    const line = buildSshCommand('test-host', '/srv/app', 'ab-12', { port: 1234, auth: 'scratch', codex: true });
    expect(line).not.toContain("'\\''");
  });

  for (const shell of ['/bin/bash', '/bin/zsh']) {
    it.skipIf(shell.endsWith('zsh') && process.platform !== 'darwin')(`routes through the launcher after ${shell} login`, () => {
      const home = mkdtempSync(join(tmpdir(), 'codex-shell-'));
      try {
        const dir = join(home, '.maiterm', 'codex');
        mkdirSync(dir, { recursive: true });
        copyFileSync('src-tauri/resources/codex/launch.sh', join(dir, 'launch.sh'));
        copyFileSync('src-tauri/resources/codex/login.sh', join(dir, 'login.sh'));
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

  it('falls back cleanly to exec $SHELL -l when login.sh is not installed yet', () => {
    const home = mkdtempSync(join(tmpdir(), 'codex-shell-nologin-'));
    try {
      const line = buildSshCommand('test-host', home, 'ab-12', { port: 1234, auth: 'scratch', codex: true });
      const remote = execFileSync('/bin/sh', ['-c', 'ssh() { shift 4; printf "%s" "$1"; }; ' + line], { encoding: 'utf8' });
      const result = execFileSync('/bin/sh', ['-c', remote], {
        env: { HOME: home, SHELL: '/bin/bash', PATH: process.env.PATH },
        input: 'printf "TAB=%s" "$MAITERM_TAB_ID"\n',
        encoding: 'utf8',
      });
      expect(result).toContain('TAB=ab-12');
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

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

  for (const shell of ['/bin/bash', '/bin/zsh']) {
    it.skipIf(shell.endsWith('zsh') && process.platform !== 'darwin')(`remoteCodexAlias replaces pre-existing alias and falls back cleanly in ${shell}`, () => {
      const home = mkdtempSync(join(tmpdir(), 'codex-shell-alias-'));
      try {
        writeFileSync(join(home, 'codex'), '#!/bin/sh\nif [ "$1" = --help ]; then echo "  --no-daemon"; else printf "REAL:"; printf "<%s>" "$@"; fi\n', { mode: 0o755 });
        const args = shell.endsWith('zsh') ? ['-f'] : ['--norc'];
        const prelude = shell.endsWith('bash') ? 'shopt -s expand_aliases\n' : '';
        const script = `${prelude}alias codex='echo OLD'\nexport MAITERM_TAB_ID=ab-12\n ${remoteCodexAlias}\ncodex resume "two words"\n`;
        const fallbackOut = execFileSync(shell, args, {
          env: { HOME: home, PATH: `${home}:${process.env.PATH}` },
          input: script,
          encoding: 'utf8',
        });
        expect(fallbackOut.trim()).toBe('REAL:<resume><two words>');

        // Once setup installs launch.sh, the same alias routes through it.
        const dir = join(home, '.maiterm', 'codex');
        mkdirSync(dir, { recursive: true });
        copyFileSync('src-tauri/resources/codex/launch.sh', join(dir, 'launch.sh'));
        const routedOut = execFileSync(shell, args, {
          env: { HOME: home, PATH: `${home}:${process.env.PATH}` },
          input: script,
          encoding: 'utf8',
        });
        expect(routedOut.trim()).toBe('REAL:<--no-daemon><resume><two words>');
      } finally {
        rmSync(home, { recursive: true, force: true });
      }
    });
  }
});
