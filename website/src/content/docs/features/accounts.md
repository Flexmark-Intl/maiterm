---
title: Agent Accounts
description: Hold more than one Claude Code login and run them side by side — a client's account in one tab, your own in the next, with no signing out.
---

Claude Code has a single login slot. The credential is global to the machine, so working across two organisations means `/logout`, `/login`, a browser round trip — and it takes every tab with it. The agent offers no way to switch, and no way to be two identities at once.

**Preferences → Accounts** holds as many logins as you need and hands each tab the right one as it starts. A client's account runs in the client's workspace while your own runs in the tab beside it, at the same time.

## What maiTerm actually holds

Nothing, is the short answer — and this is the load-bearing distinction in the whole feature.

Each account is its own **configuration directory**. maiTerm creates the directory, tells the agent to use it, and stops there: the agent runs its own sign-in, stores the credential its own way, refreshes it on its own timer and signs out of it on its own. maiTerm never reads, writes or parses a login, and never touches the keychain item one lives in.

That means there is nothing for maiTerm to get wrong when the credential format changes, nothing of yours in `aiterm-state.json`, and nothing an agent can reach over MCP.

## Turning it on

It is off out of the box, and until you turn it on maiTerm does not go near your sign-in.

**Set up…** signs you in once and switches the feature on in the same step. Afterwards the pane shows a **Manage agent logins** toggle and the list of accounts you hold.

The toggle and the accounts are separate things on purpose:

- **Manage agent logins, off** — the accounts are kept, but nothing is applied. New tabs use your normal login. One click puts it back.
- **Clear setup** — the destructive one. It signs each account out, deletes its directory and turns the feature off.

## Adding a second account

This is the step with a trap in it, and the dialog says so before you hit it: **your browser is already signed in**. The provider reuses that session, so a second sign-in usually hands back the account you already have without ever asking who you are.

Two ways through, both offered in the dialog:

- **Sign in privately** opens the sign-in link in a new private window of a browser that supports one, which has no session to reuse. Your normal browser is never opened, so there is no signed-in tab to click by mistake.
- **Sign in and copy link** opens nothing and puts the link on your clipboard, for pasting into a private or incognito window yourself. This is the path on Safari, which has no private-window switch to drive from outside.

If a sign-in comes back as an account you already hold, maiTerm refuses it and says which one rather than adding a duplicate row.

## The account list

Each row is one login, grouped by runtime, showing its plan and organisation. The dot on the left is the short version: green on the account new tabs launch under, dim otherwise.

| Control | What it does |
|---|---|
| **Use** | Makes this the account new tabs launch under |
| **Verify** | Reads back the identity this account currently resolves to |
| **Remove** | Signs the account out and deletes its directory |
| **Add account…** | Another sign-in, with the duplicate-session warning above |
| **Clear setup** | Removes every account and turns the feature off |

## Verify, and why "signed in" isn't the answer

Credential resolution is a fall-through. A cloud-provider variable, an `ANTHROPIC_AUTH_TOKEN`, an `ANTHROPIC_API_KEY` or an `apiKeyHelper` all rank *above* a subscription login — so a stray key exported in a shell profile quietly answers instead, while everything still reports itself as signed in.

**Verify** therefore reports the identity that genuinely resolved, not a green light. It says so out loud when all is well, and when it is not, it distinguishes the two cases that need different fixes: an account that is **signed out**, and one that is **resolving as something else** — naming what answered instead.

## Which account a tab uses

An account is an environment variable handed to the shell **when the tab starts**. A process's environment is fixed at exec, so nothing can move a running tab to a different account from outside.

So switching decides what *new* tabs use. Tabs already open keep the account they started with, and so does an agent already working in one.

To move tabs across, respawn their shells — reloading a tab (`Cmd+Shift+R`) keeps its name, directory and scrollback and restarts the shell inside it. Switching accounts offers to do that for you at the granularity you think in: everything, one window, or one workspace. Declining is a perfectly good answer, and often the right one — an agent mid-turn is interrupted by a reload, and your next new tab picks the account up anyway.

## Nothing else about the tab changes

Relocating an agent's configuration directory relocates *everything* in it — hooks, skills, slash commands, permissions and MCP servers included. Left alone, that would mean a tab under a managed account quietly lost its maiTerm integration, its task tools and its own project history.

It doesn't. Those are shared into every account, so a tab running as one behaves exactly like a tab that isn't. It is a different login, not a different setup.

## Limits worth knowing

- **This computer only.** Accounts apply to tabs on your own machine. Signing your SSH hosts in is a separate job that is not built yet — it carries a trade-off this does not, so it will be opt-in per host and explained there.
- **Claude Code today.** The other runtimes are listed in the sign-in dialog and decline rather than half-work.
- **Not for managed machines.** This is a solo and small-team feature. Where an administrator has set a login policy, working around it is not something maiTerm should automate.
