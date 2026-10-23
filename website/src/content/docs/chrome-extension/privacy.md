---
title: Privacy Policy — maiTerm for Chrome
description: What the maiTerm Chrome extension reads, where it goes, and what it keeps.
---

Last updated: 22 October 2026

## The short version

The extension sends nothing to Flexmark, to us, or to any server of ours. It talks to one
place only: the maiTerm app running on your own computer (`127.0.0.1`). It has no analytics, no
telemetry and no account.

What it reads from a tab you linked goes to that maiTerm app, and from there to the AI agent you
linked the tab to. That agent may send it on to its AI provider, for example Anthropic or
OpenAI, as part of its conversation. That provider's policy governs what happens to it there.

## Which tabs it touches

Only tabs you link yourself, and the tabs those pages open for the agent:

- **You link a tab** by clicking the extension's icon on it and choosing one maiTerm agent tab.
  One agent can hold several linked tabs; each tab belongs to one agent.
- **A tab a linked page opens joins that page's agent**: a popup window it opens (a sign-in
  window, for example), or a tab it opens within a few seconds of the agent acting on it. A tab
  you open yourself from a linked page, such as with a middle-click, is not linked.

An agent cannot link a tab or choose one, and reaches only the tabs linked to it.

The extension uses Chrome's debugger (the DevTools Protocol) on linked tabs only. While it does,
Chrome shows its "started debugging this browser" bar, in every tab of every window: the bar is
about the whole browser, and doesn't mean the extension can reach the other tabs. Cancelling that
bar ends every link. Closing a tab, clicking **Unlink**, or turning the feature off in maiTerm
ends that tab's link (or all of them, for the last).

It does read the title and address of your open tabs, and which tab opened which, because it
needs them to mark linked tabs, to tell maiTerm which page a link is on, and to know when a linked
page opened a new tab. Those never leave your computer except as
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

- your links: the Chrome tab's id, title and address, the maiTerm tab's id and name, the short
  handle the agent knows the tab by (`b1`, `b2`…), which linked tab opened it, and where the tab
  sat before it joined the agent's tab group, so it can go back there;
- the highest handle each maiTerm tab has used, so a closed tab's handle is never given to a new
  page.

Console and network logs are kept in memory only while a tab is linked (the last 200 console
messages and 300 requests), and are discarded when the link ends.

Nothing is kept in `chrome.storage.local` or synced storage.

## How it finds maiTerm

Through Chrome native messaging: Chrome runs maiTerm's own program, which answers with the port
maiTerm is listening on and a secret that changes each time maiTerm starts. The extension then
connects to `ws://127.0.0.1:<port>`. maiTerm refuses any other extension and any wrong secret.

## Contact

Questions: open an issue at https://github.com/Flexmark-Intl/maiterm/issues
