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

/// A partial message's attachment saved beside it, in `Attachments/<ROWID>/<section>/`, where the
/// folder is the part's MIME section number ("2", "2.1"). Measured on a real store: 3001 of 3015
/// such folders are a section of their message, and 3005 hold exactly one file.
///
/// The folder is found by the section, which we compute, never by the sender's filename: a name
/// is never joined onto a path (`/abs` or `../` would read any file maiTerm can), and same-named
/// attachments each have their own section. Inside the folder the one file is taken; only a
/// folder with several is narrowed by exact name.
pub fn partial_attachment(data_dir: &Path, rowid: i64, section: &str, name: &str) -> Option<PathBuf> {
    if section.is_empty() || !section.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let dir = data_dir.join("Attachments").join(rowid.to_string()).join(section);
    let files: Vec<std::fs::DirEntry> = std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_file()).collect();
    match files.len() {
        1 => Some(files[0].path()),
        _ => files.iter().find(|e| e.file_name().to_str() == Some(name)).map(|e| e.path()),
    }
}

/// Each part's MIME section number (IMAP numbering): a multipart's children are `1`, `2`…, a
/// nested one's `2.1`…; a message that isn't multipart has the one part `1`.
fn sections(msg: &mail_parser::Message) -> std::collections::HashMap<u32, String> {
    fn walk(msg: &mail_parser::Message, id: u32, sec: String, out: &mut std::collections::HashMap<u32, String>) {
        let Some(part) = msg.parts.get(id as usize) else { return };
        if let mail_parser::PartType::Multipart(children) = &part.body {
            for (i, child) in children.iter().enumerate() {
                let s = if sec.is_empty() { (i + 1).to_string() } else { format!("{sec}.{}", i + 1) };
                walk(msg, *child, s, out);
            }
        }
        out.insert(id, sec);
    }
    let mut out = std::collections::HashMap::new();
    walk(msg, 0, String::new(), &mut out);
    if out.get(&0).is_some_and(|s| s.is_empty()) && !matches!(msg.parts.first().map(|p| &p.body), Some(mail_parser::PartType::Multipart(_))) {
        out.insert(0, "1".into());
    }
    out
}

/// The section of the `index`-th attachment.
fn attachment_section(msg: &mail_parser::Message, secs: &std::collections::HashMap<u32, String>, index: usize) -> String {
    msg.attachments.get(index).and_then(|id| secs.get(id)).cloned().unwrap_or_default()
}

fn attachment_name(part: &mail_parser::MessagePart, index: usize) -> String {
    part.attachment_name().map(str::to_string).unwrap_or_else(|| format!("attachment-{}", index + 1))
}

pub fn parse(bytes: &[u8], located: Option<&Located>, rowid: i64) -> Option<Parsed> {
    let msg = MessageParser::default().parse(bytes)?;
    // Every text part: an inline image between two paragraphs is IN `text_body` and has no text,
    // so stopping at the first part without any would drop everything after the picture.
    let join = |parts: Vec<String>| parts.into_iter().filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n\n");
    let text = join((0..msg.text_body.len()).filter_map(|i| msg.body_text(i).map(|t| t.into_owned())).collect());
    let html = join((0..msg.html_body.len()).filter_map(|i| msg.body_html(i).map(|t| t.into_owned())).collect());
    let secs = sections(&msg);
    let attachments = msg
        .attachments()
        .enumerate()
        .map(|(index, part)| {
            let name = attachment_name(part, index);
            let content_type = part
                .content_type()
                .map(|c| match c.subtype() { Some(s) => format!("{}/{s}", c.ctype()), None => c.ctype().to_string() })
                .unwrap_or_else(|| "application/octet-stream".into());
            let inline = part.contents().len();
            let (size, downloaded) = if inline > 0 {
                (inline, true)
            } else {
                match located.filter(|l| l.partial).and_then(|l| partial_attachment(&l.data_dir, rowid, &attachment_section(&msg, &secs, index), &name)) {
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
        html: (!html.is_empty()).then_some(html),
        attachments,
    })
}

/// One attachment's bytes: inline, or from the partial kind's folder.
pub fn attachment_bytes(bytes: &[u8], located: Option<&Located>, rowid: i64, index: usize) -> Result<(String, Vec<u8>), String> {
    let msg = MessageParser::default().parse(bytes).ok_or("Mail's copy of this message could not be parsed")?;
    let part = msg.attachments().nth(index).ok_or("no attachment at that index")?;
    let name = attachment_name(part, index);
    let inline = part.contents();
    if !inline.is_empty() {
        return Ok((name, inline.to_vec()));
    }
    let section = attachment_section(&msg, &sections(&msg), index);
    match located.filter(|l| l.partial).and_then(|l| partial_attachment(&l.data_dir, rowid, &section, &name)) {
        Some(p) => std::fs::read(&p).map(|b| (name, b)).map_err(|e| format!("cannot read the attachment: {e}")),
        None => Err("not_downloaded".into()),
    }
}

/// Cut the quoted history at the END of a reply: the trailing run of `>`-quoted lines, and the
/// "… wrote:" attribution just above it. Returns the text and whether anything was cut.
///
/// Only text the sender's client explicitly marked as quoted is cut. Guessing from wording ("On …
/// wrote:", "Original Message", Outlook's "From:/Sent:") was tried twice and each version either
/// dropped a bottom-posted reply or missed Apple Mail's own format, so it is deliberately not
/// done: a missed trim costs space, a wrong one loses the reply. HTML-only mail (no `>` once
/// converted) and Outlook-style history therefore come back whole.
pub fn trim_quoted(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let blank = |l: &str| l.trim().is_empty();
    let quoted = |l: &str| l.trim_start().starts_with('>');
    let mut end = lines.len();
    while end > 0 && blank(lines[end - 1]) { end -= 1; }
    let mut start = end;
    while start > 0 && (quoted(lines[start - 1]) || blank(lines[start - 1])) { start -= 1; }
    if start == end || !lines[start..end].iter().any(|l| quoted(l)) {
        return (text.trim_end().to_string(), false);
    }
    // The attribution line(s) right above the quote: "On … wrote:", possibly wrapped onto two.
    let mut cut = start;
    if cut > 0 && lines[cut - 1].trim_end().ends_with(':') && lines[cut - 1].contains("wrote") {
        cut -= 1;
        if cut > 0 && lines[cut - 1].trim_start().starts_with("On ") && !lines[cut].trim_start().starts_with("On ") {
            cut -= 1;
        }
    }
    let kept = lines[..cut].join("\n");
    let kept = kept.trim_end();
    if kept.is_empty() {
        // Nothing but a quote: keep it, there is no reply to show instead.
        return (text.trim_end().to_string(), false);
    }
    (kept.to_string(), true)
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
    fn a_senders_attachment_name_cannot_reach_outside_the_message_folder() {
        let dir = std::env::temp_dir().join(format!("maiterm-emlx-{}", uuid::Uuid::new_v4()));
        let data = dir.join("A/INBOX.mbox/STORE-1/Data/1");
        std::fs::create_dir_all(data.join("Messages")).unwrap();
        std::fs::create_dir_all(data.join("Attachments/1234/2")).unwrap();
        let secret = dir.join("SECRET.txt");
        std::fs::write(&secret, b"outside").unwrap();
        for evil in [secret.to_string_lossy().to_string(), "../../../../../../../SECRET.txt".to_string()] {
            let body = MIME.replace("JVBERi0x\r\n", "").replace("filename=inv.pdf", &format!("filename=\"{evil}\""));
            std::fs::write(data.join("Messages/1234.partial.emlx"), emlx(&body)).unwrap();
            let loc = locate(&dir, "A", "INBOX", 1234).unwrap();
            let file = std::fs::read(&loc.path).unwrap();
            let raw = rfc822(&file).unwrap();
            let p = parse(raw, Some(&loc), 1234).unwrap();
            assert!(!p.attachments[0].downloaded, "{evil}");
            assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 0).unwrap_err(), "not_downloaded", "{evil}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_named_attachments_each_get_their_own_file() {
        let dir = std::env::temp_dir().join(format!("maiterm-emlx-{}", uuid::Uuid::new_v4()));
        let data = dir.join("A/INBOX.mbox/STORE-1/Data/1");
        std::fs::create_dir_all(data.join("Messages")).unwrap();
        // Parts: 1 text, 2 inv.pdf, 3 inv.pdf. Mail fetched only part 3 at first.
        let pdf = "--b\r\nContent-Type: application/pdf; name=inv.pdf\r\nContent-Disposition: attachment; filename=inv.pdf\r\n\r\n\r\n";
        let body = MIME.replace("--b\r\nContent-Type: application/pdf", &format!("{pdf}--b\r\nContent-Type: application/pdf")).replace("JVBERi0x\r\n", "");
        std::fs::write(data.join("Messages/1234.partial.emlx"), emlx(&body)).unwrap();
        std::fs::create_dir_all(data.join("Attachments/1234/3")).unwrap();
        std::fs::write(data.join("Attachments/1234/3/inv.pdf"), "second").unwrap();
        let loc = locate(&dir, "A", "INBOX", 1234).unwrap();
        let file = std::fs::read(&loc.path).unwrap();
        let raw = rfc822(&file).unwrap();
        assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 0).unwrap_err(), "not_downloaded");
        assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 1).unwrap().1, b"second");
        let p = parse(raw, Some(&loc), 1234).unwrap();
        assert_eq!(p.attachments.iter().map(|a| a.downloaded).collect::<Vec<_>>(), vec![false, true]);
        // Then part 2 too.
        std::fs::create_dir_all(data.join("Attachments/1234/2")).unwrap();
        std::fs::write(data.join("Attachments/1234/2/inv.pdf"), "first").unwrap();
        assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 0).unwrap().1, b"first");
        assert_eq!(attachment_bytes(raw, Some(&loc), 1234, 1).unwrap().1, b"second");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_after_an_inline_image_is_kept() {
        let m = "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nPart one\r\n--b\r\nContent-Type: image/png\r\nContent-Disposition: inline; filename=IMG.png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0K\r\n--b\r\nContent-Type: text/plain\r\n\r\nPart two\r\n--b--\r\n";
        let p = parse(m.as_bytes(), None, 1).unwrap();
        assert!(p.text.contains("Part one") && p.text.contains("Part two"), "{:?}", p.text);
    }

    #[test]
    fn quoted_history_is_trimmed() {
        let (t, cut) = trim_quoted("Sounds good.\n\nOn Mon, Nov 23, 2026 at 10:00 Pat <p@x.test>\nwrote:\n> earlier\n> text");
        assert_eq!((t.as_str(), cut), ("Sounds good.", true));
        // Bottom-posted: the answer is below the quote, so nothing is cut.
        let (t, cut) = trim_quoted("Hi,\n\nOn Mon, Pat wrote:\n> are you free Friday?\n\nYes, Friday works.");
        assert!(t.ends_with("Yes, Friday works.") && !cut);
        // Apple Mail / iOS: the attribution is inside the quote.
        let (t, cut) = trim_quoted("Sounds good\n\nSent from my iPhone\n\n> On Nov 23, 2026, at 10:00, Pat <p@x.test> wrote:\n> \n> are you free Friday?\n");
        assert_eq!((t.as_str(), cut), ("Sounds good\n\nSent from my iPhone", true));
        // Interleaved answers end unquoted: nothing is cut.
        let (_, cut) = trim_quoted("> question one\nanswer one\n> question two\nanswer two");
        assert!(!cut);
        // A list footer after the quote: whole, rather than guess.
        let (_, cut) = trim_quoted("Sure\n\nOn Mon, Pat wrote:\n> free Friday?\n\n____\nlist footer");
        assert!(!cut);
        // Unmarked history (Outlook, HTML converted to text) is left whole.
        let (t, cut) = trim_quoted("Yes.\n-----Original Message-----\nFrom: a\nSent: b");
        assert_eq!((t.as_str(), cut), ("Yes.\n-----Original Message-----\nFrom: a\nSent: b", false));
        // Nothing but a quote: kept.
        let (_, cut) = trim_quoted("> only a quote");
        assert!(!cut);
        let (t, cut) = trim_quoted("No quotes here.\nSecond line.");
        assert_eq!((t.as_str(), cut), ("No quotes here.\nSecond line.", false));
    }
}
