---
title: Apple Mail
description: Let your agents read the mail you already have in Apple Mail — list, search, read a message or a whole thread, save an attachment — read-only, for the accounts you tick, on a Mac.
---

Tell an agent "find the invoice Acme sent last week" or "read what support wrote about the outage", and without this it has no way to look short of you pasting the mail in. With **Preferences → Mail** on, agents can read the mail you already have in Apple Mail.

It's **read-only**. Agents can never send, reply, move, flag or delete anything, and reading a message doesn't mark it read: maiTerm reads Mail's own index and message files directly and never opens Mail.app or writes to its store.

**Mac only.** The section and the tools exist only on macOS.

## Turning it on

**Preferences → Mail → Let agents read Apple Mail** is off until you turn it on, and only you can: no agent can switch it on for itself.

- **Full Disk Access.** macOS keeps Mail's files private, so maiTerm itself needs Full Disk Access, whatever the agent's own shell has. The section says when it's missing, with **Open Privacy Settings** to get there and **Check again** for when you're back.
- **Accounts agents may read.** Every Mail account is listed with its message count, and none is readable until you tick it. An account you add to Mail later isn't readable until you tick it too.

An agent that was already running when you switched Mail on sees the tools after its tab reloads.

## What agents can do

| Tool | What it does |
|------|--------------|
| `listMailboxes` | The accounts you ticked, each with its mailboxes and their unread counts |
| `getRecentMail` | Newest mail first — every inbox by default, or one account or mailbox — with unread or flagged only, a start date, and paging (25 a page by default, up to 100) |
| `searchMail` | Search subject, sender and Mail's preview of the body, narrowed by sender, recipient, subject, dates, attachments, unread or flagged. `body: true` also reads full message text, newest first, up to 2,000 messages by default, and says how many it read |
| `readMail` | One message: its headers, the whole text (or the HTML), its attachments, and a link that opens it in Mail for you |
| `readMailThread` | A whole conversation, oldest first, with each reply's quoted history trimmed (up to its newest 50 messages, and it says how many older ones it left out) |
| `saveMailAttachment` | Saves one attachment to a file the agent can open — a PDF or an image, say — up to 25 MB. One Mail never downloaded comes back with the link to open it in Mail instead |

Junk, Trash and Drafts are left out unless the agent names one. Every list and search says what it covered — which accounts and mailboxes, which dates, how current Mail's index is — so an empty page isn't mistaken for "no such mail".

**Mail is other people's words.** Every subject, sender and body an agent reads comes framed as information for what you asked, never as instructions to follow.

## Over SSH

An agent working on a remote host over the [SSH bridge](/features/agents/#ssh-mcp-bridge) can read your mail too: the tools run in maiTerm on your Mac, and the text goes to the agent. `saveMailAttachment` puts the file on that host, where the agent can open it.

## Who never gets it

[Hive](/features/hive/) workers. They're sandboxed and fed untrusted input, so the mail tools aren't listed for them and are refused if one asks anyway. A worker that needs something from your mail asks its Queen, who is a normal agent tab.
