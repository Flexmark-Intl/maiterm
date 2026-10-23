//! Where an editor tab's remote file lives, as both `ssh` and `scp` must be told.
//!
//! The editor's remote I/O used to reduce an ssh command to its `user@host` and run
//! `scp -o BatchMode=yes user@host:path`. Everything else that says where to connect was lost:
//! a port (`-p 2222`), an identity (`-i`), a jump host (`-J`), any `-o`. So a host that Files (or
//! a terminal) reaches only through those listed fine and failed every file open, and so did one
//! reached only through the tab's MCP bridge: a password-only host's one logged-in connection is
//! the bridge tunnel's ControlMaster, which a bare BatchMode scp never used.
//!
//! Here the connect options are kept, written the one way both programs read them (`-o Key=v`:
//! scp spells the port `-P`, and its `-l` is a bandwidth limit), and the bridge tunnel to the
//! same host, if any, is muxed over (`ControlMaster=no`: a missing socket is a direct
//! BatchMode connection, never a new master).

use std::process::Command;

/// One remote destination with the options that reach it.
#[derive(Debug, Clone, PartialEq)]
pub struct SshTarget {
    /// Options for `ssh` and `scp` alike, in `-o Key=value` (or `-F file`) form.
    pub opts: Vec<String>,
    /// The destination (`user@host`, `host`).
    pub dest: String,
    /// The bridge tunnel's ControlMaster socket to this host, when there is one.
    pub control_path: Option<String>,
}

/// ssh's options that take an argument (OpenSSH's getopt string, as in `watch.rs`).
const SSH_ARG_FLAGS: &str = "bceilmopBDEFIJLOPQRSwW";

/// The `-o` form of a flag with an argument, None for one that has nothing to do with
/// reaching the host (forwards, logging, escape char, mux control).
fn option_for(flag: char, arg: &str) -> Option<Vec<String>> {
    let o = |k: &str| Some(vec!["-o".to_string(), format!("{k}={arg}")]);
    match flag {
        'p' => o("Port"),
        'l' => o("User"),
        'i' => o("IdentityFile"),
        'J' => o("ProxyJump"),
        'c' => o("Ciphers"),
        'm' => o("MACs"),
        'b' => o("BindAddress"),
        'B' => o("BindInterface"),
        'I' => o("PKCS11Provider"),
        'S' => o("ControlPath"),
        'F' => Some(vec!["-F".to_string(), arg.to_string()]),
        'o' => {
            let key = arg.split('=').next().unwrap_or("").to_ascii_lowercase();
            // Never a master or a persisted one of our own (the ControlPersist leak history),
            // and never the config's remote command or a terminal: we pass our own command.
            if matches!(key.as_str(), "controlmaster" | "controlpersist" | "remotecommand" | "requesttty") {
                None
            } else {
                Some(vec!["-o".to_string(), arg.to_string()])
            }
        }
        _ => None,
    }
}

/// The `-o` form of a flag without an argument that changes how the host is reached.
fn switch_for(flag: char) -> Option<&'static str> {
    match flag {
        '4' => Some("AddressFamily=inet"),
        '6' => Some("AddressFamily=inet6"),
        'C' => Some("Compression=yes"),
        'K' => Some("GSSAPIAuthentication=yes"),
        _ => None,
    }
}

impl SshTarget {
    /// Read an ssh command as stored or typed: `ssh …`, an ssh by another name (`/usr/bin/ssh`,
    /// `autossh -M 0 …`), or the bare args (`-p 2222 ews@nova`). A remote command after the
    /// destination is ignored.
    pub fn parse(ssh_command: &str) -> Result<SshTarget, String> {
        let mut words: Vec<&str> = ssh_command.split_whitespace().collect();
        let autossh = match words.first() {
            Some(w) => {
                let base = w.rsplit('/').next().unwrap_or(w);
                let program = base == "ssh" || base == "autossh";
                if program {
                    words.remove(0);
                }
                base == "autossh"
            }
            None => false,
        };
        let mut opts: Vec<String> = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let w = words[i];
            if w == "--" {
                let dest = words.get(i + 1).ok_or("Cannot extract host from SSH command")?;
                return Ok(SshTarget { opts, dest: dest.to_string(), control_path: None });
            }
            let Some(flags) = w.strip_prefix('-').filter(|f| !f.is_empty()) else {
                return Ok(SshTarget { opts, dest: w.to_string(), control_path: None });
            };
            // autossh's own: `-M port` (monitoring) and `-f` (background).
            if autossh && (flags == "f" || flags.starts_with('M')) {
                i += if flags == "M" { 2 } else { 1 };
                continue;
            }
            for (j, ch) in flags.char_indices() {
                if SSH_ARG_FLAGS.contains(ch) {
                    let rest = &flags[j + ch.len_utf8()..];
                    let arg = if rest.is_empty() {
                        i += 1;
                        words.get(i).copied().unwrap_or("")
                    } else {
                        rest
                    };
                    if let Some(o) = option_for(ch, arg) {
                        opts.extend(o);
                    }
                    break;
                }
                if let Some(s) = switch_for(ch) {
                    opts.push("-o".into());
                    opts.push(s.into());
                }
            }
            i += 1;
        }
        Err("Cannot extract host from SSH command".to_string())
    }

    /// `parse`, riding the bridge tunnel to the same host when there is one.
    pub fn for_command(ssh_command: &str, state: &crate::state::AppState) -> Result<SshTarget, String> {
        let mut t = SshTarget::parse(ssh_command)?;
        t.control_path = tunnel_socket(ssh_command, state);
        Ok(t)
    }

    /// Groups watched files by connection (the poller's one ssh per host).
    pub fn key(&self) -> String {
        let mut k = self.opts.join(" ");
        if !k.is_empty() {
            k.push(' ');
        }
        k.push_str(&self.dest);
        k
    }

    fn base(&self, cmd: &mut Command, connect_timeout: u32) {
        cmd.arg("-o").arg("BatchMode=yes").arg("-o").arg(format!("ConnectTimeout={connect_timeout}"));
        if let Some(sock) = &self.control_path {
            cmd.arg("-o").arg("ControlMaster=no").arg("-o").arg(format!("ControlPath={sock}"));
        }
        cmd.args(&self.opts);
    }

    /// `ssh <options> <dest>`, to which the caller adds the remote command.
    pub fn ssh(&self, connect_timeout: u32) -> Command {
        let mut c = Command::new("ssh");
        self.base(&mut c, connect_timeout);
        // A config's RemoteCommand would refuse ours; a forced terminal would mangle output.
        c.arg("-o").arg("RemoteCommand=none").arg("-T");
        c.arg(&self.dest);
        c
    }

    /// The same as `ssh` for tokio.
    pub fn ssh_async(&self, connect_timeout: u32) -> tokio::process::Command {
        tokio::process::Command::from(self.ssh(connect_timeout))
    }

    /// `scp <options>`, to which the caller adds the source and target (`remote(path)`).
    pub fn scp(&self, connect_timeout: u32) -> Command {
        let mut c = Command::new("scp");
        self.base(&mut c, connect_timeout);
        c
    }

    /// `dest:path`, scp's spelling of a file on the host.
    pub fn remote(&self, path: &str) -> String {
        format!("{}:{}", self.dest, path)
    }
}

/// The ControlMaster socket of a bridge tunnel to the host `ssh_command` reaches (compared by
/// connect args, as the transcript mirror and Files find it), if that socket exists.
fn tunnel_socket(ssh_command: &str, state: &crate::state::AppState) -> Option<String> {
    let want = crate::watch::connect_args(&without_program(ssh_command));
    if want.is_empty() {
        return None;
    }
    let key = state.ssh_tunnels.read().values().find(|t| crate::watch::connect_args(&t.host_key) == want).map(|t| t.host_key.clone())?;
    let sock = crate::commands::ssh_tunnel::cm_socket_path(&key)?;
    (cfg!(unix) && sock.exists()).then(|| sock.to_string_lossy().into_owned())
}

/// The args of an ssh command, without a leading ssh program (and autossh's own options).
fn without_program(cmd: &str) -> String {
    let mut words = cmd.split_whitespace().peekable();
    let Some(first) = words.peek().copied() else { return String::new() };
    let base = first.rsplit('/').next().unwrap_or(first);
    if base != "ssh" && base != "autossh" {
        return cmd.trim().to_string();
    }
    words.next();
    let autossh = base == "autossh";
    let mut out: Vec<&str> = Vec::new();
    while let Some(w) = words.next() {
        if autossh && w == "-M" {
            words.next();
        } else if !(autossh && (w == "-f" || w.starts_with("-M"))) {
            out.push(w);
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(c: &Command) -> Vec<String> {
        c.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn connect_options_reach_both_programs() {
        let t = SshTarget::parse("ssh -p 2222 -i ~/.ssh/k -J bastion -o StrictHostKeyChecking=no ews@nova").unwrap();
        assert_eq!(t.dest, "ews@nova");
        assert_eq!(t.opts, ["-o", "Port=2222", "-o", "IdentityFile=~/.ssh/k", "-o", "ProxyJump=bastion", "-o", "StrictHostKeyChecking=no"]);
        let scp = argv(&t.scp(10));
        assert!(scp.windows(2).any(|w| w == ["-o", "Port=2222"]), "{scp:?}");
        assert!(!scp.iter().any(|a| a == "-p" || a == "-l"), "scp reads -p and -l otherwise: {scp:?}");
        let ssh = argv(&t.ssh(5));
        assert_eq!(ssh.last().map(String::as_str), Some("ews@nova"));
        assert!(ssh.windows(2).any(|w| w == ["-o", "ProxyJump=bastion"]));
    }

    #[test]
    fn every_spelling_of_a_command_reads_the_same() {
        let bare = SshTarget::parse("-p 2222 ews@nova").unwrap();
        assert_eq!(SshTarget::parse("ssh -t -p2222 ews@nova 'cd /x && exec $SHELL -l'").unwrap(), bare);
        assert_eq!(SshTarget::parse("/usr/bin/ssh -p 2222 ews@nova").unwrap(), bare);
        assert_eq!(SshTarget::parse("autossh -M 0 -f -p 2222 ews@nova").unwrap(), bare);
        // A cluster: switches, then a flag whose argument is the rest of the word.
        let c = SshTarget::parse("-xCp2222 -l ews nova").unwrap();
        assert_eq!((c.dest.as_str(), c.opts.join(" ")), ("nova", "-o Compression=yes -o Port=2222 -o User=ews".to_string()));
        // Forwards, terminals and masters say nothing about where.
        let f = SshTarget::parse("ssh -L 8080:x:80 -R 9:y:1 -N -o ControlMaster=auto -o ControlPersist=60 -A ews@nova").unwrap();
        assert!(f.opts.is_empty(), "{:?}", f.opts);
        assert!(SshTarget::parse("ssh -p 2222").is_err());
    }

    #[test]
    fn a_bridge_tunnel_is_muxed_over() {
        let mut t = SshTarget::parse("ews@nova").unwrap();
        t.control_path = Some("/tmp/cm/ews@nova.sock".into());
        let scp = argv(&t.scp(10));
        assert!(scp.windows(2).any(|w| w == ["-o", "ControlMaster=no"]));
        assert!(scp.windows(2).any(|w| w == ["-o", "ControlPath=/tmp/cm/ews@nova.sock"]));
        assert_eq!(without_program("autossh -M 0 -x ews@nova"), "-x ews@nova");
    }
}
