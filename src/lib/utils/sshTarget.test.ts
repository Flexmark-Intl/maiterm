import { describe, expect, it } from 'vitest';
import { parseSshTarget } from './sshTarget';

describe('parseSshTarget', () => {
  it('takes a host, user@host or alias, with or without the program', () => {
    expect(parseSshTarget('nova')).toBe('nova');
    expect(parseSshTarget('ews@nova')).toBe('ews@nova');
    expect(parseSshTarget('  ssh ews@nova.example.com ')).toBe('ews@nova.example.com');
  });

  it('keeps the allowed flags', () => {
    expect(parseSshTarget('ssh -p 2222 -i ~/.ssh/id_ed25519 -J jump@bastion:22 ews@nova')).toBe('-p 2222 -i ~/.ssh/id_ed25519 -J jump@bastion:22 ews@nova');
    expect(parseSshTarget('-l ews nova')).toBe('-l ews nova');
  });

  it('refuses a remote command, -o, other flags and shell syntax', () => {
    expect(parseSshTarget('ews@nova rm -rf ~')).toBeNull();
    expect(parseSshTarget('ews@nova uptime')).toBeNull();
    expect(parseSshTarget('-o ProxyCommand=sh ews@nova')).toBeNull();
    expect(parseSshTarget('-A ews@nova')).toBeNull();
    expect(parseSshTarget('ews@nova; id')).toBeNull();
    expect(parseSshTarget("ews@nova'")).toBeNull();
    expect(parseSshTarget('-p 22')).toBeNull();
    expect(parseSshTarget('ssh')).toBeNull();
    expect(parseSshTarget('/usr/bin/ssh nova')).toBeNull();
  });
});
