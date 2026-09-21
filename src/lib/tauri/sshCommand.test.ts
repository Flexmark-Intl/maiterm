import { describe, it, expect } from 'vitest';
import { buildSshCommand, cleanSshCommand } from './commands';

const TAB = 'a1b2c3d4-5e6f-7788-99aa-bbccddeeff00';
/** Verbatim what `accounts::remote::export_fragment` produces — keep the two in step. */
const ACCT =
  `__mt_oat=$(cat ~/.maiterm/tokens/tok-${TAB} 2>/dev/null); rm -f ~/.maiterm/tokens/tok-${TAB} 2>/dev/null; ` +
  `[ -n "$__mt_oat" ] && export CLAUDE_CODE_OAUTH_TOKEN="$__mt_oat"; unset __mt_oat`;

describe('buildSshCommand', () => {
  it('bakes the tab id into the remote command so shared hosts get a per-tab identity', () => {
    const cmd = buildSshCommand('-x -C ews@nova', '/srv/app', TAB);
    expect(cmd).toContain(`export MAITERM_TAB_ID=${TAB};`);
    expect(cmd).toContain("cd '/srv/app' && exec $SHELL -l");
    // -t is required for the remote command to get a tty.
    expect(cmd).toMatch(/^ssh -t -o ControlMaster=no /);
  });

  it('carries the tab id even with no remote cwd', () => {
    const cmd = buildSshCommand('ews@nova', null, TAB);
    expect(cmd).toContain(`export MAITERM_TAB_ID=${TAB}; exec $SHELL -l`);
  });

  it('is unchanged when no tab id is given', () => {
    expect(buildSshCommand('ews@nova', null)).toBe('ssh -o ControlMaster=no ews@nova');
    expect(buildSshCommand('ews@nova', '/srv/app')).toBe(
      "ssh -t -o ControlMaster=no ews@nova 'cd '/srv/app' && exec $SHELL -l'",
    );
  });

  it('refuses to interpolate anything that is not a plain id', () => {
    const cmd = buildSshCommand('ews@nova', null, "x'; rm -rf /; echo '");
    expect(cmd).not.toContain('rm -rf');
    expect(cmd).toBe('ssh -o ControlMaster=no ews@nova');
  });

  it('carries which maiTerm to talk to, so the shared remote config need not name it', () => {
    const cmd = buildSshCommand('ews@nova', '/srv/app', TAB, { port: 28123, auth: 'tok-123' });
    expect(cmd).toContain(`export MAITERM_TAB_ID=${TAB} MAITERM_PORT=28123 MAITERM_AUTH=tok-123;`);
  });

  it('drops a bridge it cannot safely paste into a remote shell', () => {
    const cmd = buildSshCommand('ews@nova', null, TAB, { port: 28123, auth: "t'; rm -rf /; echo '" });
    expect(cmd).not.toContain('rm -rf');
    expect(cmd).not.toContain('MAITERM_PORT');
    expect(cmd).toContain(`export MAITERM_TAB_ID=${TAB};`);
  });

  // §6. The fragment names a file Rust already pushed over its own connection; the token itself
  // must never be here, because this whole string is typed into the user's local shell.
  it('carries the account fragment as its own statement, ahead of the cd', () => {
    const cmd = buildSshCommand('ews@nova', '/srv/app', TAB, null, ACCT);
    expect(cmd).toContain(`export MAITERM_TAB_ID=${TAB}; ${ACCT}; cd '/srv/app'`);
    expect(cmd).not.toContain('sk-ant');
  });

  it('carries the account fragment with no bridge and no cwd', () => {
    expect(buildSshCommand('ews@nova', null, TAB, null, ACCT))
      .toBe(`ssh -t -o ControlMaster=no ews@nova 'export MAITERM_TAB_ID=${TAB}; ${ACCT}; exec $SHELL -l'`);
  });

  it('refuses an account fragment that would break out of the quoting', () => {
    const cmd = buildSshCommand('ews@nova', null, TAB, null, "x'; rm -rf /; echo '");
    expect(cmd).not.toContain('rm -rf');
    expect(cmd).toBe(`ssh -t -o ControlMaster=no ews@nova 'export MAITERM_TAB_ID=${TAB}; exec $SHELL -l'`);
  });
});

describe('cleanSshCommand round-trip', () => {
  // The failure this prevents: cleanSshCommand strips the remote command to recover the bare
  // host for storage. If it does not recognise the export form, the plain pattern matches from
  // ` cd …` onwards and leaves a dangling `'export MAITERM_TAB_ID=…;`, which is stored and then
  // baked into the NEXT build — accumulating on every clone/restore.
  it('strips the baked export, leaving the bare host', () => {
    for (const cwd of ['/srv/app', null]) {
      const built = buildSshCommand('-x -C ews@nova', cwd, TAB);
      expect(cleanSshCommand(built)).toBe('-x -C ews@nova');
    }
  });

  it('is idempotent across repeated build/clean cycles', () => {
    let stored = '-x -C ews@nova';
    for (let i = 0; i < 3; i++) {
      stored = cleanSshCommand(buildSshCommand(stored, '/srv/app', TAB));
    }
    expect(stored).toBe('-x -C ews@nova');
  });

  it('still strips the pre-export forms stored by earlier builds', () => {
    expect(cleanSshCommand("ssh -t -o ControlMaster=no ews@nova 'cd '/srv/app' && exec $SHELL -l'"))
      .toBe('ews@nova');
    // Unquoted, as it comes back from ps.
    expect(cleanSshCommand('ssh -t ews@nova cd /srv/app && exec $SHELL -l')).toBe('ews@nova');
  });

  it('strips the unquoted export form that ps reports', () => {
    expect(cleanSshCommand(`ssh -t ews@nova export MAITERM_TAB_ID=${TAB}; cd /srv/app && exec $SHELL -l`))
      .toBe('ews@nova');
  });

  // The export carries several variables now. A pattern that stops at the first space
  // recognises none of them, and the dangling remainder accumulates on every round-trip.
  it('strips the multi-variable export, quoted and unquoted', () => {
    const bridge = { port: 28123, auth: 'tok-123' };
    for (const cwd of ['/srv/app', null]) {
      expect(cleanSshCommand(buildSshCommand('-x -C ews@nova', cwd, TAB, bridge))).toBe('-x -C ews@nova');
    }
    expect(cleanSshCommand(
      `ssh -t ews@nova export MAITERM_TAB_ID=${TAB} MAITERM_PORT=28123 MAITERM_AUTH=tok-123; cd /srv/app && exec $SHELL -l`,
    )).toBe('ews@nova');
  });

  // §6 added a second statement between the export and the `cd`. A pattern that enumerates what
  // may appear there stops recognising the command, and the dangling remainder — which now names
  // a credential handoff path — accumulates into the stored host string on every round trip.
  it('strips the account fragment too, quoted and unquoted', () => {
    const bridge = { port: 28123, auth: 'tok-123' };
    for (const cwd of ['/srv/app', null]) {
      expect(cleanSshCommand(buildSshCommand('-x -C ews@nova', cwd, TAB, bridge, ACCT)))
        .toBe('-x -C ews@nova');
    }
    expect(cleanSshCommand(
      `ssh -t ews@nova export MAITERM_TAB_ID=${TAB} MAITERM_PORT=28123 MAITERM_AUTH=tok-123; ${ACCT}; cd /srv/app && exec $SHELL -l`,
    )).toBe('ews@nova');
  });

  it('stays idempotent with the account fragment in play', () => {
    let stored = '-x -C ews@nova';
    for (let i = 0; i < 3; i++) {
      stored = cleanSshCommand(buildSshCommand(stored, '/srv/app', TAB, { port: 1, auth: 'a' }, ACCT));
    }
    expect(stored).toBe('-x -C ews@nova');
  });
});
