---
title: Privacy Policy — maiTerm for Chrome
description: What the maiTerm Chrome extension reads, where it goes, and what it keeps.
---

Last updated: 21 October 2026

## The short version

The extension sends nothing to Flexmark, to us, or to any server of ours. It talks to one
place only: the maiTerm app running on your own computer (`127.0.0.1`). It has no analytics, no
telemetry and no account.

What it reads from a tab you linked goes to that maiTerm app, and from there to the AI agent you
linked the tab to. That agent may send it on to its AI provider, for example Anthropic or
OpenAI, as part of its conversation. That provider's policy governs what happens to it there.

## Which tabs it touches

Only tabs you link yourself. You click the extension's icon on a tab and choose one maiTerm
agent tab, and that is the only way a link is made. An agent cannot link a tab, choose a tab or
reach any other tab.

The extension uses Chrome's debugger (the DevTools Protocol) on a linked tab, and Chrome shows
its "is debugging this browser" bar while it does. Cancelling that bar, closing the tab, clicking
**Unlink**, or turning the feature off in maiTerm ends the link.

It does read the title and address of your open tabs, because it needs them to mark linked
tabs and to tell maiTerm which page a link is on. Those never leave your computer except as
part of a linked tab's information sent to your local maiTerm.

## What it reads from a linked tab

Only when the agent linked to that tab asks for it:

- the page's accessibility tree: its headings, text, links, buttons and form fields, with their
  values;
- console messages and errors, and the browser's own warnings;
- network requests (addresses, methods, status codes, timings) and, when asked for one request,
  that response's body;
- a screenshot;
- the result of JavaScript the agent runs in the page.

It can also click, type, press keys and navigate in that tab, in your logged-in session.

## What it stores

In `chrome.storage.session`, which Chrome clears when the browser closes:

- your links: the Chrome tab's id, title and address, and the maiTerm tab's id and name.

Console and network logs are kept in memory only while a tab is linked (the last 200 console
messages and 300 requests), and are discarded when the link ends.

Nothing is kept in `chrome.storage.local` or synced storage.

## How it finds maiTerm

Through Chrome native messaging: Chrome runs maiTerm's own program, which answers with the port
maiTerm is listening on and a secret that changes each time maiTerm starts. The extension then
connects to `ws://127.0.0.1:<port>`. maiTerm refuses any other extension and any wrong secret.

## Contact

Questions: open an issue at https://github.com/Flexmark-Intl/maiterm/issues
