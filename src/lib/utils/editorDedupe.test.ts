import { describe, expect, it } from 'vitest';
import type { EditorFileInfo, Workspace } from '$lib/tauri/types';
import { findOpenEditor, revealsWorkspace, sameEditorFile } from './editorDedupe';

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

describe('revealsWorkspace', () => {
  const inW2 = { workspaceId: 'w2', paneId: 'p2', tabId: 'e2' };

  it('switches workspace when the on-screen workspace asked for a file open in another', () => {
    // A Cmd+click in w1 on a file w2 already shows: making it active inside hidden w2 is a dead click.
    expect(revealsWorkspace(inW2, 'w1', 'w1')).toBe(true);
  });

  it('stays put when the hit is already on screen', () => {
    expect(revealsWorkspace(inW2, 'w2', 'w2')).toBe(false);
    expect(revealsWorkspace(inW2, 'w1', 'w2')).toBe(false);
  });

  it('never pulls the window away for a request from a background workspace', () => {
    expect(revealsWorkspace(inW2, 'w3', 'w1')).toBe(false);
  });
});

describe('sameEditorFile path forms', () => {
  it('matches a click joined onto a cwd with ./ or .. to the plain path', () => {
    expect(sameEditorFile(local('/Users/d/repo/./src/a.ts'), local('/Users/d/repo/src/a.ts'))).toBe(true);
    expect(sameEditorFile(local('/Users/d/repo/lib/../src/a.ts'), local('/Users/d/repo/src/a.ts'))).toBe(true);
  });

  it('ignores separators and case for a local Windows file only', () => {
    expect(sameEditorFile(local('C:\\repo/src/a.ts'), local('c:\\Repo\\src\\a.ts'), true)).toBe(true);
    expect(sameEditorFile(local('/A/b.ts'), local('/a/b.ts'))).toBe(false);
    // A remote path is POSIX whatever this machine runs.
    expect(sameEditorFile(remote('/A/b.ts', 'h'), remote('/a/b.ts', 'h'), true)).toBe(false);
  });
});

describe('sameEditorFile identities', () => {
  it('matches two spellings Rust says are one file, whatever each tab stored', () => {
    // A tab stored /tmp/x/a.ts (the agent's spelling, Cmd+O, an older build); the request is
    // the canonical /private/tmp/x/a.ts. Lexically different, one file on disk.
    const ids = new Map([['/tmp/x/a.ts', 'inode:1:2'], ['/private/tmp/x/a.ts', 'inode:1:2'], ['/repo/README.md', 'inode:1:3'], ['/repo/Readme.md', 'inode:1:3']]);
    expect(sameEditorFile(local('/tmp/x/a.ts'), local('/private/tmp/x/a.ts'), false, ids)).toBe(true);
    expect(sameEditorFile(local('/repo/README.md'), local('/repo/Readme.md'), false, ids)).toBe(true);
    const workspaces = [ws('w1', [{ id: 'p1', tabs: [editor('e1', local('/tmp/x/a.ts'))] }])];
    expect(findOpenEditor(workspaces, local('/private/tmp/x/a.ts'), {}, false, ids)?.tabId).toBe('e1');
  });

  it('trusts different identities over a lexical match', () => {
    // /home/me/proj links elsewhere: proj/../shared is not /home/me/shared.
    const ids = new Map([['/home/me/proj/../shared/x.ts', 'inode:1:4'], ['/home/me/shared/x.ts', 'inode:1:5']]);
    expect(sameEditorFile(local('/home/me/proj/../shared/x.ts'), local('/home/me/shared/x.ts'), false, ids)).toBe(false);
  });

  it('never folds .. in a remote path, which nothing here can resolve', () => {
    expect(sameEditorFile(remote('/home/me/proj/../shared/x.ts', 'h'), remote('/home/me/shared/x.ts', 'h'))).toBe(false);
    expect(sameEditorFile(remote('/home/me/./shared//x.ts', 'h'), remote('/home/me/shared/x.ts', 'h'))).toBe(true);
  });
});
