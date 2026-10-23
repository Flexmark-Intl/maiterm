/**
 * A remote root's host, as the frontend has to compare it (docs/file-tree.md §15).
 *
 * Three spellings of "which ssh host" meet here: the place's `host_key` (Rust's
 * `watch::connect_args` of the tunnel, `-p 2222 ews@nova`), a tab's stored ssh command
 * (`cleanSshCommand`'s bare args, maybe with flags such as `-x -C`), and the live foreground
 * command off the process table (`ssh -t ews@nova 'export …; cd … && exec $SHELL -l'`). They
 * are the same host when the words that say WHERE to connect are the same, which is what
 * `connectArgs` keeps, read as ssh's getopt reads them (a port of `watch::parse_ssh`).
 */
import { sshArgsOf } from '$lib/tauri/commands';

/** ssh's options that take an argument (OpenSSH's getopt string), as `watch.rs` has them. */
const SSH_ARG_FLAGS = 'bceilmopBDEFIJLOPQRSwW';

/** The words that say where to connect: options and destination, without `-t` and without a
 *  remote command after the destination. Empty for something that names no host. */
export function connectArgs(cmd: string | null | undefined): string {
  if (!cmd) return '';
  const words = sshArgsOf(cmd).split(/\s+/).filter(Boolean);
  const out: string[] = [];
  let dest = false;
  for (let i = 0; i < words.length; i++) {
    const w = words[i];
    if (w === '--') {
      if (!dest && words[i + 1]) out.push(words[i + 1]);
      break;
    }
    if (!w.startsWith('-') || w === '-') {
      // The first plain word is the destination; one after it starts the remote command.
      if (dest) break;
      dest = true;
      out.push(w);
      continue;
    }
    const flags = w.slice(1);
    // A terminal is never part of where: `-t` / `-tt` come and go with how a tab was started.
    if (/^t+$/.test(flags)) continue;
    // Nor is ControlMaster: `buildSshCommand` injects `-o ControlMaster=no` into every ssh it
    // starts, and `cleanSshCommand` drops it from what is stored.
    if (/^ocontrolmaster=/i.test(flags)) continue;
    if (flags === 'o' && /^controlmaster=/i.test(words[i + 1] ?? '')) {
      i++;
      continue;
    }
    out.push(w);
    // A cluster (`-xCp2222`): its argument is the rest of the word, or else the next word.
    for (let j = 0; j < flags.length; j++) {
      if (!SSH_ARG_FLAGS.includes(flags[j])) continue;
      if (j === flags.length - 1 && i + 1 < words.length) out.push(words[++i]);
      break;
    }
  }
  return dest ? out.join(' ') : '';
}

/** Two ssh spellings reach the same host (and port, user and options). */
export function sameSshHost(a: string | null | undefined, b: string | null | undefined): boolean {
  const x = connectArgs(a);
  return x !== '' && x === connectArgs(b);
}

/** A machine's short name, for comparing what OSC 7 says with what the host's helper says:
 *  `nova.lan.` and `NOVA` are one machine, and shells report either. */
export function shortHostname(name: string | null | undefined): string {
  return (name ?? '').trim().toLowerCase().replace(/\.$/, '').split('.')[0];
}

/**
 * The wrong-host guard (§15.3): the tab's shell reports (OSC 7) a machine that is neither the
 * one Files is showing nor this computer. That is an onward ssh from the host (the tree shows
 * the first hop while the shell works on another box). OSC 7 naming this computer is a report
 * left over from the local shell before ssh started, not a third machine; no report says
 * nothing either way.
 */
export function onAnotherHost(oscHost: string | null | undefined, localHost: string | null | undefined, remoteHost: string | null | undefined): boolean {
  const osc = shortHostname(oscHost);
  const remote = shortHostname(remoteHost);
  if (!osc || !remote || osc === 'localhost') return false;
  if (localHost && osc === shortHostname(localHost)) return false;
  return osc !== remote;
}

/** A ledger entry's path (an ssh tab's is `connect args:/abs`, §9.1) names `abs` on the root's
 *  host (`host` null: a local root, where paths are compared NFC). */
export function ledgerPathIs(entryPath: string, abs: string, host: string | null): boolean {
  if (!host) return entryPath.normalize('NFC') === abs.normalize('NFC');
  const at = entryPath.indexOf(':/');
  return at > 0 && entryPath.slice(at + 1) === abs && sameSshHost(entryPath.slice(0, at), host);
}

/** A folder on a host is in a remote root: POSIX, case-sensitive, component-wise. */
export function remoteWithin(path: string, root: string): boolean {
  const r = root.replace(/\/+$/, '');
  return path === r || path === root || path.startsWith(`${r}/`);
}
