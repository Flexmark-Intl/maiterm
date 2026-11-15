import { describe, expect, it } from 'vitest';
import { execFileSync } from 'node:child_process';
import { buildSshCommand, cleanSshCommand, remoteCommandIndex, sshArgsOf } from './commands';
import { connectArgs, sameSshHost } from '$lib/filetree/remote';
import { hostLabel } from '$lib/followUps/model';
import { joinWords, splitWords } from '$lib/utils/shellWords';
import { HARD_WORDS } from '$lib/utils/shellWords.test';

const TAB = 'a1b2c3d4-5e6f-7788-99aa-bbccddeeff00';

/** Where the remote command starts: the same table is asserted against `watch::parse_ssh` in
 *  watch.rs (`remote_command_index`). */
export const REMOTE_AT: [string, number][] = [
  ['ews@nova', 1],
  ['ews@nova uptime', 1],
  ['-p 2222 ews@nova uptime -l', 3],
  ['ews@nova -o SetEnv=MAITERM_PROFILE=dev', 3],
  ["ews@nova -o 'SendEnv=MAITERM_*' tmux", 3],
  ['nova -oSendEnv=MAITERM_X -p 2222', 4],
  ['-xCp2222 nova -t -J bastion ls', 5],
  ['-- nova uptime', 2],
  ['nova -- -p 1', 2],
  ['-p 2222', 2],
];

/** The argv a POSIX shell gives the program when `line` is typed into it — what Rust then reads
 *  back off the process (`KERN_PROCARGS2`, `/proc/<pid>/cmdline`). */
function argvOf(line: string): string[] {
  const out = execFileSync('/bin/sh', ['-c', `printf '%s\\0' ${line}`], { encoding: 'utf8' });
  return out.split('\0').slice(0, -1);
}

// A tab's ssh with a word a whitespace split mangles, around the whole loop: stored, typed into
// the shell by `buildSshCommand`, read off the ssh's argv and joined as shell words (Rust's
// `ssh_command_line`), cleaned, and read again by everything that hands it to maiTerm's own ssh.
describe('ssh args keep their words from the stored command back to the stored command', () => {
  for (const hard of HARD_WORDS.filter(Boolean)) {
    it(JSON.stringify(hard), () => {
      const cert = `CertificateFile=${hard}`;
      const args = ['-i', hard, '-o', cert, 'ews@nova'];
      const stored = joinWords(args);

      const line = buildSshCommand(stored, '/srv/my app', TAB, { port: 28123, auth: 'tok-123' });
      const argv = argvOf(line);
      expect(argv.slice(0, 4)).toEqual(['ssh', '-t', '-o', 'ControlMaster=no']);
      expect(argv.slice(4, 9)).toEqual(args);
      expect(argv[9]).toContain('MAITERM_AUTH=tok-123');

      const captured = joinWords(argv);
      const cleaned = cleanSshCommand(captured);
      expect(cleaned).not.toContain('MAITERM');
      expect(cleaned).toBe(stored);
      expect(splitWords(sshArgsOf(cleaned))).toEqual(args);

      // One host, however it is spelled, and the same key Rust's `watch::connect_args` makes.
      expect(connectArgs(captured)).toBe(stored);
      expect(connectArgs(cleaned)).toBe(stored);
      expect(hostLabel(connectArgs(captured))).toBe('ews@nova');

      // And the next clone types the same argv again.
      expect(argvOf(buildSshCommand(cleaned, '/srv/my app', TAB)).slice(4, 9)).toEqual(args);
    });
  }

  it('keeps a hand-typed command in the spelling its human typed', () => {
    expect(cleanSshCommand('ssh -t -x -i "/Users/me/My Keys/id" -x ~/x ews@nova')).toBe('-t -x -i "/Users/me/My Keys/id" -x ~/x ews@nova');
    // A `-t` or ControlMaster inside a quoted word is part of that word.
    expect(cleanSshCommand("ssh -i '/a -t b' -o 'SetEnv=X=-o ControlMaster=no' h")).toBe("-i '/a -t b' -o 'SetEnv=X=-o ControlMaster=no' h");
    expect(sshArgsOf("autossh -M 0 -f -i '/a -M b' h")).toBe("-i '/a -M b' h");
  });

  // maiTerm's remote command can reach ssh as several words: a wrapper such as
  // `ssh(){ command ssh $@; }` re-splits it with its unquoted `$@` (the session still works:
  // ssh joins its args again), and cmd.exe on Windows doesn't treat `'` as quoting at all.
  // Joined as shell words, each piece is quoted on its own; none of it may be stored.
  describe('strips the remote command however ssh received it', () => {
    const bridge = { port: 28123, auth: 'tok-123' };
    const wrapped = (line: string) =>
      execFileSync('/bin/bash', ['-c', `set -f; f(){ printf '%s\\0' $@; }; f ${line}`], { encoding: 'utf8' }).split('\0').slice(0, -1);
    for (const cwd of ['/srv', '/srv/my app', null]) {
      it(`through an unquoted $@, cwd ${cwd}`, () => {
        const argv = wrapped(buildSshCommand('-x ews@nova', cwd, TAB, bridge));
        expect(argv.length).toBeGreaterThan(8);
        expect(cleanSshCommand(joinWords(argv))).toBe('-x ews@nova');
        expect(cleanSshCommand(argv.join(' '))).toBe('-x ews@nova');
      });
      it(`through cmd.exe, cwd ${cwd}`, () => {
        const argv = buildSshCommand('-x ews@nova', cwd, TAB, bridge).split(/\s+/);
        const cleaned = cleanSshCommand(joinWords(argv));
        expect(cleaned).not.toContain('MAITERM');
        expect(cleaned).toBe('-x ews@nova');
      });
    }
    it('with the account fragment and no bridge', () => {
      const acct = `[ -r ~/.maiterm/tokens/tok-${TAB} ] && . ~/.maiterm/tokens/tok-${TAB}; rm -f ~/.maiterm/tokens/tok-${TAB}`;
      const argv = wrapped(buildSshCommand('ews@nova', '/srv', TAB, null, acct));
      expect(cleanSshCommand(joinWords(argv))).toBe('ews@nova');
    });
    it("keeps the user's own options that name maiTerm env, and the host after them", () => {
      for (const stored of ["-o 'SendEnv=MAITERM_*' ews@nova", '-o SetEnv=MAITERM_PROFILE=dev ews@nova']) {
        const argv = argvOf(buildSshCommand(stored, '/srv', TAB, bridge));
        const cleaned = cleanSshCommand(joinWords(argv));
        expect(cleaned).toBe(stored);
        expect(cleaned).not.toContain('MAITERM_AUTH');
      }
      expect(cleanSshCommand('ssh -o SetEnv=MAITERM_PROFILE=dev ews@nova')).toBe('-o SetEnv=MAITERM_PROFILE=dev ews@nova');
      expect(cleanSshCommand("ssh -o 'SendEnv=MAITERM_*' ews@nova 'MAITERM_AUTH=x tmux'")).toBe("-o 'SendEnv=MAITERM_*' ews@nova");
    });
    // OpenSSH reads options after the destination too: those are the user's, not a remote command.
    // And a repeated option that takes an argument is meant, every time (`-o X -o Y`).
    for (const typed of [
      'ssh ews@nova -o SetEnv=MAITERM_PROFILE=dev',
      "ssh ews@nova -o 'SendEnv=MAITERM_*'",
      'ssh nova -oSendEnv=MAITERM_X -p 2222',
      'ssh -o A=1 ews@nova -o SetEnv=MAITERM_PROFILE=dev',
      'ssh -L 8080:a:80 -L 9090:b:90 nova',
      'ssh -i k1 -i k2 nova',
      'ssh -o ServerAliveInterval=30 -o ServerAliveCountMax=3 nova',
      'ssh -p 2222 nova -p 2223',
      'ssh -v -v nova',
      'ssh -M -M -S ~/.ssh/cm nova',
      'ssh -t nova',
      'ssh -t -t nova',
      'ssh -tt nova',
      'ssh nova tmux attach -t main',
    ]) {
      it(`stores options after the host as typed: ${typed}`, () => {
        const stored = cleanSshCommand(typed);
        expect(stored).toBe(typed.slice('ssh '.length));
        // The shell the line is typed into expands a `~` before ssh sees it.
        const viaShell = stored.replace(/(^| )~\//g, `$1${process.env.HOME}/`);
        const argv = argvOf(buildSshCommand(stored, '/srv', TAB, bridge));
        const cleaned = cleanSshCommand(joinWords(argv));
        expect(cleaned).toBe(viaShell);
        expect(cleaned).not.toContain('MAITERM_AUTH');
        expect(cleanSshCommand(joinWords(wrapped(buildSshCommand(stored, '/srv', TAB, bridge))))).toBe(viaShell);
        // And with no remote command, where buildSshCommand adds no `-t` of its own.
        expect(cleanSshCommand(joinWords(argvOf(buildSshCommand(stored, null))))).toBe(viaShell);
      });
    }
    it('folds no switch: each one the user typed is kept', () => {
      expect(cleanSshCommand('ssh -x -C -x -C ews@nova')).toBe('-x -C -x -C ews@nova');
      expect(cleanSshCommand('ssh -o -x -x nova')).toBe('-o -x -x nova');
      expect(cleanSshCommand('ssh -x nova ls -x -x')).toBe('-x nova ls -x -x');
    });
    it('never keeps a word naming maiTerm env, whatever else it looks like', () => {
      expect(cleanSshCommand("ssh ews@nova 'MAITERM_AUTH=tok-123 tmux'")).toBe('ews@nova');
      expect(cleanSshCommand('ssh ews@nova export MAITERM_PORT=1 MAITERM_AUTH=x; tmux')).toBe('ews@nova');
    });
  });

  // A key persisted before quoting is the same host as its quoted spelling: compared by words.
  it('finds a host stored before quoting', () => {
    for (const old of ['-i C:\\Users\\me\\.ssh\\id ews@nova', '-J ews@[fe80::1]:22 nova', '-L [::1]:5432:db:5432 nova', '-o SendEnv=LC_* nova', '-o HostKeyAlgorithms=^ssh-ed25519 nova']) {
      expect(sameSshHost(old, connectArgs(old)), old).toBe(true);
      expect(sameSshHost(old, 'ews@nova')).toBe(false);
    }
    expect(connectArgs('-i C:\\Users\\me\\.ssh\\id ews@nova')).toBe("-i 'C:\\Users\\me\\.ssh\\id' ews@nova");
  });

  // A Windows key path is quoted, so a POSIX shell (Git Bash, WSL) gives ssh the path itself.
  it('gives a Windows path to ssh intact through a POSIX shell', () => {
    const stored = joinWords(['-i', 'C:\\Users\\me\\.ssh\\id', 'ews@nova']);
    expect(stored).toBe("-i 'C:\\Users\\me\\.ssh\\id' ews@nova");
    expect(argvOf(buildSshCommand(stored, '/srv', TAB)).slice(4, 7)).toEqual(['-i', 'C:\\Users\\me\\.ssh\\id', 'ews@nova']);
  });

  it('finds where the remote command starts as Rust does', () => {
    for (const [args, at] of REMOTE_AT) expect(remoteCommandIndex(splitWords(args)), args).toBe(at);
  });

  it('still cleans a lossy ps line, an apostrophe included', () => {
    const ps = `ssh -t -o ControlMaster=no -i /Users/me/Bob's/id ews@nova export MAITERM_TAB_ID=${TAB} MAITERM_AUTH=tok; cd /srv && exec $SHELL -l`;
    expect(cleanSshCommand(ps)).toBe("-i /Users/me/Bob's/id ews@nova");
  });
});
