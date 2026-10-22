---
title: Chrome Extension
description: Link one tab in your own Chrome to one maiTerm agent, and it can diagnose and drive that tab — console errors, failing requests and their responses, the page itself, clicks and typing — signed in as you.
---

An agent fixing a bug in a web app usually can't see the bug. It reads the code, guesses, and asks you to reload the page and tell it what happened. Tools that give an agent a browser give it a *new* one: no logins, none of your data, often not the state the bug needs.

The **maiTerm extension for Chrome** lets an agent work in **your** browser instead, in one tab you choose. Link a tab to an agent tab in maiTerm, and that agent can:

**Diagnose it**

- read the console: errors and warnings, uncaught exceptions with the file and line they came from, and the browser's own warnings (a blocked script, a failed load);
- read the network: which requests failed, their status and timing, and the body an API returned, which is usually where a broken call says why;
- read the page as an accessibility tree: headings, text, links, buttons and form fields with their values.

**Drive it**

- click, type, press keys, and go to an address, back, forward or reload;
- take a screenshot;
- run JavaScript in the page.

All of it in your real, signed-in session, and in the background: the tab doesn't have to be the one you're looking at, so you can keep working in another while the agent works in its own.

<figure>
  <img src="/screenshots/chrome-linked.webp" alt="A web app showing a failed request, with the maiTerm extension's popup open beside it: linked to the agent tab web-ui" />
  <figcaption>A linked tab. The agent reads the failing request and works the page.</figcaption>
</figure>

## You stay in control

- **Only you link a tab**, from the extension's icon in the browser. No agent can link one, pick one, or see your other tabs.
- **One tab, one agent.** Each agent can use only the tab linked to it, and a tab is linked to one agent at a time.
- **Chrome shows that it's happening.** While an agent is working, Chrome shows its "is debugging this browser" bar. **Cancel** it, close the tab, click **Unlink**, or turn the feature off in maiTerm, and the link ends.
- **Nothing leaves your computer through the extension.** It talks only to maiTerm on your own machine. What the agent reads from the tab goes to that agent, as everything else it reads does. See the [privacy policy](/chrome-extension/privacy/).

## Setting it up

The extension needs the maiTerm app; on its own it does nothing.

:::note
The Chrome extension arrives in the maiTerm release after 3.10.0.
:::

1. In maiTerm, open **Preferences → Chrome extension** and turn on **Let agents diagnose and drive linked browser tabs**. This gives agents the browser tools and lets the extension find maiTerm.
2. Install the extension. Until its Chrome Web Store listing is live, click **Load unpacked…** there: Chrome opens on its extensions page with the folder shown in Finder. Turn on **Developer mode**, click **Load unpacked**, and choose that folder. You do this once per browser.

It works in Chrome and other Chromium browsers: Brave, Edge and Arc. Within 30 seconds the browser appears under **Connected browsers**.

## Linking a tab

<figure>
  <img src="/screenshots/chrome-link.webp" alt="The maiTerm extension's popup listing agent tabs to link the current page to" />
  <figcaption>Choosing the agent tab that may use this page.</figcaption>
</figure>

1. Open the page in your browser.
2. Click the maiTerm icon in the toolbar. The popup lists your agent tabs in maiTerm, grouped by workspace, with a filter.
3. Choose one. The page is now that agent's to use, and the extension's icon shows on the tab that it's linked.

Tell the agent what to look at: "the orders page won't load, find out why" is enough for it to read the failing request and its response. To end it, click the icon again and **Unlink**, or click **Unlink** beside it in **Preferences → Chrome extension → Linked tabs**, which lists every link.

A link follows the tab: navigating, or reloading the page, keeps it. Reloading the *agent's* tab in maiTerm keeps it too. Links don't outlive the browser: after a restart of Chrome, link the tab again.

## The tools

Agents in an agent tab get these while the feature is on. Every one acts on the caller's own linked tab; an agent with no linked tab is told to ask you to link one.

| Tool | What it does |
|------|-------------|
| `browserStatus` | Whether this agent has a linked tab, and its title and address |
| `browserConsole` | Console messages, uncaught exceptions with their source, and the browser's warnings since the link — or only errors |
| `browserNetwork` | Requests since the link: method, address, status, failure, size and time; failed ones only, or one request's response body |
| `browserSnapshot` | The page as an accessibility tree, each element it can act on numbered for the tools below |
| `browserClick` | Click an element from the snapshot, with real mouse events |
| `browserType` | Type into an element, or the focused one |
| `browserPressKey` | Press one key: Enter, Tab, Escape, arrows and so on |
| `browserNavigate` | Go to an address, or back, forward or reload, and wait for the page |
| `browserScreenshot` | What the tab shows, as an image |
| `browserEval` | Run a JavaScript expression in the page and return its value |

Console and network records start when the agent first uses the tab, so for a bug that happens on load, the agent reloads the page first. The extension keeps the last 200 console messages and 300 requests per linked tab, in memory, and drops them when the link ends.

## Worth knowing

- **It acts as you.** Clicking "Delete" in a linked tab deletes, in your account. Link a tab you're happy for the agent to work in, and say what it may and may not do.
- **Agent tabs only.** The popup lists maiTerm tabs running an agent, not shells or stack services.
- **One tab per agent.** Linking a second tab to the same agent moves the link; linking a tab already linked to another agent takes it from that one.
