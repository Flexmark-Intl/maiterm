# Apple Mail for agents

**Status: BUILT and DEPLOYED 2026-11-23, all six tools tried live** (`src-tauri/src/mail/`,
Preferences → Mail, `components/mail/MailPane.svelte`). The live check went through the
installed build's MCP endpoint with one account ticked:
- every call answered in about 0.1 s;
- paging with `before` worked, and a body search worked;
- `readMail`, `readMailThread` and `saveMailAttachment` worked, saving attachments by index and by name, and the files were valid;
- an unticked account was refused with `unknown_account`.

maiTerm's own MCP server never sends `tools/list_changed`, so an agent that was already
running when Mail was switched on doesn't see the tools until its tab reloads. The same is
true of every other preference-gated tool set. Probe results in §2 were measured on a real Mail store (V10,
13 accounts, ~706k messages) the same day. The `#[ignore]`d `mail::tests::real_store` re-runs
every tool against this Mac's store and prints counts and timings only: 25 recent rows in
~20 ms, an indexed search in ~30 ms, a body search over 128 messages in ~0.1 s, 40/40
messages read with a body, 15/15 attachments saved.

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
   (`readCommsChannel`'s wording, as a `note` beside the JSON answer; mail text is only ever a JSON
   string value, never spliced into maiTerm's own sentences). Email is the most obvious injection
   path into an agent with a shell. **A sender's attachment filename is never joined onto a path**:
   a partial message's attachment is found by comparing it with what `read_dir` lists
   (`emlx::partial_attachment`), since `/abs/path` or `../` in a name would otherwise read any file
   maiTerm (with Full Disk Access) can.
4. **An empty answer is never "no such mail".** Every list/search answer says what it covered:
   which accounts and mailboxes, the date window, whether Junk/Trash were included, how many
   bodies were scanned versus skipped, and the date of the newest message in Mail's index (so a
   Mail.app that hasn't synced shows up as stale, not as absence).
5. **Nothing from a real mailbox in the repo.** No account names, addresses or UUIDs in docs,
   fixtures or tests. Tests against the real store are `#[ignore]`d, like `hive::probe`.
6. **macOS only.** The module compiles everywhere (it is plain Rust, so its tests run on the
   Windows CI job too), but `mail::tools_on` is false off macOS, so the tools are never listed or
   answered there, and the Preferences section is shown only on a Mac.

## 2. Where the data is (probed)

| What | Where | Notes |
|---|---|---|
| Message index | `~/Library/Mail/V10/MailData/Envelope Index` (SQLite, WAL) | Mail writes it live. Open `mode=ro` per call, never `immutable`, short-lived, in `spawn_blocking`. |
| Account names | `~/Library/Accounts/Accounts4.sqlite`, `ZACCOUNT.ZIDENTIFIER` = the UUID in a mailbox URL | Some accounts have no `ZACCOUNTDESCRIPTION` and take it from their parent account (`ZPARENTACCOUNT`); fall back to `ZUSERNAME`, then "Account <first 4 of uuid>". |
| Message bodies | `~/Library/Mail/V10/<account uuid>/<a>.mbox/<b>.mbox/<store uuid>/Data/<d…>/Messages/<ROWID>.emlx` | `<d…>` = digits of `ROWID / 1000` **reversed**, one folder each (6758457 → `8/5/7/6`). Nested mailboxes nest `.mbox` folders (`Logs/Billing` → `Logs.mbox/Billing.mbox`). Find the store uuid by listing, don't assume one. |
| Partly downloaded | `<ROWID>.partial.emlx` (~20% of one inbox) | Text parts are inline; attachments live in `…/Attachments/<ROWID>/<section>/<filename>`, which may be absent if Mail never fetched them. `<section>` is the part's MIME (IMAP) section number: measured over 866 partial messages, 3001 of 3015 folders are a section of their message and 3005 hold exactly one file. So an attachment is found by its computed section, never by its filename (which is the sender's). |
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
Mail and INBOX is one result): a mail is shown as its first copy in sort order that passes the
filter (`store::is_first_copy`, checked against the database), so it can't come back on a later
page. A per-call "seen" set did let it come back, once the cursor had passed its first copy.

**Previews.** `summaries` holds Mail's own text preview for nearly every message (299 of 301 in a
30-day inbox sample). It is the list preview and a cheap first body search.

**Spotlight is no help**: `mdfind` finds 2 `.emlx` items on a machine with 706k messages. Mail
keeps its body index private. Full body search is ours.

**Measured costs (unscoped, all 706k messages):** subject `LIKE` 0.35 s · sender address/name
0.28 s · preview text 0.47 s · recipient address 1.1 s (an index on `recipients(address)` exists).
A newest-first page of one mailbox: 10 ms. Newest-first queries walk the date index and stop at
the page, so a scoped search is far cheaper than these unscoped counts.

**Query-plan trap:** `WHERE conversation_id = ? AND deleted = 0` makes SQLite walk the index on
`deleted` (every live message, ~0.1 s per call), which made a 25-row page take 5.7 s while it
counted each row's thread. `+deleted` steers it to the conversation index (8 ms). Any new
per-row query on `messages` should be checked with `EXPLAIN QUERY PLAN` against a real store.

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
| `readMail` | One message: headers (from, to, cc, reply-to, date, Message-ID, list-id), the text body (`text/plain`, else HTML converted to text; `format: "html"` for the raw HTML), attachments (name, type, size, downloaded or not), and `open_url` (`message://<Message-ID>`, which opens it in Mail for the human). Long bodies page with `offset`/`max_chars` (default 20 000). The body is the WHOLE text by default; `trim_quotes: true` opts in to cutting a reply's trailing quote. The body is every text part, so text after an inline image is kept. |
| `readMailThread` | The NEWEST 50 messages of the conversation (`conversation_id`), shown oldest first, de-duplicated, each with a reply's trailing quoted history trimmed (flagged where it was; the earlier messages are right there) — the cheap way to read a back-and-forth. A longer thread says how many older ones it left out (`older_not_shown`). |

**Quote trimming (`emlx::trim_quoted`) cuts only on two pieces of positive evidence**: a trailing
block the client marked as quoted (`>`), AND a reply attribution ("… wrote:") above it or as its
first quoted line (Apple Mail / iOS). Three looser versions each lost text in review:
wording-based cuts dropped bottom-posted answers, then missed Apple Mail's own reply format, and
`>` alone cut Apple Mail FORWARDS, whose whole content is `>`-quoted under "Begin forwarded
message:". Interleaved and bottom-posted replies end unquoted and are never cut; Outlook-style and
HTML-converted history isn't `>`-quoted and comes back whole. A missed trim costs space, a wrong
one loses the message — which is why `readMail` only trims when asked.
| `saveMailAttachment` | Copies one attachment (by message id + name or index) to a temp file and returns the path, so the agent can open a PDF or image with its own Read. An attachment Mail never downloaded answers `not_downloaded` with `open_url`. |

## 5. Who gets the tools

Decided by Darryl, 2026-11-23:
- **Normal agent tabs**: yes while `mail_enabled`.
- **Accounts**: only the ones the human ticks (`mail_accounts`, account UUIDs). None is readable
  until ticked, and an account added to Mail later is not readable until ticked. Every query is
  limited to the ticked accounts' mailboxes, and a message id from another account answers "no
  such message".
- **SSH tabs**: yes. The tools run in maiTerm on the Mac; the text lands on the host in the
  agent's transcript, which the human accepted. `saveMailAttachment` writes to the host's `/tmp`
  over the bridge tunnel (the comms tools' `staging_target_for_tab` + `push_bytes_remote`), and
  answers `no_bridge` for an SSH tab without one.
- **Hive workers**: never — not listed (`ToolGates.mail` with a worker role), denied in the
  worker's settings and refused by Rust on the connection's own identity
  (`WORKER_EXTRA_DENIED_MCP_TOOLS` → `refuse_agent_tool`), so naming another tab's `tabId` doesn't
  get a worker in. They're sandboxed and fed untrusted input; a worker that needs mail asks its
  coordinator, which is a normal tab.
- **`saveMailAttachment` acts only for the caller** (`OWN_TAB_ONLY_TOOLS` + `PEER_ADDRESSING_TOOLS`):
  where the file lands depends on the tab, so neither a named `tabId` nor a guessed identity may
  choose another tab's host.
- **Attachments**: in v1 (`saveMailAttachment`, 25 MB cap, filename sanitized).

## 6. Code layout

- `src-tauri/src/mail/store.rs` — open + schema check, accounts (Accounts4 with the parent
  fallback), mailboxes and their kinds, the label-aware scope, filters, paging (newest first,
  cursor `date:rowid`, duplicates folded by `global_message_id`), threads.
- `mail/emlx.rs` — path derivation, `.emlx` framing, MIME via `mail-parser` 0.11 (1.x needs a
  newer Rust), HTML → text, quote trimming, partial-attachment lookup.
- `mail/mod.rs` — the six tool definitions and handlers, the access gates, coverage reporting,
  the bounded body scan, attachment staging, and `mail_list_accounts` for Preferences.
- `claude_code/protocol.rs` (`ToolGates.mail`) + `server.rs` (dispatch) — listed only when
  `mail_enabled` on macOS and never to a Hive worker.
- Preferences → Mail (`components/mail/MailPane.svelte`): the switch, Full Disk Access state with
  an Open Privacy Settings button, and the account ticks.
- Tests: fixtures for the SQL (labels, duplicates, paging, every filter), framing, MIME decode,
  partial attachments, quote trimming, dates; `mail::tests::real_store` `#[ignore]`d.

## 7. Not in v1

Sending or replying (it would be an agent-request card, `docs/agent-requests.md`, if ever);
moving, flagging or marking read; Outlook/other clients; mail on an SSH host; notifications on new
mail (a watch script over `getRecentMail` covers "tell me when X arrives").
