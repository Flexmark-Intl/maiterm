import { describe, it, expect } from 'vitest';
import {
  buildForkCommand,
  getResumeCommand,
  isForkCommand,
  supportsFork,
  toForkCommand,
} from './resume';

// The runtimes do not agree on the SHAPE of a fork. Claude appends `--fork-session` to a
// resume; Codex has a distinct `codex fork SESSION_ID` subcommand. Modelling only the flag is
// why Codex forking was inert everywhere the flag was the test
// (docs/codex-integration-review.md C6).

describe('fork support by runtime', () => {
  it('knows which runtimes can fork', () => {
    expect(supportsFork('claude')).toBe(true);
    expect(supportsFork('codex')).toBe(true);
    expect(supportsFork('gemini')).toBe(false);
  });

  it('builds each runtime spawn command in its own shape', () => {
    expect(buildForkCommand('claude', 'sid-1')).toBe('claude --resume sid-1 --fork-session');
    expect(buildForkCommand('codex', 'sid-1')).toBe('codex fork sid-1');
    expect(buildForkCommand('gemini', 'sid-1')).toBeNull();
  });
});

describe('isForkCommand', () => {
  // A fork command must never be reused as a resume command: it would re-fork the ORIGINAL
  // session on every restore, losing the tab's own conversation and pinning the wrong id.
  it('recognises a fork for each runtime', () => {
    expect(isForkCommand('claude', 'claude --resume sid --fork-session')).toBe(true);
    expect(isForkCommand('codex', 'codex fork sid')).toBe(true);
  });

  it('does not mistake a resume for a fork', () => {
    expect(isForkCommand('claude', 'claude --resume sid')).toBe(false);
    expect(isForkCommand('codex', 'codex resume sid')).toBe(false);
    expect(isForkCommand('codex', getResumeCommand('codex'))).toBe(false);
    expect(isForkCommand('claude', getResumeCommand('claude'))).toBe(false);
  });

  it('is not fooled by the word appearing elsewhere', () => {
    // A cwd or a flag value containing "fork" is not a fork subcommand.
    expect(isForkCommand('codex', 'cd /src/forklift && codex resume sid')).toBe(false);
    expect(isForkCommand('codex', 'codex resume forked-session-id')).toBe(false);
  });

  it('is false for a runtime with no fork, and for empty input', () => {
    expect(isForkCommand('gemini', 'gemini fork sid')).toBe(false);
    expect(isForkCommand('codex', null)).toBe(false);
    expect(isForkCommand('codex', undefined)).toBe(false);
  });
});

describe('toForkCommand (contested session ids)', () => {
  it('appends the flag for Claude and swaps the verb for Codex', () => {
    expect(toForkCommand('claude', 'claude --resume %claudeSessionId'))
      .toBe('claude --resume %claudeSessionId --fork-session');
    expect(toForkCommand('codex', 'codex resume %codexSessionId'))
      .toBe('codex fork %codexSessionId');
  });

  it('rewrites the resume verb even with a command prefix', () => {
    expect(toForkCommand('codex', 'cd /srv && codex resume %codexSessionId'))
      .toBe('cd /srv && codex fork %codexSessionId');
  });

  it('returns null rather than double-forking something already forked', () => {
    expect(toForkCommand('claude', 'claude --resume sid --fork-session')).toBeNull();
    expect(toForkCommand('codex', 'codex fork sid')).toBeNull();
  });

  it('returns null when the command is not that runtime resume, or there is no fork', () => {
    // Codex cannot fork a command whose verb it does not recognise — appending a flag the way
    // Claude does would produce a command that is not a thing.
    expect(toForkCommand('codex', 'codex exec "do something"')).toBeNull();
    expect(toForkCommand('gemini', 'gemini --resume %geminiSessionId')).toBeNull();
  });
});

describe('getResumeCommand', () => {
  it('leaves the plain templates alone by default', () => {
    expect(getResumeCommand('codex')).toBe('codex resume %codexSessionId');
    expect(getResumeCommand('claude')).toBe('claude --resume %claudeSessionId');
  });

  it('adds the Codex hook-trust bypass only when asked, and only for Codex', () => {
    expect(getResumeCommand('codex', { bypassHookTrust: true }))
      .toBe('codex resume --dangerously-bypass-hook-trust %codexSessionId');
    expect(getResumeCommand('claude', { bypassHookTrust: true }))
      .toBe('claude --resume %claudeSessionId');
  });

  it('keeps a bypassing resume recognisable as a resume, not a fork', () => {
    // Otherwise handleEnableAutoResume would discard it as a fork command.
    expect(isForkCommand('codex', getResumeCommand('codex', { bypassHookTrust: true }))).toBe(false);
  });
});
