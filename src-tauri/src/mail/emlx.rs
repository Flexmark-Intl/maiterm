//! Mail's message files (docs/apple-mail.md §2): where one lives, its framing, and its MIME.
//!
//! `<account>/<a>.mbox/<b>.mbox/<store uuid>/Data/<digits of ROWID/1000, reversed>/Messages/
//! <ROWID>.emlx`. A `.partial.emlx` holds the text with its attachments kept beside it in
//! `…/Attachments/<ROWID>/<part>/<filename>` (or nowhere, when Mail never fetched them).

use mail_parser::{MessageParser, MimeHeaders};
use std::path::{Path, PathBuf};

/// The folders under `Data` for a message: 6758457 → `8/5/7/6`.
pub fn digit_dirs(rowid: i64) -> PathBuf {
    let mut p = PathBuf::new();
    let n = rowid / 1000;
    if n > 0 {
        for c in n.to_string().chars().rev() {
            p.push(c.to_string());
        }
    }
    p
}

/// `Logs/Billing` → `Logs.mbox/Billing.mbox`.
pub fn mbox_dir(account_dir: &Path, mailbox_path: &str) -> PathBuf {
    let mut p = account_dir.to_path_buf();
    for part in mailbox_path.split('/').filter(|s| !s.is_empty()) {
        p.push(format!("{part}.mbox"));
    }
    p
}

/// A message's file on disk, and whether it is the partial kind.
pub struct Located {
    pub path: PathBuf,
    pub partial: bool,
    /// `…/Data/<digits>` — where the partial kind keeps its attachments.
    pub data_dir: PathBuf,
}

pub fn locate(root: &Path, account_id: &str, mailbox_path: &str, rowid: i64) -> Option<Located> {
    let mbox = mbox_dir(&root.join(account_id), mailbox_path);
    let digits = digit_dirs(rowid);
    let mut stores: Vec<PathBuf> = std::fs::read_dir(&mbox)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_none_or(|x| x != "mbox") && p.join("Data").is_dir())
        .collect();
    stores.sort();
    for store in stores {
        let data_dir = store.join("Data").join(&digits);
        for (name, partial) in [(format!("{rowid}.emlx"), false), (format!("{rowid}.partial.emlx"), true)] {
            let path = data_dir.join("Messages").join(name);
            if path.is_file() {
                return Some(Located { path, partial, data_dir });
            }
        }
    }
    None
}

/// The RFC 822 bytes inside an `.emlx`: a decimal length line, then that many bytes, then a plist.
pub fn rfc822(file: &[u8]) -> Option<&[u8]> {
    let nl = file.iter().position(|&b| b == b'\n')?;
    let len: usize = std::str::from_utf8(&file[..nl]).ok()?.trim().parse().ok()?;
    let start = nl + 1;
    Some(&file[start..(start + len).min(file.len())])
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttachmentInfo {
    pub index: usize,
    pub name: String,
    pub content_type: String,
    /// Bytes we can hand over: inline in the message, or in the partial kind's folder. 0 when
    /// Mail never downloaded it.
    pub size: usize,
    pub downloaded: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Parsed {
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub reply_to: Vec<String>,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub list_id: Option<String>,
    pub subject: Option<String>,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

fn addresses(a: Option<&mail_parser::Address>) -> Vec<String> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .map(|x| match (x.name(), x.address()) {
            (Some(n), Some(ad)) if !n.is_empty() => format!("{n} <{ad}>"),
            (_, Some(ad)) => ad.to_string(),
            (Some(n), None) => n.to_string(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// A partial message's attachment saved beside it, found by filename.
pub fn partial_attachment(data_dir: &Path, rowid: i64, name: &str) -> Option<PathBuf> {
    let base = data_dir.join("Attachments").join(rowid.to_string());
    for part in std::fs::read_dir(base).ok()?.flatten() {
        let candidate = part.path().join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn parse(bytes: &[u8], located: Option<&Located>, rowid: i64) -> Option<Parsed> {
    let msg = MessageParser::default().parse(bytes)?;
    let mut text = String::new();
    let mut i = 0;
    while let Some(t) = msg.body_text(i) {
        if !text.is_empty() { text.push_str("\n\n"); }
        text.push_str(&t);
        i += 1;
    }
    let attachments = msg
        .attachments()
        .enumerate()
        .map(|(index, part)| {
            let name = part.attachment_name().map(str::to_string).unwrap_or_else(|| format!("attachment-{}", index + 1));
            let content_type = part
                .content_type()
                .map(|c| match c.subtype() { Some(s) => format!("{}/{s}", c.ctype()), None => c.ctype().to_string() })
                .unwrap_or_else(|| "application/octet-stream".into());
            let inline = part.contents().len();
            let (size, downloaded) = if inline > 0 {
                (inline, true)
            } else {
                match located.filter(|l| l.partial).and_then(|l| partial_attachment(&l.data_dir, rowid, &name)) {
                    Some(p) => (std::fs::metadata(p).map(|m| m.len() as usize).unwrap_or(0), true),
                    None => (0, false),
                }
            };
            AttachmentInfo { index, name, content_type, size, downloaded }
        })
        .collect();
    Some(Parsed {
        from: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        reply_to: addresses(msg.reply_to()),
        date: msg.date().map(|d| d.to_rfc3339()),
        message_id: msg.message_id().map(str::to_string),
        list_id: msg.header_raw("List-Id").map(|s| s.trim().to_string()),
        subject: msg.subject().map(str::to_string),
        text,
        html: msg.body_html(0).map(|h| h.into_owned()),
        attachments,
    })
}

/// One attachment's bytes: inline, or from the partial kind's folder.
pub fn attachment_bytes(bytes: &[u8], located: Option<&Located>, rowid: i64, index: usize) -> Result<(String, Vec<u8>), String> {
    let msg = MessageParser::default().parse(bytes).ok_or("Mail's copy of this message could not be parsed")?;
    let part = msg.attachments().nth(index).ok_or("no attachment at that index")?;
    let name = part.attachment_name().map(str::to_string).unwrap_or_else(|| format!("attachment-{}", index + 1));
    let inline = part.contents();
    if !inline.is_empty() {
        return Ok((name, inline.to_vec()));
    }
    match located.filter(|l| l.partial).and_then(|l| partial_attachment(&l.data_dir, rowid, &name)) {
        Some(p) => std::fs::read(&p).map(|b| (name, b)).map_err(|e| format!("cannot read the attachment: {e}")),
        None => Err("not_downloaded".into()),
    }
}

/// Cut quoted reply history: the text from the first "On … wrote:" / "-----Original Message-----"
/// / Outlook "From: … Sent:" marker on, and any run of `>` lines. Returns the text and whether
/// anything was cut.
pub fn trim_quoted(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let mut cut_at = lines.len();
    for (i, raw) in lines.iter().enumerate() {
        let l = raw.trim();
        let wrote = l.starts_with("On ") && (l.ends_with("wrote:") || lines.get(i + 1).is_some_and(|n| n.trim().ends_with("wrote:")));
        let original = l.trim_matches('-').trim().eq_ignore_ascii_case("original message");
        let outlook = l.starts_with("From:") && lines.iter().skip(i + 1).take(4).any(|n| n.trim_start().starts_with("Sent:"));
        if (wrote || original || outlook) && i > 0 {
            cut_at = i;
            break;
        }
    }
    let kept: Vec<&str> = lines[..cut_at].iter().copied().filter(|l| !l.trim_start().starts_with('>')).collect();
    let trimmed = cut_at < lines.len() || kept.len() < cut_at;
    let mut out = kept.join("\n");
    while out.ends_with('\n') || out.ends_with(' ') { out.pop(); }
    (out, trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_reversed_thousands() {
        assert_eq!(digit_dirs(6758457), PathBuf::from("8/5/7/6"));
        assert_eq!(digit_dirs(990140), PathBuf::from("0/9/9"));
        assert_eq!(digit_dirs(999), PathBuf::new());
    }

    #[test]
    fn nested_mailboxes_nest_mbox_folders() {
        assert_eq!(mbox_dir(Path::new("/r/A"), "Logs/Billing"), PathBuf::from("/r/A/Logs.mbox/Billing.mbox"));
    }

    const MIME: &str = "From: Acme Billing <billing@acme.test>\r\nTo: me@example.test\r\nSubject: =?utf-8?q?Invoice_=E2=84=96_42?=\r\nMessage-ID: <abc@acme.test>\r\nDate: Mon, 23 Nov 2026 10:00:00 -0600\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nWW91ciBpbnZvaWNlIGlzIGF0dGFjaGVkLg==\r\n--b\r\nContent-Type: application/pdf; name=inv.pdf\r\nContent-Disposition: attachment; filename=inv.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0x\r\n--b--\r\n";

    fn emlx(body: &str) -> Vec<u8> {
        let mut v = format!("{}\n", body.len()).into_bytes();
        v.extend_from_slice(body.as_bytes());
        v.extend_from_slice(b"<?xml version=\"1.0\"?><plist><dict/></plist>");
        v
    }

    #[test]
    fn framing_then_mime_decodes_base64_and_encoded_words() {
        let file = emlx(MIME);
        let raw = rfc822(&file).unwrap();
        assert_eq!(raw.len(), MIME.len());
        let p = parse(raw, None, 1).unwrap();
        assert_eq!(p.text.trim(), "Your invoice is attached.");
        assert_eq!(p.subject.as_deref(), Some("Invoice № 42"));
        assert_eq!(p.from, vec!["Acme Billing <billing@acme.test>"]);
        assert_eq!(p.message_id.as_deref(), Some("abc@acme.test"));
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].name, "inv.pdf");
        assert_eq!(p.attachments[0].content_type, "application/pdf");
        assert!(p.attachments[0].downloaded);
        let (name, bytes) = attachment_bytes(raw, None, 1, 0).unwrap();
        assert_eq!((name.as_str(), bytes.as_slice()), ("inv.pdf", b"%PDF-1".as_slice()));
    }

    #[test]
    fn a_partial_message_finds_its_attachment_beside_it() {
        let dir = std::env::temp_dir().join(format!("maiterm-emlx-{}", uuid::Uuid::new_v4()));
        let data = dir.join("A/INBOX.mbox/STORE-1/Data/1");
        std::fs::create_dir_all(data.join("Messages")).unwrap();
        std::fs::create_dir_all(data.join("Attachments/1234/2")).unwrap();
        let body = MIME.replace("JVBERi0x\r\n", "");
        std::fs::write(data.join("Messages/1234.partial.emlx"), emlx(&body)).unwrap();
        std::fs::write(data.join("Attachments/1234/2/inv.pdf"), b"%PDF-full").unwrap();
        let loc = locate(&dir, "A", "INBOX", 1234).unwrap();
        assert!(loc.partial);
        let file = std::fs::read(&loc.path).unwrap();
        let raw = rfc822(&file).unwrap();
        let p = parse(raw, Some(&loc), 1234).unwrap();
        assert_eq!((p.attachments[0].size, p.attachments[0].downloaded), (9, true));
        assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 0).unwrap().1, b"%PDF-full");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quoted_history_is_trimmed() {
        let (t, cut) = trim_quoted("Sounds good.\n\nOn Mon, Nov 23, 2026 at 10:00 Pat <p@x.test>\nwrote:\n> earlier\n> text");
        assert_eq!((t.as_str(), cut), ("Sounds good.", true));
        let (t, cut) = trim_quoted("Yes.\n-----Original Message-----\nFrom: a\nSent: b");
        assert_eq!((t.as_str(), cut), ("Yes.", true));
        let (t, cut) = trim_quoted("No quotes here.\nSecond line.");
        assert_eq!((t.as_str(), cut), ("No quotes here.\nSecond line.", false));
    }
}
