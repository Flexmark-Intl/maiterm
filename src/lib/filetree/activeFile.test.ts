import { describe, expect, it } from 'vitest';
import type { DiffContext, EditorFileInfo } from '$lib/tauri/types';
import { activeFileOf, lookupIn, settleActiveFile } from './activeFile';

const editor = (file_path: string, is_remote = false): EditorFileInfo => ({
  file_path,
  is_remote,
  remote_ssh_command: null,
  remote_path: null,
  language: null,
});
const diff = (file_path: string): DiffContext => ({ request_id: '', file_path, old_content: '', new_content: '', tab_name: 'Diff' });

describe('activeFileOf', () => {
  it('a local editor tab names its file and may offer "Show" outside the root', () => {
    expect(activeFileOf({ tab_type: 'editor', editor_file: editor('/r/a.ts'), diff_context: null })).toEqual({ path: '/r/a.ts', offerShow: true, host: null });
  });

  it('a remote editor tab names its path on the host, never offering to pin it here', () => {
    const f = { ...editor('/home/x/a.ts', true), remote_ssh_command: 'ews@nova', remote_path: '/home/x/a.ts' };
    expect(activeFileOf({ tab_type: 'editor', editor_file: f, diff_context: null })).toEqual({ path: '/home/x/a.ts', offerShow: false, host: 'ews@nova' });
  });

  it('a diff tab names its file but never offers to pin its folder', () => {
    expect(activeFileOf({ tab_type: 'diff', editor_file: null, diff_context: diff('/r/b.ts') })).toEqual({ path: '/r/b.ts', offerShow: false });
  });

  it('terminal, board, a remote editor with no host, and missing tabs have no active file', () => {
    expect(activeFileOf({ tab_type: 'terminal', editor_file: null, diff_context: null })).toBeNull();
    expect(activeFileOf({ tab_type: 'board', editor_file: null, diff_context: null })).toBeNull();
    expect(activeFileOf({ tab_type: 'editor', editor_file: editor('/home/x/a.ts', true), diff_context: null })).toBeNull();
    expect(activeFileOf({ tab_type: 'editor', editor_file: null, diff_context: null })).toBeNull();
    expect(activeFileOf(null)).toBeNull();
  });

  it('follows the file a tab shows, not the tab id: a preview swap is a new target', () => {
    const tab = { tab_type: 'editor' as const, editor_file: editor('/r/a.ts'), diff_context: null };
    const before = activeFileOf(tab);
    tab.editor_file = editor('/r/b.ts');
    expect(activeFileOf(tab)?.path).toBe('/r/b.ts');
    expect(before?.path).toBe('/r/a.ts');
  });
});

describe('lookupIn', () => {
  const same = (a: string, b: string) => a === b;
  it('looks a path up only in a root on its own machine', () => {
    expect(lookupIn({ path: '/tmp/a', offerShow: true, host: null }, null, same)).toBe(true);
    expect(lookupIn({ path: '/tmp/a', offerShow: true, host: null }, 'ews@nova', same)).toBe(false);
    expect(lookupIn({ path: '/tmp/a', offerShow: false, host: 'ews@nova' }, null, same)).toBe(false);
    expect(lookupIn({ path: '/tmp/a', offerShow: false, host: 'ews@nova' }, 'ews@nova', same)).toBe(true);
    expect(lookupIn({ path: '/tmp/a', offerShow: false, host: 'ews@vega' }, 'ews@nova', same)).toBe(false);
  });

  it('a diff tab’s path, whose machine nobody knows, is looked up wherever', () => {
    expect(lookupIn({ path: '/tmp/a', offerShow: false }, 'ews@nova', same)).toBe(true);
    expect(lookupIn({ path: '/tmp/a', offerShow: false }, null, same)).toBe(true);
  });
});

describe('settleActiveFile', () => {
  const under = (map: Record<string, string>) => async (abs: string) => map[abs] ?? null;

  it('no target, or no subscription, clears both the highlight and "Show"', async () => {
    expect(await settleActiveFile(null, under({}))).toEqual({ active: null, outside: null });
    expect(await settleActiveFile({ path: '/r/a.ts', offerShow: true }, null)).toEqual({ active: null, outside: null });
  });

  it('a file under the root is highlighted', async () => {
    expect(await settleActiveFile({ path: '/r/src/a.ts', offerShow: true }, under({ '/r/src/a.ts': 'src/a.ts' }))).toEqual({ active: 'src/a.ts', outside: null });
  });

  it('outside the root: an editor offers "Show", a diff stays quiet', async () => {
    expect(await settleActiveFile({ path: '/elsewhere/a.ts', offerShow: true }, under({}))).toEqual({ active: null, outside: '/elsewhere/a.ts' });
    expect(await settleActiveFile({ path: '/elsewhere/a.ts', offerShow: false }, under({}))).toEqual({ active: null, outside: null });
  });

  it('a failed lookup reads as outside the root, never as a stale highlight', async () => {
    const failing = async () => { throw new Error('sub closed'); };
    expect(await settleActiveFile({ path: '/r/a.ts', offerShow: false }, failing)).toEqual({ active: null, outside: null });
  });
});
