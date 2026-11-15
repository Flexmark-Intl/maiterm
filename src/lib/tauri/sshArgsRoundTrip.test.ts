import { describe, expect, it } from 'vitest';
import { execFileSync } from 'node:child_process';
import { buildSshCommand, cleanSshCommand, sshArgsOf } from './commands';
import { connectArgs } from '$lib/filetree/remote';
import { hostLabel } from '$lib/followUps/model';
import { joinWords, splitWords } from '$lib/utils/shellWords';
import { HARD_WORDS } from '$lib/utils/shellWords.test';

const TAB = 'a1b2c3d4-5e6f-7788-99aa-bbccddeeff00';

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
    expect(cleanSshCommand('ssh -t -x -i "/Users/me/My Keys/id" -x ~/x ews@nova')).toBe('-x -i "/Users/me/My Keys/id" ~/x ews@nova');
    // A `-t` or ControlMaster inside a quoted word is part of that word.
    expect(cleanSshCommand("ssh -i '/a -t b' -o 'SetEnv=X=-o ControlMaster=no' h")).toBe("-i '/a -t b' -o 'SetEnv=X=-o ControlMaster=no' h");
    expect(sshArgsOf("autossh -M 0 -f -i '/a -M b' h")).toBe("-i '/a -M b' h");
  });

  it('still cleans a lossy ps line, an apostrophe included', () => {
    const ps = `ssh -t -o ControlMaster=no -i /Users/me/Bob's/id ews@nova export MAITERM_TAB_ID=${TAB} MAITERM_AUTH=tok; cd /srv && exec $SHELL -l`;
    expect(cleanSshCommand(ps)).toBe("-i /Users/me/Bob's/id ews@nova");
  });
});
