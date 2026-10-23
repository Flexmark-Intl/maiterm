import { describe, expect, it } from 'vitest';
import { connectArgs, ledgerPathIs, onAnotherHost, remoteWithin, sameSshHost, shortHostname } from './remote';

describe('connectArgs', () => {
  it('keeps where to connect and drops the rest', () => {
    expect(connectArgs('ews@nova')).toBe('ews@nova');
    expect(connectArgs('-p 2222 ews@nova')).toBe('-p 2222 ews@nova');
    expect(connectArgs('-xCp2222 ews@nova')).toBe('-xCp2222 ews@nova');
    expect(connectArgs('-t -o ControlMaster=no ews@nova')).toBe('-o ControlMaster=no ews@nova');
  });

  it('reads a live foreground command, remote command and all, without its secrets', () => {
    const ps = "ssh -t ews@nova export MAITERM_TAB_ID=t MAITERM_AUTH=secret; cd /srv && exec $SHELL -l";
    expect(connectArgs(ps)).toBe('ews@nova');
    expect(connectArgs('/usr/bin/ssh -p 2222 ews@nova')).toBe('-p 2222 ews@nova');
  });

  it('names nothing without a destination', () => {
    expect(connectArgs('')).toBe('');
    expect(connectArgs(null)).toBe('');
    expect(connectArgs('-p 2222')).toBe('');
  });
});

describe('sameSshHost', () => {
  it('matches the place, a stored command and a live one for one host', () => {
    expect(sameSshHost('ews@nova', 'ssh -t ews@nova')).toBe(true);
    expect(sameSshHost('-p 2222 ews@nova', 'ssh -t -p 2222 ews@nova')).toBe(true);
  });

  it('tells hosts, ports and users apart', () => {
    expect(sameSshHost('ews@nova', 'ews@vega')).toBe(false);
    expect(sameSshHost('-p 2222 ews@nova', 'ews@nova')).toBe(false);
    expect(sameSshHost('root@nova', 'ews@nova')).toBe(false);
    expect(sameSshHost('', '')).toBe(false);
  });
});

describe('onAnotherHost', () => {
  it('warns when the shell reports a third machine', () => {
    expect(onAnotherHost('vega', 'mac', 'nova')).toBe(true);
    expect(onAnotherHost('vega.lan', 'mac', 'nova.lan')).toBe(true);
  });

  it('stays quiet for the host itself, this computer, or no report', () => {
    expect(onAnotherHost('NOVA.lan.', 'mac', 'nova')).toBe(false);
    expect(onAnotherHost('mac.local', 'mac', 'nova')).toBe(false);
    expect(onAnotherHost(null, 'mac', 'nova')).toBe(false);
    expect(onAnotherHost('localhost', 'mac', 'nova')).toBe(false);
    expect(onAnotherHost('vega', 'mac', null)).toBe(false);
  });

  it('short names', () => {
    expect(shortHostname(' Nova.Example.COM. ')).toBe('nova');
  });
});

describe('ledgerPathIs', () => {
  it('matches an ssh tab’s host-keyed path on the root’s host only', () => {
    expect(ledgerPathIs('ews@nova:/srv/app/a.ts', '/srv/app/a.ts', 'ews@nova')).toBe(true);
    expect(ledgerPathIs('-p 2222 ews@nova:/srv/app/a.ts', '/srv/app/a.ts', '-p 2222 ews@nova')).toBe(true);
    expect(ledgerPathIs('ews@vega:/srv/app/a.ts', '/srv/app/a.ts', 'ews@nova')).toBe(false);
    expect(ledgerPathIs('/srv/app/a.ts', '/srv/app/a.ts', 'ews@nova')).toBe(false);
  });

  it('compares local paths NFC', () => {
    expect(ledgerPathIs('/x/é.ts', '/x/é.ts', null)).toBe(true);
  });
});

describe('remoteWithin', () => {
  it('is component-wise and case-sensitive', () => {
    expect(remoteWithin('/srv/app/src', '/srv/app')).toBe(true);
    expect(remoteWithin('/srv/app', '/srv/app/')).toBe(true);
    expect(remoteWithin('/srv/apples', '/srv/app')).toBe(false);
    expect(remoteWithin('/srv/App', '/srv/app')).toBe(false);
  });
});
