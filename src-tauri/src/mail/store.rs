//! Apple Mail's own index, read-only (docs/apple-mail.md §2).
//!
//! `Envelope Index` is a live WAL database Mail writes all the time: every call opens it
//! read-only (never `immutable`, which would ignore the WAL and read stale pages), uses it, and
//! drops it. The schema is Apple's private one, so `open` checks the columns we use and answers
//! with a sentence when one is missing instead of a SQL error.

use rusqlite::{params_from_iter, Connection, OpenFlags, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Columns this module reads, by table. Checked once per open.
const REQUIRED: &[(&str, &[&str])] = &[
    ("messages", &["ROWID", "global_message_id", "sender", "subject_prefix", "subject", "summary", "date_received", "mailbox", "read", "flagged", "deleted", "conversation_id", "size"]),
    ("mailboxes", &["ROWID", "url", "total_count", "unread_count", "source"]),
    ("addresses", &["ROWID", "address", "comment"]),
    ("subjects", &["ROWID", "subject"]),
    ("summaries", &["ROWID", "summary"]),
    ("recipients", &["message", "address", "type"]),
    ("attachments", &["message", "name"]),
    ("labels", &["message_id", "mailbox_id"]),
    ("message_global_data", &["ROWID", "message_id_header"]),
];

/// Mail keeps one versioned folder per store format (`V8`, `V10`…); the newest one that holds an
/// index is the live one.
pub fn mail_root(home: &Path) -> Option<PathBuf> {
    let base = home.join("Library/Mail");
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in std::fs::read_dir(&base).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(n) = name.strip_prefix('V').and_then(|v| v.parse::<u32>().ok()) else { continue };
        let dir = entry.path();
        if dir.join("MailData/Envelope Index").exists() && best.as_ref().map_or(true, |(b, _)| n > *b) {
            best = Some((n, dir));
        }
    }
    best.map(|(_, d)| d)
}

#[derive(Debug)]
pub enum OpenError {
    /// The files exist but macOS won't let maiTerm read them.
    NoAccess(String),
    /// The schema changed under us (a macOS update).
    Schema(String),
}

pub fn open(root: &Path) -> Result<Connection, OpenError> {
    let path = root.join("MailData/Envelope Index");
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| OpenError::NoAccess(e.to_string()))?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(3));
    for (table, cols) in REQUIRED {
        let have: HashSet<String> = match conn.prepare(&format!("PRAGMA table_info({table})")) {
            Ok(mut st) => st
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| OpenError::NoAccess(e.to_string()))?
                .flatten()
                .collect(),
            // A database macOS won't let us read fails here, not at open.
            Err(e) => return Err(OpenError::NoAccess(e.to_string())),
        };
        if have.is_empty() {
            return Err(OpenError::Schema(format!("Mail's index has no `{table}` table")));
        }
        for c in *cols {
            if !have.contains(*c) && *c != "ROWID" {
                return Err(OpenError::Schema(format!("Mail's index has no `{table}.{c}` column")));
            }
        }
    }
    Ok(conn)
}

// ---------------------------------------------------------------------------------------------
// Accounts and mailboxes
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    /// imap, local, ews, pop
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct Mailbox {
    pub id: i64,
    pub account_id: String,
    /// Decoded path below the account: `INBOX`, `Logs/Billing`.
    pub path: String,
    pub total: i64,
    pub unread: i64,
    /// A Gmail-style label: its messages are in `labels`, not `messages.mailbox`.
    pub is_label: bool,
    pub kind: &'static str,
}

impl Mailbox {
    /// Junk, Trash and Drafts are left out of every scope that doesn't name them.
    pub fn excluded_by_default(&self) -> bool {
        matches!(self.kind, "junk" | "trash" | "drafts")
    }
}

/// `imap://<uuid>/INBOX` → (scheme, uuid, decoded path).
pub fn split_mailbox_url(url: &str) -> Option<(String, String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let (account, path) = rest.split_once('/').unwrap_or((rest, ""));
    Some((scheme.to_string(), account.to_string(), percent_decode(path)))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a mailbox is for, from its last path component. Providers spell these differently.
pub fn mailbox_kind(path: &str, is_label: bool) -> &'static str {
    let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match leaf.as_str() {
        "inbox" => "inbox",
        "sent" | "sent messages" | "sent items" | "sent mail" => "sent",
        "drafts" | "draft" => "drafts",
        "junk" | "spam" | "junk e-mail" | "junk email" | "bulk mail" => "junk",
        "trash" | "deleted messages" | "deleted items" | "bin" | "deleted" => "trash",
        "archive" | "all mail" | "archives" => "archive",
        _ if is_label => "label",
        _ => "folder",
    }
}

pub fn load_mailboxes(conn: &Connection) -> rusqlite::Result<Vec<Mailbox>> {
    let mut st = conn.prepare("SELECT ROWID, url, total_count, unread_count, source FROM mailboxes")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (id, url, total, unread, source) = row;
        let Some((_, account_id, path)) = split_mailbox_url(&url) else { continue };
        let is_label = source.is_some();
        out.push(Mailbox { id, account_id, kind: mailbox_kind(&path, is_label), path, total, unread, is_label });
    }
    Ok(out)
}

/// Account names come from the system accounts database; Mail's index knows only UUIDs. Some
/// accounts carry no description of their own and take their parent's.
pub fn load_accounts(conn: &Connection, home: &Path) -> Vec<Account> {
    let mut seen: Vec<(String, String)> = Vec::new();
    if let Ok(mut st) = conn.prepare("SELECT DISTINCT url FROM mailboxes") {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
            for url in rows.flatten() {
                if let Some((scheme, id, _)) = split_mailbox_url(&url) {
                    if !seen.iter().any(|(_, s)| *s == id) {
                        seen.push((scheme, id));
                    }
                }
            }
        }
    }
    let names = system_account_names(home);
    seen.into_iter()
        .map(|(kind, id)| {
            let (name, email) = names.get(&id).cloned().unwrap_or((None, None));
            let name = name
                .or_else(|| email.clone())
                .unwrap_or_else(|| if kind == "local" { "On My Mac".to_string() } else { format!("Account {}", &id[..id.len().min(4)]) });
            Account { id, name, email, kind }
        })
        .collect()
}

fn system_account_names(home: &Path) -> HashMap<String, (Option<String>, Option<String>)> {
    let mut out = HashMap::new();
    let path = home.join("Library/Accounts/Accounts4.sqlite");
    let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else { return out };
    let Ok(mut st) = conn.prepare(
        "SELECT a.ZIDENTIFIER, a.ZACCOUNTDESCRIPTION, a.ZUSERNAME, p.ZACCOUNTDESCRIPTION, p.ZUSERNAME \
         FROM ZACCOUNT a LEFT JOIN ZACCOUNT p ON a.ZPARENTACCOUNT = p.Z_PK",
    ) else { return out };
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?))
    });
    if let Ok(rows) = rows {
        for (id, desc, user, pdesc, puser) in rows.flatten() {
            let Some(id) = id else { continue };
            let nonempty = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
            let email = nonempty(user).or_else(|| nonempty(puser)).filter(|u| u.contains('@'));
            out.insert(id, (nonempty(desc).or_else(|| nonempty(pdesc)), email));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------------------------

/// Which mailboxes a query covers.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Mailboxes whose messages are `messages.mailbox`.
    pub direct: Vec<i64>,
    /// Label mailboxes, reached through `labels`.
    pub labels: Vec<i64>,
}

impl Scope {
    pub fn from_mailboxes<'a>(boxes: impl IntoIterator<Item = &'a Mailbox>) -> Self {
        let mut s = Scope::default();
        for b in boxes {
            if b.is_label { s.labels.push(b.id) } else { s.direct.push(b.id) }
        }
        s
    }
    /// SQL condition on `m`, with its parameters appended to `params`.
    fn sql(&self, params: &mut Vec<rusqlite::types::Value>) -> String {
        let mut parts = Vec::new();
        if !self.direct.is_empty() {
            parts.push(format!("m.mailbox IN ({})", placeholders(self.direct.len())));
            params.extend(self.direct.iter().map(|v| rusqlite::types::Value::Integer(*v)));
        }
        if !self.labels.is_empty() {
            parts.push(format!("m.ROWID IN (SELECT message_id FROM labels WHERE mailbox_id IN ({}))", placeholders(self.labels.len())));
            params.extend(self.labels.iter().map(|v| rusqlite::types::Value::Integer(*v)));
        }
        if parts.is_empty() { "0".into() } else { format!("({})", parts.join(" OR ")) }
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// `LIKE` pattern for a substring, with `\` as the escape.
pub fn like(s: &str) -> String {
    let mut out = String::from("%");
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') { out.push('\\'); }
        out.push(c);
    }
    out.push('%');
    out
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub scope: Scope,
    /// Every term must appear in the subject, the sender, or Mail's preview.
    pub terms: Vec<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub has_attachment: bool,
    pub attachment_name: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub unread_only: bool,
    pub flagged_only: bool,
    /// Paging: strictly older than this (date_received, ROWID).
    pub before: Option<(i64, i64)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    pub rowid: i64,
    pub global_id: i64,
    pub mailbox: i64,
    pub date: i64,
    pub from_address: String,
    pub from_name: String,
    pub subject: String,
    pub preview: String,
    pub read: bool,
    pub flagged: bool,
    pub conversation_id: i64,
    pub size: i64,
}

const ROW_COLS: &str = "m.ROWID, m.global_message_id, m.mailbox, COALESCE(m.date_received, 0), \
     COALESCE(a.address, ''), COALESCE(a.comment, ''), COALESCE(m.subject_prefix, '') || COALESCE(s.subject, ''), \
     COALESCE(su.summary, ''), m.read, m.flagged, m.conversation_id, m.size";

const ROW_JOINS: &str = "FROM messages m \
     LEFT JOIN addresses a ON a.ROWID = m.sender \
     LEFT JOIN subjects s ON s.ROWID = m.subject \
     LEFT JOIN summaries su ON su.ROWID = m.summary";

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        rowid: r.get(0)?,
        global_id: r.get(1)?,
        mailbox: r.get(2)?,
        date: r.get(3)?,
        from_address: r.get(4)?,
        from_name: r.get(5)?,
        subject: r.get(6)?,
        preview: r.get(7)?,
        read: r.get::<_, i64>(8)? != 0,
        flagged: r.get::<_, i64>(9)? != 0,
        conversation_id: r.get(10)?,
        size: r.get(11)?,
    })
}

/// The WHERE clause for a filter. `with_terms` false leaves the free-text terms to the caller
/// (the body scan matches them itself).
fn where_clause(f: &Filter, with_terms: bool, params: &mut Vec<rusqlite::types::Value>) -> String {
    use rusqlite::types::Value as V;
    let mut w = vec!["m.deleted = 0".to_string(), f.scope.sql(params)];
    if with_terms {
        for t in &f.terms {
            w.push("(s.subject LIKE ? ESCAPE '\\' OR a.address LIKE ? ESCAPE '\\' OR a.comment LIKE ? ESCAPE '\\' OR su.summary LIKE ? ESCAPE '\\')".into());
            for _ in 0..4 { params.push(V::Text(like(t))); }
        }
    }
    if let Some(v) = &f.from {
        w.push("(a.address LIKE ? ESCAPE '\\' OR a.comment LIKE ? ESCAPE '\\')".into());
        params.push(V::Text(like(v)));
        params.push(V::Text(like(v)));
    }
    if let Some(v) = &f.to {
        w.push("EXISTS (SELECT 1 FROM recipients r JOIN addresses ra ON ra.ROWID = r.address WHERE r.message = m.ROWID AND (ra.address LIKE ? ESCAPE '\\' OR ra.comment LIKE ? ESCAPE '\\'))".into());
        params.push(V::Text(like(v)));
        params.push(V::Text(like(v)));
    }
    if let Some(v) = &f.subject {
        w.push("s.subject LIKE ? ESCAPE '\\'".into());
        params.push(V::Text(like(v)));
    }
    if f.has_attachment {
        w.push("EXISTS (SELECT 1 FROM attachments at WHERE at.message = m.ROWID)".into());
    }
    if let Some(v) = &f.attachment_name {
        w.push("EXISTS (SELECT 1 FROM attachments at WHERE at.message = m.ROWID AND at.name LIKE ? ESCAPE '\\')".into());
        params.push(V::Text(like(v)));
    }
    if let Some(v) = f.since {
        w.push("m.date_received >= ?".into());
        params.push(V::Integer(v));
    }
    if let Some(v) = f.until {
        w.push("m.date_received < ?".into());
        params.push(V::Integer(v));
    }
    if f.unread_only { w.push("m.read = 0".into()); }
    if f.flagged_only { w.push("m.flagged = 1".into()); }
    if let Some((d, id)) = f.before {
        w.push("(m.date_received < ? OR (m.date_received = ? AND m.ROWID < ?))".into());
        params.extend([V::Integer(d), V::Integer(d), V::Integer(id)]);
    }
    w.join(" AND ")
}

/// Newest first. Asks for more than `limit` so the same mail filed twice (INBOX and All Mail)
/// can be folded into one row without leaving the page short. Returns the rows and whether the
/// database had more beyond what was fetched.
pub fn query(conn: &Connection, f: &Filter, limit: usize, with_terms: bool) -> rusqlite::Result<(Vec<Row>, bool)> {
    let mut params = Vec::new();
    let wh = where_clause(f, with_terms, &mut params);
    let fetch = limit + 1;
    let sql = format!("SELECT {ROW_COLS} {ROW_JOINS} WHERE {wh} ORDER BY m.date_received DESC, m.ROWID DESC LIMIT {fetch}");
    let mut st = conn.prepare(&sql)?;
    let rows: Vec<Row> = st.query_map(params_from_iter(params), row_from)?.collect::<Result<_, _>>()?;
    let more = rows.len() > limit;
    Ok((rows.into_iter().take(limit).collect(), more))
}

/// One page of rows with duplicates (one mail in several mailboxes) folded by global id. Pages
/// through the database until `limit` distinct rows are found or it runs out.
pub fn page(conn: &Connection, f: &Filter, limit: usize) -> rusqlite::Result<(Vec<Row>, bool)> {
    let mut f = f.clone();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    loop {
        let (rows, more) = query(conn, &f, limit * 2, true)?;
        let last = rows.last().map(|r| (r.date, r.rowid));
        let n = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            if seen.insert(r.global_id) {
                out.push(r);
                if out.len() == limit {
                    // Rows left in this batch (or beyond it) may be more mail; they may also be
                    // copies already shown, so the next page can come back shorter.
                    return Ok((out, more || i + 1 < n));
                }
            }
        }
        match (more, last) {
            (true, Some(l)) => f.before = Some(l),
            _ => return Ok((out, false)),
        }
    }
}

pub fn row_by_id(conn: &Connection, rowid: i64) -> rusqlite::Result<Option<Row>> {
    let sql = format!("SELECT {ROW_COLS} {ROW_JOINS} WHERE m.ROWID = ?");
    conn.query_row(&sql, [rowid], row_from).optional()
}

/// The mailboxes a message is in: its own, plus any labels on it.
pub fn message_mailboxes(conn: &Connection, rowid: i64, own: i64) -> Vec<i64> {
    let mut out = vec![own];
    if let Ok(mut st) = conn.prepare("SELECT mailbox_id FROM labels WHERE message_id = ?") {
        if let Ok(rows) = st.query_map([rowid], |r| r.get::<_, i64>(0)) {
            out.extend(rows.flatten());
        }
    }
    out
}

pub fn attachment_names(conn: &Connection, rowid: i64) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(mut st) = conn.prepare("SELECT COALESCE(name, '') FROM attachments WHERE message = ? ORDER BY ROWID") {
        if let Ok(rows) = st.query_map([rowid], |r| r.get::<_, String>(0)) {
            out.extend(rows.flatten().filter(|n| !n.is_empty()));
        }
    }
    out
}

pub fn thread_size(conn: &Connection, conversation_id: i64) -> i64 {
    if conversation_id == 0 { return 1; }
    conn.query_row(
        // `+deleted`: without it SQLite walks the index on `deleted` (every live message, ~0.1 s a
        // row on a real store) instead of the one on `conversation_id`.
        "SELECT COUNT(DISTINCT global_message_id) FROM messages WHERE conversation_id = ? AND +deleted = 0",
        [conversation_id],
        |r| r.get(0),
    )
    .unwrap_or(1)
}

/// Every message in a conversation, oldest first, one per global id.
pub fn thread_rows(conn: &Connection, conversation_id: i64, limit: usize) -> rusqlite::Result<Vec<Row>> {
    let sql = format!("SELECT {ROW_COLS} {ROW_JOINS} WHERE m.conversation_id = ? AND +m.deleted = 0 ORDER BY m.date_received ASC, m.ROWID ASC");
    let mut st = conn.prepare(&sql)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in st.query_map([conversation_id], row_from)?.flatten() {
        if seen.insert(r.global_id) {
            out.push(r);
            if out.len() >= limit { break; }
        }
    }
    Ok(out)
}

pub fn message_id_header(conn: &Connection, global_id: i64) -> Option<String> {
    conn.query_row("SELECT message_id_header FROM message_global_data WHERE ROWID = ?", [global_id], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// The newest message date in a scope: how current Mail's index is, so an empty answer from a
/// Mail that hasn't synced reads as stale rather than as "no such mail".
pub fn newest_date(conn: &Connection, scope: &Scope) -> Option<i64> {
    let mut params = Vec::new();
    let sql = format!("SELECT MAX(m.date_received) FROM messages m WHERE m.deleted = 0 AND {}", scope.sql(&mut params));
    conn.query_row(&sql, params_from_iter(params), |r| r.get::<_, Option<i64>>(0)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_urls_split_and_decode() {
        let (scheme, acct, path) = split_mailbox_url("imap://AAAA-BB/Logs/Sent%20Messages").unwrap();
        assert_eq!((scheme.as_str(), acct.as_str(), path.as_str()), ("imap", "AAAA-BB", "Logs/Sent Messages"));
        let (_, _, p) = split_mailbox_url("local://X/Recovered%20Messages%20(On%20My%20Mac)").unwrap();
        assert_eq!(p, "Recovered Messages (On My Mac)");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
    }

    #[test]
    fn kinds_by_leaf_name() {
        assert_eq!(mailbox_kind("INBOX", false), "inbox");
        assert_eq!(mailbox_kind("[Gmail]/Sent Mail", false), "sent");
        assert_eq!(mailbox_kind("Deleted Messages", false), "trash");
        assert_eq!(mailbox_kind("Junk E-mail", false), "junk");
        assert_eq!(mailbox_kind("Clients/Acme", true), "label");
        assert_eq!(mailbox_kind("Clients/Acme", false), "folder");
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like("50%_off\\"), "%50\\%\\_off\\\\%");
    }

    /// A tiny database in Mail's shape: labels, duplicates across mailboxes, paging.
    fn fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, url TEXT, total_count INTEGER, unread_count INTEGER, source INTEGER);
             CREATE TABLE messages (ROWID INTEGER PRIMARY KEY, global_message_id INTEGER, sender INTEGER, subject_prefix TEXT, subject INTEGER, summary INTEGER, date_received INTEGER, mailbox INTEGER, read INTEGER, flagged INTEGER, deleted INTEGER, conversation_id INTEGER, size INTEGER);
             CREATE TABLE addresses (ROWID INTEGER PRIMARY KEY, address TEXT, comment TEXT);
             CREATE TABLE subjects (ROWID INTEGER PRIMARY KEY, subject TEXT);
             CREATE TABLE summaries (ROWID INTEGER PRIMARY KEY, summary TEXT);
             CREATE TABLE recipients (ROWID INTEGER PRIMARY KEY, message INTEGER, address INTEGER, type INTEGER);
             CREATE TABLE attachments (ROWID INTEGER PRIMARY KEY, message INTEGER, name TEXT);
             CREATE TABLE labels (message_id INTEGER, mailbox_id INTEGER);
             CREATE TABLE message_global_data (ROWID INTEGER PRIMARY KEY, message_id_header TEXT);
             INSERT INTO mailboxes VALUES (1,'imap://A/INBOX',0,0,NULL),(2,'imap://A/Archive',0,0,NULL),(3,'imap://B/%5BGmail%5D/All%20Mail',0,0,NULL),(4,'imap://B/INBOX',0,0,3),(5,'imap://A/Junk',0,0,NULL);
             INSERT INTO addresses VALUES (1,'billing@acme.test','Acme Billing'),(2,'pat@example.test','Pat'),(3,'me@example.test','');
             INSERT INTO subjects VALUES (1,'Invoice 42'),(2,'Lunch?'),(3,'Win a prize');
             INSERT INTO summaries VALUES (1,'Your invoice is attached'),(2,'Are you free Friday'),(3,'Click here');
             INSERT INTO messages VALUES
               (10,100,1,'',1,1,1000,1,0,0,0,7,10),
               (11,100,1,'',1,1,1000,2,0,0,0,7,10),
               (12,101,2,'Re: ',2,2,2000,3,1,1,0,8,10),
               (13,102,2,'',3,3,3000,5,0,0,0,9,10),
               (14,103,2,'',2,2,1500,1,1,0,1,8,10);
             INSERT INTO labels VALUES (12,4);
             INSERT INTO recipients VALUES (1,10,3,0),(2,12,3,1);
             INSERT INTO attachments VALUES (1,10,'invoice-42.pdf');
             INSERT INTO message_global_data VALUES (100,'<abc@acme.test>');",
        )
        .unwrap();
        c
    }

    fn scope_of(c: &Connection, ids: &[i64]) -> Scope {
        let boxes = load_mailboxes(c).unwrap();
        Scope::from_mailboxes(boxes.iter().filter(|b| ids.contains(&b.id)))
    }

    #[test]
    fn a_label_mailbox_finds_its_messages_through_labels() {
        let c = fixture();
        let f = Filter { scope: scope_of(&c, &[4]), ..Default::default() };
        let (rows, _) = page(&c, &f, 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.rowid).collect::<Vec<_>>(), vec![12]);
        assert_eq!(rows[0].subject, "Re: Lunch?");
    }

    #[test]
    fn one_mail_in_two_mailboxes_is_one_row_and_deleted_is_hidden() {
        let c = fixture();
        let f = Filter { scope: scope_of(&c, &[1, 2, 3, 4]), ..Default::default() };
        let (rows, more) = page(&c, &f, 10).unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.global_id).collect();
        assert_eq!(ids, vec![101, 100]);
        assert!(!more);
    }

    #[test]
    fn filters_match_terms_sender_recipient_and_attachments() {
        let c = fixture();
        let all = scope_of(&c, &[1, 2, 3, 4, 5]);
        let q = |f: Filter| page(&c, &Filter { scope: all.clone(), ..f }, 10).unwrap().0.iter().map(|r| r.global_id).collect::<Vec<_>>();
        assert_eq!(q(Filter { terms: vec!["invoice".into()], ..Default::default() }), vec![100]);
        assert_eq!(q(Filter { terms: vec!["friday".into(), "pat".into()], ..Default::default() }), vec![101]);
        assert_eq!(q(Filter { from: Some("acme".into()), ..Default::default() }), vec![100]);
        assert_eq!(q(Filter { to: Some("me@example".into()), ..Default::default() }), vec![101, 100]);
        assert_eq!(q(Filter { attachment_name: Some(".pdf".into()), ..Default::default() }), vec![100]);
        assert_eq!(q(Filter { unread_only: true, ..Default::default() }), vec![102, 100]);
        assert_eq!(q(Filter { since: Some(1500), until: Some(2500), ..Default::default() }), vec![101]);
    }

    #[test]
    fn paging_walks_strictly_older() {
        let c = fixture();
        let f = Filter { scope: scope_of(&c, &[1, 2, 3, 4, 5]), ..Default::default() };
        let (first, more) = page(&c, &f, 1).unwrap();
        assert!(more);
        assert_eq!(first[0].global_id, 102);
        let (second, _) = page(&c, &Filter { before: Some((first[0].date, first[0].rowid)), ..f }, 1).unwrap();
        assert_eq!(second[0].global_id, 101);
    }

    #[test]
    fn threads_fold_duplicates_oldest_first() {
        let c = fixture();
        let rows = thread_rows(&c, 7, 50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(thread_size(&c, 8), 1);
        assert_eq!(message_id_header(&c, 100).as_deref(), Some("<abc@acme.test>"));
    }
}
