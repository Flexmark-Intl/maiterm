//! Is a folder on a network mount? (docs/file-tree.md §6.4)
//!
//! A network mount gets no watcher: FSEvents, inotify and SMB change notification all miss
//! writes made by another machine, so a "live" tree there would quietly lie. It is revalidated
//! on expand, on window focus and by a slow poll of the folders on screen instead.
//!
//! The answer is a String, never an enum: "local" | "network" | "unknown".

use std::path::Path;

pub const LOCAL: &str = "local";
pub const NETWORK: &str = "network";
pub const UNKNOWN: &str = "unknown";

#[cfg(target_os = "macos")]
pub fn classify(path: &Path) -> &'static str {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return UNKNOWN };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` is a properly sized out-parameter.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return UNKNOWN;
    }
    let name: Vec<u8> = st.f_fstypename.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
    classify_fstypename(&String::from_utf8_lossy(&name))
}

/// macOS `f_fstypename` values that live on another machine.
#[cfg(any(target_os = "macos", test))]
fn classify_fstypename(name: &str) -> &'static str {
    match name {
        "smbfs" | "nfs" | "afpfs" | "webdav" | "macfuse" | "osxfuse" | "cifs" => NETWORK,
        "" => UNKNOWN,
        _ => LOCAL,
    }
}

#[cfg(target_os = "linux")]
pub fn classify(path: &Path) -> &'static str {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return UNKNOWN };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return UNKNOWN;
    }
    classify_magic(st.f_type as i64)
}

/// Linux `f_type` magics for remote filesystems.
#[cfg(any(target_os = "linux", test))]
fn classify_magic(magic: i64) -> &'static str {
    const NFS: i64 = 0x6969;
    const SMB: i64 = 0x517B;
    const CIFS: i64 = 0xFF53_4D42;
    const SMB2: i64 = 0xFE53_4D42;
    const FUSE: i64 = 0x6573_5546;
    const CODA: i64 = 0x7375_7245;
    const AFS: i64 = 0x5346_414F;
    // f_type is a signed word on some targets: compare the low 32 bits.
    match magic & 0xFFFF_FFFF {
        NFS | SMB | CIFS | SMB2 | FUSE | CODA | AFS => NETWORK,
        _ => LOCAL,
    }
}

#[cfg(windows)]
pub fn classify(path: &Path) -> &'static str {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOTE: u32 = 4;
    // A UNC share is remote by definition; a drive letter may be a mapped network drive.
    let root = match path.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return NETWORK,
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:\\", d as char),
            _ => return UNKNOWN,
        },
        _ => return UNKNOWN,
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&root).encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 root path.
    match unsafe { GetDriveTypeW(wide.as_ptr()) } {
        DRIVE_REMOTE => NETWORK,
        0 | 1 => UNKNOWN,
        _ => LOCAL,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn classify(_path: &Path) -> &'static str {
    UNKNOWN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_filesystems_are_named() {
        assert_eq!(classify_fstypename("smbfs"), NETWORK);
        assert_eq!(classify_fstypename("nfs"), NETWORK);
        assert_eq!(classify_fstypename("apfs"), LOCAL);
        assert_eq!(classify_magic(0x6969), NETWORK);
        assert_eq!(classify_magic(0xFF53_4D42u32 as i32 as i64), NETWORK, "a sign-extended CIFS magic");
        assert_eq!(classify_magic(0xEF53), LOCAL, "ext4");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_temp_dir_is_local() {
        assert_eq!(classify(&std::env::temp_dir()), LOCAL);
    }
}
