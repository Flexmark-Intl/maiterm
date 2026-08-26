import { describe, it, expect } from 'vitest';
import { mergeAutoResumeContext } from './autoResumeContext';

const SAVED = { cwd: '/Users/dprusak', remoteCwd: '/home/ews/public_html/flexmark/phpapi' };

describe('mergeAutoResumeContext', () => {
  // The incident: agent-init-session now fires when the agent STARTS, so this runs before the
  // remote shell has reported OSC 7. remoteCwd is null, and the old code wrote that null over
  // a real path. The damage then compounds — with no remote cwd the next ssh replay has no
  // `cd`, the session lands in the remote HOME, and home gets recorded as the tab's cwd.
  it('never erases a saved path with an unknown one', () => {
    expect(mergeAutoResumeContext({ cwd: null, remoteCwd: null }, SAVED)).toEqual(SAVED);
  });

  it('keeps the saved remote path when only the local cwd is visible', () => {
    // The exact shape of the failure: an SSH tab whose local shell sits in ~ while the remote
    // end has not said where it is yet.
    expect(
      mergeAutoResumeContext({ cwd: '/Users/dprusak', remoteCwd: null }, SAVED),
    ).toEqual(SAVED);
  });

  it('takes an observation when there is one — this must still track a real cd', () => {
    expect(
      mergeAutoResumeContext({ cwd: '/tmp', remoteCwd: '/home/ews/other' }, SAVED),
    ).toEqual({ cwd: '/tmp', remoteCwd: '/home/ews/other' });
  });

  it('accepts the remote home when it is genuinely observed', () => {
    // Not special-cased: if the shell really is in ~, that is where the tab is. The bug was
    // writing null, not writing home.
    expect(
      mergeAutoResumeContext({ cwd: null, remoteCwd: '/home/ews' }, SAVED),
    ).toEqual({ cwd: SAVED.cwd, remoteCwd: '/home/ews' });
  });

  it('fills in from nothing when there is nothing saved', () => {
    expect(
      mergeAutoResumeContext({ cwd: '/tmp', remoteCwd: null }, { cwd: null, remoteCwd: null }),
    ).toEqual({ cwd: '/tmp', remoteCwd: null });
  });
});
