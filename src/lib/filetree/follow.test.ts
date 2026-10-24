import { describe, expect, it } from 'vitest';
import { followStep } from './follow';

const local = (place: string, extra: { project?: boolean; source?: string } = {}) => ({
  place,
  kind: 'local',
  host_key: null,
  source: extra.source ?? 'pty',
  project: extra.project ?? false,
});
const remote = (host: string, root: string, project = false) => ({ place: `${host}:${root}`, kind: 'remote', host_key: host, source: 'osc7', project });

describe('followStep', () => {
  it('re-roots from a home root to a project inside it', () => {
    // An ssh tab exited to a local shell at ~; the next tab is in ~/proj, a repository. The
    // folder is inside home, but its root is the project: only moving the tick left the tree
    // stuck on home for good, forced Follow included.
    expect(followStep(local('/Users/me', { project: false }), local('/Users/me/proj', { project: true }))).toBe('reroot');
    expect(followStep(local('/', {}), local('/srv/app', { project: true }))).toBe('reroot');
    expect(followStep(local('/Volumes/Data', {}), local('/Volumes/Data/work/app', { project: true }))).toBe('reroot');
  });

  it('re-roots to another repository inside a non-shallow root too', () => {
    expect(followStep(local('/Users/me/checkouts'), local('/Users/me/checkouts/app', { project: true }))).toBe('reroot');
  });

  it('keeps a cd inside a project cheap', () => {
    // The same top level resolves to the same place: nothing is opened again.
    expect(followStep(local('/Users/me/proj', { project: true }), local('/Users/me/proj', { project: true }))).toBe('same');
  });

  it('only looks a plain folder up in the root', () => {
    // A folder in no repository roots at itself; inside the root it moves the tick.
    expect(followStep(local('/Users/me'), local('/Users/me/Downloads'))).toBe('locate');
  });

  it('re-roots across machines and pins', () => {
    expect(followStep(null, local('/a'))).toBe('reroot');
    expect(followStep(local('/tmp'), remote('ews@nova', '/tmp/x'))).toBe('reroot');
    expect(followStep(remote('ews@nova', '/home/ews'), remote('ews@nova', '/home/ews/x'))).toBe('locate');
    expect(followStep(remote('ews@nova', '/home/ews'), remote('ews@nova', '/home/ews/app', true))).toBe('reroot');
    expect(followStep(remote('ews@nova', '/home/ews'), remote('ews@mars', '/home/ews/x'))).toBe('reroot');
    expect(followStep(local('/a', { source: 'pin' }), local('/a/b'))).toBe('reroot');
    expect(followStep(local('/a'), local('/a/b', { source: 'pin' }))).toBe('reroot');
  });

  it('roots at a Hive folder even when it is no repository and lies inside the root', () => {
    // A coordinator on a plain primary, or a worker with only its scratch dir, from a home root.
    expect(followStep(local('/Users/me'), local('/Users/me/footage', { source: 'hive' }))).toBe('reroot');
    expect(followStep(local('/Users/me/footage', { source: 'hive' }), local('/Users/me/footage', { source: 'hive' }))).toBe('same');
  });
});
