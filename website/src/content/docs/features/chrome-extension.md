---
title: Chrome Extension
description: Link tabs in your own Chrome to a maiTerm agent, and it can diagnose and drive them — console errors, failing requests and their responses, the page itself, clicks and typing — signed in as you.
---

An agent fixing a bug in a web app usually can't see the bug. It reads the code, guesses, and asks you to reload the page and tell it what happened. Tools that give an agent a browser give it a *new* one: no logins, none of your data, often not the state the bug needs.

The **maiTerm extension for Chrome** lets an agent work in **your** browser instead, in the tabs you choose. Link a tab to an agent tab in maiTerm, and that agent can:

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
- **Each tab has one agent.** An agent can use only the tabs linked to it, and a tab is linked to one agent at a time. The one tab you don't link by hand is one a linked page opens for its agent, such as a sign-in popup ([below](#several-tabs-and-popups)).
- **Chrome shows that it's happening.** While an agent is working, Chrome shows its "is debugging this browser" bar. **Cancel** it, close the tab, click **Unlink**, or turn the feature off in maiTerm, and the link ends.
- **Nothing leaves your computer through the extension.** It talks only to maiTerm on your own machine. What the agent reads from the tab goes to that agent, as everything else it reads does. See the [privacy policy](/chrome-extension/privacy/).

## Setting it up

The extension needs the maiTerm app; on its own it does nothing.

:::note
The Chrome extension needs maiTerm 3.11.0 or later. Its Chrome Web Store listing isn't live yet, so for now you load it from maiTerm as below.
:::

1. In maiTerm, open **Preferences → Chrome extension** and turn on **Let agents diagnose and drive linked browser tabs**. This gives agents the browser tools and lets the extension find maiTerm.
2. Install the extension, once per browser. The same section walks you through it, because Chrome only lets you add an extension that isn't from its store by hand:
   1. Click **Open in Chrome** to open Chrome's extensions page (`chrome://extensions`).
   2. Turn on **Developer mode**, the switch at the top right.
   3. Click **Load unpacked** and choose the folder maiTerm shows. **Copy path** and **Show in Finder** are beside it. The folder is inside your hidden Library: in Chrome's file picker, press ⌘⇧G, paste the path and press Return.
   4. Click the puzzle-piece icon in Chrome's toolbar and pin **maiTerm**. That icon is where you link a tab.

   maiTerm keeps that folder up to date, so the extension updates with maiTerm.

It works in Chrome and other Chromium browsers: Brave, Edge, Vivaldi, and Arc on macOS. Within 30 seconds the browser appears under **Connected browsers**.

## Linking a tab

<figure>
  <img src="/screenshots/chrome-link.webp" alt="The maiTerm extension's popup listing agent tabs to link the current page to" />
  <figcaption>Choosing the agent tab that may use this page.</figcaption>
</figure>

1. Open the page in your browser.
2. Click the maiTerm icon in the toolbar. The popup lists your agent tabs in maiTerm, the one in front in each maiTerm window first (marked **in front**), then the rest by when you last used them, with a filter.
3. Choose one. The page is now that agent's to use, and the extension's icon shows on the tab the name the agent knows it by: **b1**, **b2** and so on.

Tell the agent what to look at: "the orders page won't load, find out why" is enough for it to read the failing request and its response. To end it, click the icon again and **Unlink**, or click **Unlink** beside it in **Preferences → Chrome extension → Linked tabs**, which lists every link.

A link follows the tab: navigating, or reloading the page, keeps it. Reloading the *agent's* tab in maiTerm keeps it too. Links don't outlive the browser: after a restart of Chrome, link the tab again.

### Several tabs and popups

An agent can hold up to eight linked tabs: an app and its admin panel, two users in a chat app, a page and the docs it's built from. Link each one as above. The agent knows them as **b1**, **b2** and so on, the name on each tab's maiTerm icon, and works in whichever one it names. Until it names another, it stays on the one it named last: a tab you link beside it, or one that joins, doesn't pull it away.

A tab a linked page opens joins that page's agent by itself, so a flow like "Sign in with Google" can finish in its popup. That happens when the page opens a popup window, or opens a tab as the agent clicks, types or presses a key in it. A tab you open yourself from a linked page, with a middle-click say, stays yours, even while the agent is reading that page. The agent is told on its next step when a tab joins, closes, or is unlinked or moved to another agent.

Linking a tab that's already linked to another agent moves it to the one you choose.

:::note
Several tabs per agent and popups joining need a maiTerm release after 3.11.0. In 3.11.0 an agent has one tab, and linking another replaces it.
:::

## The tools

Agents in an agent tab get these while the feature is on. Every one acts on the caller's own linked tabs: the one it names with `tab` (**b1**, **b2**…), or the one it named last. An agent with no linked tab is told to ask you to link one.

| Tool | What it does |
|------|-------------|
| `browserStatus` | This agent's linked tabs, each with its name, title and address, which one opened it, and which is current |
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
