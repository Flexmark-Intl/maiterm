import { beforeEach, describe, expect, it, vi } from 'vitest';

const readFile = vi.fn();
const ftStat = vi.fn();
const ftDiffBase = vi.fn();
const createDiffTab = vi.fn();

vi.mock('$lib/tauri/commands', () => ({ readFile, ftStat, ftDiffBase }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: {
    activeWorkspace: { id: 'w' },
    activePane: { id: 'p', active_tab_id: 't' },
    createDiffTab,
  },
}));

const { openGitDiff } = await import('./openGitDiff');

describe('Compare with HEAD', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    ftDiffBase.mockResolvedValue('old\n');
  });

  it('a file gone from disk diffs against an empty right side', async () => {
    readFile.mockRejectedValue('Cannot access file: No such file or directory (os error 2)');
    ftStat.mockRejectedValue('No such file or directory');
    await openGitDiff({ subId: 1, abs: '/r/a.ts' }, 'a.ts');
    expect(createDiffTab).toHaveBeenCalledOnce();
    expect(createDiffTab.mock.calls[0][3]).toMatchObject({ old_content: 'old\n', new_content: '' });
  });

  it('a file still on disk that readFile refuses is an error, never a deletion', async () => {
    ftStat.mockResolvedValue({ size: 3_000_000 });
    readFile.mockRejectedValue('FILE_TOO_LARGE:2.9');
    await expect(openGitDiff({ subId: 1, abs: '/r/big.lock' }, 'big.lock')).rejects.toThrow('too large to compare (2.9 MB)');
    readFile.mockRejectedValue('Cannot read file: stream did not contain valid UTF-8');
    await expect(openGitDiff({ subId: 1, abs: '/r/latin1.txt' }, 'latin1.txt')).rejects.toThrow('valid UTF-8');
    expect(createDiffTab).not.toHaveBeenCalled();
  });
});
