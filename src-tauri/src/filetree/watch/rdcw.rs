//! What a ReadDirectoryChangesW buffer says, read without Win32: `windows.rs` fills the buffer,
//! this turns it into notify events. Apart so it is tested on every OS.

use std::path::PathBuf;

use notify::event::{CreateKind, Event, EventKind, Flag, ModifyKind, RemoveKind, RenameMode};

pub const FILE_ACTION_ADDED: u32 = 1;
pub const FILE_ACTION_REMOVED: u32 = 2;
pub const FILE_ACTION_MODIFIED: u32 = 3;
pub const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
pub const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

/// The `(Action, FileName)` of each `FILE_NOTIFY_INFORMATION` record in the first bytes the
/// read returned. A record that would run past `buf` ends the walk: the kernel never writes
/// one, and a bad offset must not read past the buffer.
pub fn records(buf: &[u8]) -> Vec<(u32, Vec<u16>)> {
    let u32_at = |at: usize| buf.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let (Some(next), Some(action), Some(len)) = (u32_at(at), u32_at(at + 4), u32_at(at + 8)) else { break };
        let name_at = at + 12;
        let Some(name) = buf.get(name_at..name_at + len as usize) else { break };
        out.push((action, name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()));
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    out
}

/// The event for one record, as notify 7 reported it (so the coalescer's `content_only` and
/// the root watch's filter read the same kinds). An unknown action is dropped.
pub fn event(action: u32, path: PathBuf) -> Option<Event> {
    let kind = match action {
        FILE_ACTION_ADDED => EventKind::Create(CreateKind::Any),
        FILE_ACTION_REMOVED => EventKind::Remove(RemoveKind::Any),
        FILE_ACTION_MODIFIED => EventKind::Modify(ModifyKind::Any),
        FILE_ACTION_RENAMED_OLD_NAME => EventKind::Modify(ModifyKind::Name(RenameMode::From)),
        FILE_ACTION_RENAMED_NEW_NAME => EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        _ => return None,
    };
    Some(Event::new(kind).add_path(path))
}

/// Events were lost under `dir` (the buffer overflowed): everything under it is re-listed.
pub fn rescan(dir: PathBuf) -> Event {
    Event::new(EventKind::Other).add_path(dir).set_flag(Flag::Rescan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
        let wide: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut r = Vec::new();
        r.extend(next.to_le_bytes());
        r.extend(action.to_le_bytes());
        r.extend((wide.len() as u32).to_le_bytes());
        r.extend(wide);
        r
    }

    fn names(buf: &[u8]) -> Vec<(u32, String)> {
        records(buf).into_iter().map(|(a, n)| (a, String::from_utf16(&n).unwrap())).collect()
    }

    #[test]
    fn records_are_walked_by_their_offsets() {
        // Records are DWORD aligned: the kernel pads each to the next one's offset.
        let mut first = record(0, FILE_ACTION_RENAMED_OLD_NAME, "a.ts");
        first.resize(24, 0);
        let next = first.len() as u32;
        first[..4].copy_from_slice(&next.to_le_bytes());
        let mut buf = first;
        buf.extend(record(0, FILE_ACTION_RENAMED_NEW_NAME, "src\\b.ts"));
        assert_eq!(names(&buf), vec![(FILE_ACTION_RENAMED_OLD_NAME, "a.ts".into()), (FILE_ACTION_RENAMED_NEW_NAME, "src\\b.ts".into())]);
    }

    #[test]
    fn a_record_running_past_the_bytes_read_ends_the_walk() {
        let buf = record(0, FILE_ACTION_ADDED, "new.ts");
        assert_eq!(names(&buf[..buf.len() - 1]), vec![]);
        // An offset past the end, after a whole record.
        let mut buf = record(400, FILE_ACTION_ADDED, "x");
        buf.extend([0; 8]);
        assert_eq!(names(&buf), vec![(FILE_ACTION_ADDED, "x".into())]);
        assert_eq!(names(&[]), vec![]);
    }

    #[test]
    fn actions_map_to_the_kinds_notify_reported() {
        let p = PathBuf::from("x");
        let kind = |a| event(a, p.clone()).map(|e| e.kind);
        assert_eq!(kind(FILE_ACTION_ADDED), Some(EventKind::Create(CreateKind::Any)));
        assert_eq!(kind(FILE_ACTION_REMOVED), Some(EventKind::Remove(RemoveKind::Any)));
        assert_eq!(kind(FILE_ACTION_MODIFIED), Some(EventKind::Modify(ModifyKind::Any)));
        assert_eq!(kind(FILE_ACTION_RENAMED_OLD_NAME), Some(EventKind::Modify(ModifyKind::Name(RenameMode::From))));
        assert_eq!(kind(FILE_ACTION_RENAMED_NEW_NAME), Some(EventKind::Modify(ModifyKind::Name(RenameMode::To))));
        assert_eq!(kind(9), None);
        assert!(super::super::content_only(&kind(FILE_ACTION_MODIFIED).unwrap()));
        let r = rescan(p.clone());
        assert!(r.need_rescan());
        assert_eq!(r.paths, vec![p]);
    }
}
