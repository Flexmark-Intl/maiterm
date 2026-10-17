/** An ssh target an agent may name: `[ssh] [-p port] [-l user] [-i key] [-J jump] [user@]host`,
 *  and nothing after the host. Anything else is refused rather than cleaned: a trailing word would
 *  be a remote command, and `-o` can run a LOCAL one (ProxyCommand), so the card's "runs on that
 *  host" would not be true. Returns the stored form (no program, as `cleanSshCommand` keeps it). */
export function parseSshTarget(input: string): string | null {
  const t = input.trim().split(/\s+/);
  if (t[0] === 'ssh') t.shift();
  const out: string[] = [];
  const host = /^(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9._-]+$/;
  const flags: Record<string, RegExp> = {
    '-p': /^\d{1,5}$/,
    '-l': /^[A-Za-z0-9._-]+$/,
    '-i': /^[A-Za-z0-9._~/-]+$/,
    '-J': /^(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9._-]+(?::\d{1,5})?$/,
  };
  while (t.length > 1) {
    const f = t.shift()!;
    const v = t.shift()!;
    if (!flags[f]?.test(v)) return null;
    out.push(f, v);
  }
  if (t.length !== 1 || !host.test(t[0]) || t[0].startsWith('-')) return null;
  out.push(t[0]);
  return out.join(' ');
}
