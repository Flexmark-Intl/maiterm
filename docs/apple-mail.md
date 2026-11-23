# Apple Mail for agents

**Status: DESIGN, 2026-11-23. Nothing built.** Probe results in §2 were measured on a real
Mail store (V10, 13 accounts, ~706k messages) the same day.

An agent is told "find the invoice Acme sent last week" or "read what support@ wrote about the
outage". Today it has no way to do that short of asking the human to paste the mail. This gives
agents **read-only** access to the mail the human already has in Apple Mail, through maiTerm MCP
tools: mailboxes by account, recent mail, search, read a message or a whole thread, and save an
attachment to a file.

## 1. Rules

1. **Read-only.** No send, reply, move, flag, mark-read or delete. Reading a message through
   maiTerm does not mark it read in Mail (we never write to Mail's store or drive Mail.app).
2. **Only the human turns it on.** `mail_enabled` defaults off and is not in `preference_meta`,
   so `setPreference` cannot flip it — same as the accounts and watch-script waiver preferences.
   The tools are listed only while it is on (as browser link's are).
3. **Mail text is untrusted input.** Every subject, sender name, preview and body that reaches an
   agent is framed as other people's words — context for what the human asked, never instructions
   (`readCommsChannel`'s wording, and the Hive's `frame_escape` for the body delimiters). Email is
   the most obvious injection path into an agent with a shell.
4. **An empty answer is never "no such mail".** Every list/search answer says what it covered:
   which accounts and mailboxes, the date window, whether Junk/Trash were included, how many
   bodies were scanned versus skipped, and the date of the newest message in Mail's index (so a
   Mail.app that hasn't synced shows up as stale, not as absence).
5. **Nothing from a real mailbox in the repo.** No account names, addresses or UUIDs in docs,
   fixtures or tests. Tests against the real store are `#[ignore]`d, like `hive::probe`.
6. **macOS only.** The module is `cfg(target_os = "macos")`; elsewhere the tools are never listed.

## 2. Where the data is (probed)

| What | Where | Notes |
|---|---|---|
| Message index | `~/Library/Mail/V10/MailData/Envelope Index` (SQLite, WAL) | Mail writes it live. Open `mode=ro` per call, never `immutable`, short-lived, in `spawn_blocking`. |
| Account names | `~/Library/Accounts/Accounts4.sqlite`, `ZACCOUNT.ZIDENTIFIER` = the UUID in a mailbox URL | Some accounts have no `ZACCOUNTDESCRIPTION` and take it from their parent account (`ZPARENTACCOUNT`); fall back to `ZUSERNAME`, then "Account <first 4 of uuid>". |
| Message bodies | `~/Library/Mail/V10/<account uuid>/<a>.mbox/<b>.mbox/<store uuid>/Data/<d…>/Messages/<ROWID>.emlx` | `<d…>` = digits of `ROWID / 1000` **reversed**, one folder each (6758457 → `8/5/7/6`). Nested mailboxes nest `.mbox` folders (`Logs/Billing` → `Logs.mbox/Billing.mbox`). Find the store uuid by listing, don't assume one. |
| Partly downloaded | `<ROWID>.partial.emlx` (~20% of one inbox) | Text parts are inline; attachments live in `…/Attachments/<ROWID>/<part>/<filename>`, which may be absent if Mail never fetched them. |
| `.emlx` format | first line = byte count N, then N bytes of RFC 822, then an XML plist (flags, dates) | Bodies are MIME: base64/QP must be decoded, so a body search parses, it doesn't grep. |

Key tables: `messages` (`mailbox`, `sender`→`addresses`, `subject`→`subjects`, `summary`→`summaries`,
`date_received` unix secs, `read`, `flagged`, `deleted`, `conversation_id`, `global_message_id`,
`size`), `mailboxes` (`url` = `imap://<account uuid>/<url-encoded path>` or `local://…`, counts),
`recipients` (`type` 0 = To, 1 = Cc, 2 = Bcc), `attachments` (`message`, `name`),
`message_global_data.message_id_header` (the RFC Message-ID).

**Gmail-style labels.** 58 mailboxes have `source` set (they are labels over an All Mail
mailbox); their messages are in `labels(message_id, mailbox_id)`, NOT `messages.mailbox`. A query
for "mailbox X" is `messages.mailbox = X OR messages.ROWID IN (SELECT message_id FROM labels
WHERE mailbox_id = X)`. Results are de-duplicated by `global_message_id` (the same mail in All
Mail and INBOX is one result).

**Previews.** `summaries` holds Mail's own text preview for nearly every message (299 of 301 in a
30-day inbox sample). It is the list preview and a cheap first body search.

**Spotlight is no help**: `mdfind` finds 2 `.emlx` items on a machine with 706k messages. Mail
keeps its body index private. Full body search is ours.

**Measured costs (unscoped, all 706k messages):** subject `LIKE` 0.35 s · sender address/name
0.28 s · preview text 0.47 s · recipient address 1.1 s (an index on `recipients(address)` exists).
A newest-first page of one mailbox: 10 ms.

**The schema is private and changes between macOS releases.** On open, check that the columns we
use exist; if not, every tool answers with one sentence naming the missing column and the macOS
version, never a SQL error.

## 3. Access: Full Disk Access

Both files are behind macOS privacy protection. The tools run inside maiTerm (the MCP server is
the app), so it is **maiTerm** that needs Full Disk Access, whatever the agent's own shell has.
`check_full_disk_access` (commands/system.rs) already exists. When it's missing:
- the tools answer `{"error":"no_full_disk_access", …}` with a sentence an agent can relay;
- the Preferences row shows the state and an **Open Privacy Settings** button (the deep link to
  the Full Disk Access pane) — never a "go do X and restart" instruction.

## 4. Tools

All read-only, all take optional `account` (name, email or id from `listMailboxes`) and
`mailbox` (path like `INBOX` or `Logs/Billing`, or id). Junk, Trash, Deleted and Drafts are
excluded unless named or `include_junk_trash: true`. Every message is identified by an opaque
`id` (the index ROWID as a string) that the other tools take back.

| Tool | Does |
|---|---|
| `listMailboxes` | Accounts, each with its mailboxes as a tree (path, total, unread, kind: inbox/sent/drafts/junk/trash/archive/label/folder). Plus `index_newest` (date of the newest indexed message) and `full_disk_access`. |
| `getRecentMail` | Newest first across all inboxes (default), one account, or one mailbox. `since` (ISO date or `"24h"`/`"7d"`), `unread_only`, `flagged_only`, `limit` (default 25, max 100), cursor `before`. Rows: id, account, mailbox, date, from (name + address), to/cc count, subject, preview (≤200 chars), read, flagged, attachment names, thread size. |
| `searchMail` | `query` matches subject, sender and preview text; structured filters `from`, `to` (to/cc/bcc), `subject`, `has_attachment`, `attachment_name`, `since`/`until`, `unread_only`, `flagged_only`. `body: true` also decodes and scans full bodies of the candidates that survive the other filters, bounded by `max_scan` (default 2000 messages, newest first) — and says how many were scanned, so "not found in the 2000 newest" is never read as "not found". Same row shape and paging as `getRecentMail`. |
| `readMail` | One message: headers (from, to, cc, reply-to, date, Message-ID, list-id), the text body (`text/plain`, else HTML converted to text; `format: "html"` for the raw HTML), attachments (name, type, size, downloaded or not), and `open_url` (`message://<Message-ID>`, which opens it in Mail for the human). Long bodies page with `offset`/`max_chars` (default 20 000). Quoted reply history is trimmed by default (`full: true` keeps it). |
| `readMailThread` | Every message in the conversation (`conversation_id`), oldest first, de-duplicated, each with quoted history trimmed — the cheap way to read a back-and-forth. |
| `saveMailAttachment` | Copies one attachment (by message id + name or index) to a temp file and returns the path, so the agent can open a PDF or image with its own Read. An attachment Mail never downloaded answers `not_downloaded` with `open_url`. |

## 5. Who gets the tools

- **Normal agent tabs**: yes while `mail_enabled`.
- **Accounts**: `mail_accounts` — **decision pending**: all accounts, or only the ones the human
  ticks.
- **SSH tabs**: an agent on another host reading this Mac's mail — **decision pending**.
- **Hive workers**: never. They're sandboxed and fed untrusted input; a worker that needs mail
  asks its coordinator, which is a normal tab.
- **Attachments**: `saveMailAttachment` in v1 or later — **decision pending**.

## 6. Code layout (planned)

- `src-tauri/src/mail/mod.rs` — open + schema check, account resolution (Accounts4 with the
  parent fallback), mailbox tree, filters including labels.
- `mail/emlx.rs` — path derivation, `.emlx` framing, MIME via a parser crate (`mail-parser`),
  HTML → text, quote trimming, partial-attachment lookup.
- `mail/search.rs` — SQL search + the bounded body scan.
- `claude_code/protocol.rs` + `server.rs` — the six tools, listed only when enabled and macOS.
- Preferences: a Mail section (enable toggle, FDA status + button, account ticks).
- Tests: framing, path derivation, MIME decode, quote trimming on synthetic fixtures; the
  real-store probe `#[ignore]`d.

## 7. Not in v1

Sending or replying (it would be an agent-request card, `docs/agent-requests.md`, if ever);
moving, flagging or marking read; Outlook/other clients; mail on an SSH host; notifications on new
mail (a watch script over `getRecentMail` covers "tell me when X arrives").
