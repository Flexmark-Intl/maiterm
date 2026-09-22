---
title: Workspace Share
description: Hand a workspace to someone else as one file. They open it and get the same tabs in the same repositories — maiTerm clones the ones they don't have — without any of your accounts, scrollback or secrets.
---

A workspace takes a while to get right: the repo in one tab, its API in a split beside it, a shell on the staging box, an agent in each, the dev server and the database in the stack. When a teammate joins the project, or you move to a second machine, rebuilding all of that by hand is an afternoon of "which directory was that in again".

**Workspace Share** writes a workspace to a single `.maiterm-workspace` file. Whoever opens it gets the same layout, the same tabs in the same repositories, the same services — and anything they don't have yet is cloned for them.

It is not a backup. A [state backup](/features/terminal/#state-backup--import) restores *your* machine exactly: your preferences, your accounts, your scrollback, your agents' sessions. A shared workspace is meant for someone else's machine, so it carries the **structure** of the workspace and none of your identity.

## Sharing a workspace

Right-click a workspace in the sidebar and choose **Share workspace…**. Before anything is written, the dialog shows exactly what will travel:

- **Directories** — every repository the workspace's tabs, editor files and services live in, with its remote and branch. Tabs sitting in `app/`, `app/api/` and `app/web/` are *one* repository, shared once.
- **Tabs** — each tab, where it is, and whether it's shared at all. Diff tabs stay behind (they hold file contents), and so do board tabs.
- **Stack services** — each one ticked by default; untick any you don't want to hand over.
- **Also include** — **Notes** and the **Task board**, both off unless you tick them.

It warns you — without stopping you — when the other person won't get the code you have:

- the branch has unpushed commits, or no upstream at all;
- the directory isn't a git repository with a remote, so they'll have to supply it themselves;
- `HEAD` is detached, so they'll get the remote's default branch.

Click **Save…** and pick where the file goes.

### Environment variables are yours to give

A stack service's environment often holds tokens. The **names** of its variables always travel, so the service is complete on the other side — but their **values** only travel if you tick them. Each service lists its variables under **Send values for:**, with **all** and **none** to tick or clear the lot. Everything starts unticked. Whatever you leave out, the person opening the file is asked for.

### Agent tabs

A tab running Claude Code or Codex — or one whose auto-resume starts one — is listed with a **Start Claude** (or **Start Codex**) tick. Leave it ticked and the tab starts an agent on the other side; untick it and it arrives as a plain shell, and none of its agent commands come with it.

## What travels, and what doesn't

| Travels | Only if you tick it | Never |
|---|---|---|
| The workspace's name and pane layout | Notes | Preferences — accounts, chat credentials, anything machine-specific |
| Every tab: its name, and where it is inside its repository | The task board | Scrollback |
| SSH tabs: the host and the remote directory | Stack environment values | Your agents' local sessions |
| Editor tabs, local and remote | | Diff and board tabs, archived tabs |
| Stack services and their variable names | | Composer drafts, tab variables, bridges, chat bindings |
| A mesh workspace, and each tab's role in it | | Mesh conversations |

Paths under your home directory are stored relative to it, so a teammate who keeps their code in the same place as you gets every directory matched without being asked.

The ssh command that goes in the file is the one you typed — maiTerm strips out everything it injects into a live SSH session, including its own connection credentials, before it writes anything.

## Opening a shared workspace

Double-click the `.maiterm-workspace` file, or use **File › Import Shared Workspace…**. maiTerm reads it and shows each repository it needs, with what it's going to do about it.

### Where each repository goes

One rule decides every directory — the one the sender used, one you pick, or one maiTerm proposes:

| The directory… | maiTerm… |
|---|---|
| doesn't exist, or is empty | clones the repository into it |
| is already a checkout of the same repository | uses it as it is — no clone |
| is anything else | refuses it, and says why |

"The same repository" means any of your checkout's remotes names the same repository as any of the sender's, however it's written — `git@github.com:org/app.git` and `https://github.com/org/app` are one repository, and so is a fork whose `upstream` is theirs. On GitHub, GitLab and Bitbucket, differences in capitalisation don't count either.

If the sender's path works on your machine, it's used. If it doesn't, **Create all in…** takes a single parent folder and puts each missing repository in a folder of its own name inside it — using any that are already there and match, and asking you only about the ones it can't. **Change…** picks a directory for one repository; **Skip** leaves it out, and its tabs open in your home directory instead.

A directory that isn't a git repository — a scratch folder, a docs directory — is used if it exists on your machine, and otherwise you choose one.

### Checking access first

Before anything is created, maiTerm asks each repository's remote whether you can reach it. A repository you don't have access to is flagged — **"You don't have access to this repository"** — and the import won't go ahead until you fix that or point it at a checkout you already have. It also learns whether the sender's branch exists, and if it doesn't, clones the default branch instead and tells you.

The check can't answer a passphrase or a two-factor prompt, so when one of those is what's in the way it only says it couldn't check — the clone itself will ask.

### Cloning

Click **Set up workspace**. Each clone runs in an ordinary terminal tab you can see, one at a time — so if git wants your SSH passphrase, a security key touch or a new host key, you're right there to give it. A clone that finishes closes its tab; one that fails leaves the tab open with git's own error in it, and a **Retry** beside it.

When every repository is in place, maiTerm builds the workspace, puts it after the one you're in, and switches to it. Every tab opens where it was on the sender's side, relative to wherever that repository landed on yours.

## Agents in a shared workspace

- **A local agent tab** starts a fresh session in its directory. The sender's conversation lives on the sender's disk; there's nothing to pick up from it.
- **A remote agent tab** tries to **fork** the sender's session on that host — a copy you continue on your own, which never writes into theirs. That only works when you sign in to the host as the same user the sender did, since that's where the conversation is stored. If you sign in as someone else, maiTerm sees the agent report that the session doesn't exist and starts a fresh one in the same remote directory instead. Either way you get a working agent, not a dead tab.

Remote tabs assume you already have SSH access to the hosts: the file carries the command, not a way in.

### Mesh workspaces

A [mesh workspace](/features/mesh-workspace/) shares as a mesh. Every tab keeps its name and its role, and the agents that start in it join each other the way they would anywhere. The conversations the sender's agents had with each other stay behind.

## Things to know

- **Nothing about the file is secret by design** — no accounts, no tokens, no scrollback. It does name your repositories, your hosts, and any environment values you chose to tick. Send it the way you'd send those.
- **A file from a newer maiTerm is refused** rather than half-imported. Update, then open it again.
- **Opening the same file twice gives you two workspaces.** Nothing in them collides with the first.
- **On Linux, double-clicking a `.maiterm-workspace` file works with the `.deb` install.** The AppImage can't register a file type with the system, so use **File › Import Shared Workspace…**.
- **On Windows**, maiTerm can't always see when a clone's command has finished, so the row may ask you to click **It's finished** once the clone tab shows it's done.
