---
title: Agent Accounts
description: Hold more than one Claude Code login and run them side by side — a client's account in one tab, your own in the next, with no signing out.
---

Claude Code has a single login slot. One credential, shared by every terminal you open, so working across two organisations means `/logout`, `/login`, a browser round trip — and it takes every tab with it. The agent offers no way to switch, and no way to be two identities at once.

**Preferences → Accounts** holds as many logins as you need, and every tab starts under the one you have made active. Pick an account, open a tab, pick another, open the next — and the client's account is working in one while your own runs in the tab beside it, at the same time.

## What maiTerm actually holds

For the tabs on this computer, nothing — and this is the load-bearing distinction in the whole feature. (The one exception is opt-in, for SSH hosts, and has [its own section](#ssh-hosts).)

Each account is its own **configuration directory**. maiTerm creates the directory, links your existing setup into it and tells the agent to use it — and stops at the credential: the agent runs its own sign-in, stores it its own way, refreshes it on its own timer and signs out of it on its own. maiTerm never reads, writes or parses a login, and never touches the keychain entry or credentials file one lives in.

That means there is nothing for maiTerm to get wrong when the credential format changes, and nothing an agent can reach over MCP. What it does keep is what the agent reported back after sign-in — the account's label, plan and organisation, so the list has something to show. Never a login.

## Turning it on

It is off out of the box, and until you turn it on maiTerm does not go near your sign-in.

**Set up…** signs you in once and switches the feature on in the same step. Afterwards the pane shows a **Manage agent logins** toggle and the list of accounts you hold.

The toggle and the accounts are separate things on purpose:

- **Manage agent logins, off** — the accounts are kept, but nothing is applied. New tabs use your normal login. One click puts it back.
- **Clear setup** — the destructive one. It signs each account out, deletes its directory and turns the feature off.

## Adding a second account

This is the step with a trap in it, and the dialog says so before you hit it: **your browser is already signed in**. The provider reuses that session, so a second sign-in usually hands back the account you already have without ever asking who you are.

There are two ways through it, and the dialog offers whichever one your machine can actually do:

- **Sign in privately** — shown when a browser that can be told to open a private window is installed (Chrome, Brave, Edge, Chromium or Firefox). It opens the link in a fresh private window with no session to reuse. Your normal browser is never opened, so there is no signed-in tab to click by mistake.
- **Sign in and copy link** — shown when none of those is. It opens nothing and puts the link on your clipboard, for pasting into a private window yourself. Safari cannot be driven into a private window from outside, so a Safari-only machine gets this one.

Either way the sign-in link is also shown in the dialog with its own **Copy** button, so you can take it into any window you like.

If a sign-in comes back as an account you already hold, maiTerm refuses it and says which one rather than adding a duplicate row.

## The account list

Each account is a card, grouped by runtime, showing its plan, its organisation and when it was last verified. The active account's card carries a bar down its edge — accent-coloured when new tabs really launch under it, grey when it is active but **Manage agent logins** is off. The dot beside the name says the same thing in miniature.

Inside each card sits that account's **SSH hosts** section, with a one-line summary of what it covers — *Every host*, *2 hosts*, *No hosts yet*, *Not set up*. It only reads as in use on the active account, because only the active account is ever sent to a host. The other cards keep their host settings, drawn with a dashed outline and marked as applying only while that account is active.

| Control | What it does |
|---|---|
| **Use** | Makes this the account new tabs launch under — SSH tabs included, on the hosts it covers |
| **Verify** | Reads back the identity this account currently resolves to |
| **Remove** | Signs the account out and deletes its directory |
| **SSH hosts → Set up…** | Mints this account's remote token — see [SSH hosts](#ssh-hosts) |
| **Use on every SSH host** | Sends the token to every host while this account is active |
| **Add host** | Names one host at a time instead |
| **Remove token** | Stops maiTerm handing the token out, and forgets every host that depended on it |
| **Add account…** | Another sign-in, with the duplicate-session warning above |
| **Clear setup** | Removes every account and turns the feature off |

## Verify, and why "signed in" isn't the answer

Credential resolution is a fall-through. A cloud-provider variable, an `ANTHROPIC_AUTH_TOKEN`, an `ANTHROPIC_API_KEY`, a long-lived OAuth token or an `apiKeyHelper` all rank *above* a subscription login — and every one of them answers with no account attached at all. So "signed in" stays true while the account you picked is not the one doing the work.

maiTerm strips every one of those variables out of the environment it hands a tab, so nothing a tab inherits can quietly outrank the account it was started as. The rungs it cannot strip are the ones that are not variables at all: `apiKeyHelper` and an `env` block, both keys in `settings.json` — which is deliberately shared into every account, so they apply inside all of them.

**Verify** is what tells you. It asks the agent — in the same environment a tab gets — who it actually resolves to, so it reports an identity rather than a green light. It says so out loud when all is well, and when it is not, it distinguishes the two cases that need different fixes: an account that is **signed out**, and one that is **resolving as something else** — naming what answered instead.

## Which account a tab uses

An account is an environment variable handed to the shell **when the tab starts**. A process's environment is fixed at exec, so nothing can move a running tab to a different account from outside.

So switching decides what *new* tabs use. Tabs already open keep the account they started with, and so does an agent already working in one.

There is no per-tab or per-workspace assignment behind this. One account per runtime is active at a time, and a tab reads it once, at spawn. Two accounts can be live together because each tab keeps what it started with — but nothing records the pairing, so a tab that respawns later (a restart, a reload, a resumed workspace) comes back under whatever is active *then*.

To move tabs across, respawn their shells — reloading a tab (`Cmd+Shift+R`) keeps its name, directory and scrollback and restarts the shell inside it. Switching accounts offers to do that for you at the granularity you think in: everything, one window, or one workspace.

Declining is a perfectly good answer, and often the right one. An agent mid-turn is interrupted by a reload, and your next new tab picks the account up anyway. Reloading a workspace you are not currently looking at is the one to think twice about: its tabs come back, but their shells do not start until you next open that workspace, so an agent running there stops now and is out of reach until you visit it. In practice, reload the workspace you are in and leave the rest.

## Nothing else about the tab changes

Relocating an agent's configuration directory relocates *everything* in it — hooks, skills, slash commands, permissions and MCP servers included. Left alone, that would mean a tab under a managed account quietly lost its maiTerm integration, its task tools and its own project history.

It doesn't. Those are shared into every account, so a tab running as one behaves exactly like a tab that isn't. It is a different login, not a different setup.

## SSH hosts

An SSH tab runs its agent on another machine, and that machine has its own single login slot. Left alone, a remote agent is whoever that host was last signed in as — whichever account is active on your side.

You can have SSH tabs run as the active account instead. It is opt-in per account, from the **SSH hosts** section on its card, and it works differently from a local account in one way that matters: **here maiTerm does hold a credential.** A remote host cannot share your local sign-in, so the account needs a token of its own.

### Setting it up

**Set up…** has the agent mint that token — Claude Code's own `claude setup-token`, run in your browser the way sign-in is. The dialog lists what the token trades away (below) before anything is created, and offers the same private-window choice as adding an account, which matters even more here.

maiTerm stores the token in your system keychain under its own entry — the Keychain on macOS, the Secret Service on Linux. Never in its state file, and never reachable by an agent over MCP.

Then choose where it goes. Nothing is sent anywhere until you do one of these:

- **Use on every SSH host** — every SSH tab, while this account is the active one.
- **Add host** — one host at a time. A hostname or ssh alias covers every user on that host; `user@host` covers only that user.

### What happens when a tab connects

Only the **active** account is sent — switching accounts switches your SSH tabs along with your local ones, and as locally, a session already running keeps what it started with until you reload it.

When maiTerm starts an SSH session to a covered host — a new tab, a reload, a restored or resumed session, or an `ssh` you type yourself in a maiTerm tab — it first opens a separate, short-lived connection to that host and writes the token over it, into a file only your user can read in `~/.maiterm/tokens/`. The new shell reads that file once and deletes it; one that was never picked up is swept after ten minutes. The token never appears on a command line, in the terminal, in your scrollback or in your shell history, on either machine.

It also tells the agent which plan the account is on. A token on its own does not say, and that is not cosmetic: the plan decides which model the agent uses by default.

To check a session, run `/status` in the remote agent. **Auth token: CLAUDE_CODE_OAUTH_TOKEN** means it is running on the token maiTerm sent.

If the token cannot be delivered, maiTerm says so in a notification — because the tab will not fail. It comes up as whatever login the host already has, and the work is billed there.

### What the token trades away

- **It bills your subscription, not API credits** — the same plan the account already has.
- **It is narrower than a sign-in.** Model requests and MCP servers — maiTerm's own included — work normally, but Remote Control sessions and claude.ai connectors do not, and `--bare` sessions ignore the token. A host that needs those should be signed in on its own instead.
- **It lasts a year and does not rotate.** The card counts it down and warns in the last 30 days.
- **Nothing in maiTerm or the Claude Code CLI can revoke it.** **Remove token** stops maiTerm handing it out; a session already running with it keeps working.
- **maiTerm cannot tell which account a token belongs to.** A token carries no profile, so the agent reports no email for it. The browser window you approve in decides — which is why the dialog steers you to a private one.
- **On the host, it is as private as that user's processes.** Anyone who can read the environment of your remote shell can read the token.
- **Named hosts are the only record of where it went.** With **Use on every SSH host** on, there is no such list.

## Limits worth knowing

- **macOS and Linux.** The Windows build does not hand a tab its account yet — the pane works and the accounts are kept, but tabs still launch under your normal login.
- **SSH hosts need key-based login.** The token travels over its own connection, which cannot answer a password or passphrase prompt. A host you can only reach by typing a password gets a notification instead of the account.
- **SSH hosts need a keychain here.** A Linux machine with no Secret Service running — a headless box, say — has nowhere safe to keep the token, and setting one up fails there.
- **Claude Code today.** The other runtimes are named in the sign-in dialog as not yet available, rather than half-wired.
- **A key your own shell exports is still a key.** maiTerm cleans the environment it starts a tab in, but your shell profile runs afterwards, inside the tab. An `ANTHROPIC_API_KEY` exported there still outranks the account, and Verify — which asks from outside your shell — will not see it.
- **Not for managed machines.** This is a solo and small-team feature. Where an administrator has set a login policy, working around it is not something maiTerm should automate.
