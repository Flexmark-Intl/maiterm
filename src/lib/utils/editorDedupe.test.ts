import { describe, expect, it } from 'vitest';
import type { EditorFileInfo, Workspace } from '$lib/tauri/types';
import { findOpenEditor, sameEditorFile } from './editorDedupe';

const local = (path: string): EditorFileInfo => ({
  file_path: path, is_remote: false, remote_ssh_command: null, remote_path: null, language: null,
});
const remote = (path: string, ssh: string): EditorFileInfo => ({
  file_path: path, is_remote: true, remote_ssh_command: ssh, remote_path: path, language: null,
});

// Only the fields the dedupe reads; the cast keeps the fixture readable.
const editor = (id: string, file: EditorFileInfo) => ({ id, tab_type: 'editor', editor_file: file });
const terminal = (id: string) => ({ id, tab_type: 'terminal', editor_file: null });
const ws = (id: string, panes: { id: string; tabs: unknown[] }[]) => ({ id, panes }) as unknown as Workspace;

describe('findOpenEditor', () => {
  const workspaces = [
    ws('w1', [{ id: 'p1', tabs: [terminal('t1'), editor('e1', local('/src/a.ts'))] }]),
    ws('w2', [
      { id: 'p2', tabs: [editor('e2', local('/src/a.ts')), editor('e3', remote('/src/a.ts', 'ews@nova'))] },
      { id: 'p3', tabs: [editor('e4', local('/src/a.ts'))] },
    ]),
  ];

  it('returns the tab already showing the file', () => {
    expect(findOpenEditor(workspaces, local('/src/a.ts'))).toEqual({ workspaceId: 'w1', paneId: 'p1', tabId: 'e1' });
  });

  it('prefers the named pane, then the named workspace', () => {
    expect(findOpenEditor(workspaces, local('/src/a.ts'), { workspaceId: 'w2', paneId: 'p3' })?.tabId).toBe('e4');
    expect(findOpenEditor(workspaces, local('/src/a.ts'), { workspaceId: 'w2', paneId: 'nope' })?.tabId).toBe('e2');
  });

  it('finds nothing for a file no editor shows', () => {
    expect(findOpenEditor(workspaces, local('/src/b.ts'))).toBeNull();
  });

  it('never matches a local file to the same path on an ssh host, or one host to another', () => {
    expect(findOpenEditor(workspaces, remote('/src/a.ts', 'ews@nova'))?.tabId).toBe('e3');
    expect(findOpenEditor(workspaces, remote('/src/a.ts', 'me@other'))).toBeNull();
    expect(sameEditorFile(local('/x'), remote('/x', 'ews@nova'))).toBe(false);
  });
});
