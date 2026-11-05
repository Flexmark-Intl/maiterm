import { describe, expect, it, vi } from 'vitest';

const shellOpen = vi.fn(async (_u: string) => {});
const mcpServerSignIn = vi.fn(async (_id: string) => 'https://auth.example.com/authorize?x=1');
vi.mock('@tauri-apps/plugin-shell', () => ({ open: shellOpen }));
vi.mock('$lib/tauri/commands', () => ({ mcpServerSignIn }));

const { entryName, freeServerName, sameServerUrl, startSignIn, suggestServerName } = await import('./addServer');

describe('suggestServerName', () => {
  it('takes the name from the host, as the Preferences form does', () => {
    expect(suggestServerName('https://mcp.linear.app/mcp')).toBe('linear');
    expect(suggestServerName('https://api.elevenlabs.io/v1/mcp')).toBe('elevenlabs');
    expect(suggestServerName('http://127.0.0.1:9000/mcp')).toBe('local');
    expect(suggestServerName('not a url')).toBe('');
  });
});

describe('sameServerUrl', () => {
  it('compares scheme and host without case, the path with case, ignoring a trailing slash', () => {
    expect(sameServerUrl('https://MCP.fal.ai/mcp-relay/', 'https://mcp.fal.ai/mcp-relay')).toBe(true);
    expect(sameServerUrl('https://mcp.fal.ai/MCP-relay', 'https://mcp.fal.ai/mcp-relay')).toBe(false);
    expect(sameServerUrl('https://mcp.fal.ai:8443/mcp', 'https://mcp.fal.ai/mcp')).toBe(false);
    expect(sameServerUrl('https://a.example/mcp?x=1', 'https://a.example/mcp')).toBe(false);
  });
});

describe('freeServerName', () => {
  it('keeps the name when nothing holds it', () => {
    expect(freeServerName('fal', [], [], false)).toBe('fal');
  });
  it('suffixes a name a native entry holds, as agents would see it', () => {
    expect(freeServerName('fal', [], ['fal'], false)).toBe('fal-2');
    expect(freeServerName('fal', [], ['fal'], true)).toBe('fal');
    expect(freeServerName('fal', [], ['fal-dev'], true)).toBe('fal-2');
  });
  it('suffixes past another maiTerm server and a native entry together', () => {
    expect(freeServerName('fal', ['fal', 'fal-2'], ['fal-3'], false)).toBe('fal-4');
  });
  it('stays within 40 characters and refuses an invalid base', () => {
    const long = 'a'.repeat(40);
    expect(freeServerName(long, [long], [], false)).toBe(`${'a'.repeat(38)}-2`);
    expect(freeServerName('Fal', [], [], false)).toBeNull();
  });
  it('names the dev entry', () => {
    expect(entryName('fal', true)).toBe('fal-dev');
  });
});

describe('startSignIn', () => {
  it('opens an https sign-in page', async () => {
    await startSignIn('fal');
    expect(shellOpen).toHaveBeenCalledWith('https://auth.example.com/authorize?x=1');
  });
  it('never opens another scheme', async () => {
    shellOpen.mockClear();
    mcpServerSignIn.mockResolvedValueOnce('file:///etc/passwd');
    await expect(startSignIn('fal')).rejects.toThrow('not a web page');
    expect(shellOpen).not.toHaveBeenCalled();
  });
});
