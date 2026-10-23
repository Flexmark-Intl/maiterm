import { describe, expect, it } from 'vitest';
import { editorTitleAfter, editorTitleFor } from './editorTitle';

const local = (file_path: string) => ({ file_path, is_remote: false });
const remote = (file_path: string) => ({ file_path, is_remote: true });

describe('editorTitleFor', () => {
  it('splits a remote path on / only, whatever this computer is', () => {
    expect(editorTitleFor('/srv/app/main.rs', true, true)).toBe('main.rs');
    expect(editorTitleFor('C:\\p\\a.ts', false, true)).toBe('a.ts');
    expect(editorTitleFor('/p/report\\v2.txt', false, false)).toBe('report\\v2.txt');
  });
});

describe('editorTitleAfter', () => {
  it('follows the file while the title is still its name', () => {
    expect(editorTitleAfter('a.ts', local('/p/src/a.ts'), local('/p/lib/b.ts'), false)).toBe('b.ts');
  });

  it('keeps a title the human chose', () => {
    expect(editorTitleAfter('Spec', local('/p/a.ts'), local('/p/b.ts'), false)).toBe('Spec');
  });

  it('takes the new name when there is no old file or title', () => {
    expect(editorTitleAfter('', local('/p/a.ts'), local('/p/b.ts'), false)).toBe('b.ts');
    expect(editorTitleAfter('Spec', null, local('/p/b.ts'), false)).toBe('b.ts');
  });

  it('counts the whole-path title the old agent open gave a Windows tab as automatic', () => {
    expect(editorTitleAfter('C:\\p\\a.ts', local('C:\\p\\a.ts'), local('C:\\p\\b.ts'), true)).toBe('b.ts');
  });

  it('compares a remote title by the remote name', () => {
    expect(editorTitleAfter('x.py', remote('/srv/x.py'), remote('/srv/y.py'), false)).toBe('y.py');
  });
});
