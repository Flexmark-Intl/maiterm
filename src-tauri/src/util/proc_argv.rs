//! A process's argv as the kernel holds it, one entry per word.
//!
//! `ps -o command=` joins argv with spaces, so a word with a space in it (`-i "/Users/me/My
//! Keys/id"`) comes back as two, and a command rebuilt from it connects somewhere else. Where a
//! word boundary matters (the ssh command maiTerm reuses for every ssh it makes for a tab), read
//! argv here and keep it as words (`util::ssh_words::join`).
//!
//! - macOS: `sysctl` `KERN_PROCARGS2`.
//! - Linux: `/proc/<pid>/cmdline`.
//! - Windows: not here. `pty::manager`'s Windows foreground walk already holds sysinfo's
//!   `cmd()` for every process, which is the command line split as the C runtime splits it
//!   (`CommandLineToArgvW`) — what ssh itself saw as argv — and joins that.
//!
//! None when the process is gone or not ours to read; callers fall back to the `ps` line and
//! must treat what that gives them as lossy.

/// `pid`'s argv, or None if it can't be read.
#[cfg(target_os = "macos")]
pub fn argv(pid: u32) -> Option<Vec<String>> {
    use std::ffi::c_void;
    use std::ptr;

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut argmax: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: `mib` names a 2-level int sysctl and `argmax`/`size` describe a c_int buffer.
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, &mut argmax as *mut _ as *mut c_void, &mut size, ptr::null_mut(), 0) };
    if rc != 0 || argmax <= 0 {
        return None;
    }
    let mut buf = vec![0u8; argmax as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buf` is `size` bytes long and the kernel writes at most `size` into it.
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void, &mut size, ptr::null_mut(), 0) };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

/// `KERN_PROCARGS2`'s layout: `argc` (a native int), the exec path, NUL padding, then `argc`
/// NUL-terminated words (the environment follows; it is not read).
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let rest = &buf[4..];
    let path_end = rest.iter().position(|b| *b == 0)?;
    let mut at = path_end;
    while rest.get(at) == Some(&0) {
        at += 1;
    }
    let mut out = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        let tail = rest.get(at..)?;
        let len = tail.iter().position(|b| *b == 0)?;
        out.push(String::from_utf8_lossy(&tail[..len]).into_owned());
        at += len + 1;
    }
    Some(out)
}

#[cfg(target_os = "linux")]
pub fn argv(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    parse_cmdline(&raw)
}

/// `/proc/<pid>/cmdline`: the words, each NUL-terminated. Empty for a zombie or a kernel thread.
#[cfg(any(target_os = "linux", test))]
fn parse_cmdline(raw: &[u8]) -> Option<Vec<String>> {
    let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
    if raw.is_empty() {
        return None;
    }
    Some(raw.split(|b| *b == 0).map(|w| String::from_utf8_lossy(w).into_owned()).collect())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub fn argv(_pid: u32) -> Option<Vec<String>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procargs2_is_read_word_by_word() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/ssh\0\0\0\0ssh\0-i\0/Users/me/My Keys/id\0HOME=/x\0");
        assert_eq!(parse_procargs2(&buf).unwrap(), ["ssh", "-i", "/Users/me/My Keys/id"]);
        // A truncated buffer is unreadable, not a shorter argv.
        assert!(parse_procargs2(&buf[..buf.len() - 30]).is_none());
        assert!(parse_procargs2(&0i32.to_ne_bytes()).is_none());
    }

    #[test]
    fn cmdline_is_read_word_by_word() {
        assert_eq!(parse_cmdline(b"ssh\0-i\0/Users/me/My Keys/id\0ews@nova\0").unwrap(), ["ssh", "-i", "/Users/me/My Keys/id", "ews@nova"]);
        assert_eq!(parse_cmdline(b"ssh\0\0x\0").unwrap(), ["ssh", "", "x"], "an empty word is a word");
        assert!(parse_cmdline(b"").is_none());
    }

    /// The real thing: a child started with hard words reads back with every boundary kept.
    /// The child waits on its stdin, so it is alive for exactly as long as the test needs it.
    #[cfg(unix)]
    #[test]
    fn a_live_process_reads_back_exactly() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let words: Vec<&str> = crate::util::ssh_words::HARD_WORDS.to_vec();
        // bash, not sh: macOS's /bin/sh is a launcher that execs the real shell.
        let shell = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        let mut child = Command::new(shell)
            .arg("-c")
            .arg("read _")
            .arg("sh")
            .args(&words)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn sh");
        let got = argv(child.id());
        let _ = child.stdin.take().map(|mut s| s.write_all(b"\n"));
        let _ = child.wait();
        let got = got.expect("argv of our own child");
        assert_eq!(&got[..4], [shell, "-c", "read _", "sh"]);
        assert_eq!(&got[4..], words.as_slice());
    }
}
