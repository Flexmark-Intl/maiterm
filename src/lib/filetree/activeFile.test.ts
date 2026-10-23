import { describe, expect, it } from 'vitest';
import type { DiffContext, EditorFileInfo } from '$lib/tauri/types';
import { activeFileOf, settleActiveFile } from './activeFile';

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
    expect(activeFileOf({ tab_type: 'editor', editor_file: editor('/r/a.ts'), diff_context: null })).toEqual({ path: '/r/a.ts', offerShow: true });
  });

  it('a diff tab names its file but never offers to pin its folder', () => {
    expect(activeFileOf({ tab_type: 'diff', editor_file: null, diff_context: diff('/r/b.ts') })).toEqual({ path: '/r/b.ts', offerShow: false });
  });

  it('terminal, board, remote editor and missing tabs have no active file', () => {
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
