---
title: MCP Servers
description: Sign in to a remote MCP server once in maiTerm, and Claude Code and Codex both get its tools — under every managed account, and on your SSH hosts under your own sign-in.
---

Installing a remote MCP server the usual way means installing it once per agent: once in Claude Code, again in Codex, again for each [Claude account](/features/accounts/), again on every SSH host — and signing in each time. Those sign-ins then fight each other. Most services rotate their refresh tokens, so a token two places hold stops working for whichever one refreshes second.

maiTerm can hold the sign-in instead. Add a server in **Preferences → MCP servers**, sign in once, and maiTerm serves its tools to every agent, keeping the sign-in fresh itself. The agents never see the service's token.

It's opt-in, one server at a time. Servers you've installed directly in an agent keep working exactly as before, and maiTerm never edits them unless you click **Move to maiTerm**.

## What each agent sees

Each server you add appears to the agent as **its own MCP server**, under the name you gave it, not folded into maiTerm's tools. So everything a direct install gives you still works:

- **The same tool names.** A server called `linear` gives Claude `mcp__linear__…`, exactly as a direct install would.
- **Your rules still apply.** `/mcp`, turning a server off in the agent, and permission rules like `mcp__linear__*` work as usual.
- **Tools as the server publishes them.** Names, descriptions and schemas are passed through unchanged.

maiTerm writes these entries into Claude Code's `~/.claude.json` (and every [managed account](/features/accounts/)'s copy) and Codex's `~/.codex/config.toml`, for each agent whose maiTerm integration is on, and keeps them current alongside its own `maiterm` entry. They're removed when maiTerm quits, so a server added here works only while maiTerm is running.

### On your SSH hosts

Claude Code on a host you've reached through the [SSH MCP bridge](/features/agents/#ssh-mcp-bridge) gets the same servers. Each agent there reaches **your** maiTerm through your own tunnel, so it uses your sign-in, and on a host several people share, each person's agents use their own. A server you add reaches the hosts you're connected to straight away, and any other host the next time you connect to it.

The remote side only ever adds. maiTerm never overwrites a server of your own with the same name there, and never deletes an entry, because on a shared account it can't tell whether a teammate still uses it. An entry for a server your maiTerm doesn't serve is simply empty: no tools, and no error.

## Adding a server

1. Open **Preferences → MCP servers** and click **Add a server**.
2. Paste the server's URL and click **Check**. maiTerm asks the server how it signs in and says what it found: **No sign-in needed**, a sign-in with the service, or that it needs a token.
3. Choose the **name agents see it as**. One is suggested.
4. Click **Add and sign in** to finish the sign-in in your browser, or **Add** for a server with no sign-in or a token.

Some services publish a sign-in but don't let apps register themselves (GitHub, for one). For those, maiTerm asks for a **client id** from an app you registered with the service, plus its secret if it has one. The app's callback URL must allow `http://127.0.0.1` on any port. Or tick **Use a token instead of signing in** and paste an API key or access token.

Sign-ins, tokens and client secrets are kept in your system's credential store (the Keychain on macOS), never in a file. The URL itself is saved as you typed it, in plain text, so a key written into a URL isn't protected the way a token is. maiTerm refreshes a sign-in before it expires, and once more if the service refuses it.

## The server list

Each server shows its state — **Connected** with a tool count, **Needs a sign-in**, **Unreachable**, or **Off** — and these controls:

- **Sign in** / **Sign in again**, or **Replace token**.
- **Refresh** reads the tool list again; **Tools** shows it.
- **Workspaces** limits the server to the workspaces you tick. None ticked means every workspace. An agent in any other workspace sees the server with no tools.
- A switch to turn it off without removing it.
- **Move to maiTerm**, when the same server is also installed directly in an agent (see below).
- **Remove**. Agents stop seeing it in new sessions and maiTerm forgets the sign-in. The service itself isn't told, so revoke maiTerm's access there too if you want it gone.

Every tool of a server you add loads into every agent session in its workspaces, so add the ones you use.

### Installed twice

If a server here is also installed directly in Claude Code or Codex, its card says so: an agent would get every tool twice. If you've already given a server of your own the same name, maiTerm doesn't write its entry over yours, and the card says that too. **Move to maiTerm** removes your direct entries for that server, after you confirm. Their sign-ins stay where they are, and nothing is revoked.

## When a server is down

A server that goes down, or whose sign-in expires, **keeps its tools**. Calling one answers with a sentence the agent can pass on, saying what went wrong (the server can't be reached, or it needs signing in again, which you do in Preferences → MCP servers), instead of the tools vanishing in the middle of a task. The last good tool list is saved, so an agent session that starts while the server is down still gets yesterday's tools.

Only the server itself can remove a tool. When its tool list changes, maiTerm passes the change on: Claude Code picks it up in the running session, and Codex at its next session, because Codex ignores the notice mid-session.

## Agents can suggest it

Ask an agent to install an MCP server and it can tell you maiTerm is an option, with the trade-off, and let you choose:

- **Through maiTerm:** set up once for every agent here and Claude on your SSH hosts, one sign-in kept fresh by maiTerm, signed in as you on a shared host. It works only while maiTerm runs.
- **Installed directly:** works without maiTerm, but only in that one agent, and each agent signs in for itself.

If you choose maiTerm, the agent's `addMcpServer` doesn't add the server. It files a [request](/features/agents/#organising-tabs-meshes-and-maiterm), a card headed **Add this MCP server for every agent?** in the [Loom's Decisions](/features/loom/#decisions), and on your phone with [maiLink](/features/mailink/). The card is in maiTerm's words: the name, the URL, that its tools load into every session started afterwards, and how it signs in. Approve one that needs a sign-in at your desk and the sign-in opens in your browser. Approved from your phone, it waits as **Needs a sign-in** until you're back at your computer, since the sign-in finishes in your computer's browser. One that takes a token waits for you to paste it. Neither the agent nor the approval ever completes a sign-in.

Some proposals are refused before they reach you: a URL that could carry a key (anything after a `?` or `#`, or a path segment that looks like one), a service that needs a client id only you can supply, and a name or URL maiTerm already serves. You can still add any of those yourself. Agents also get `listMcpServers`, which shows the servers maiTerm manages, their state and tools. Anything in a URL that looks like a key is masked.

A running agent session doesn't see a newly added server. Start a new one.

## Limits worth knowing

- **Remote servers only.** v1 talks to servers over Streamable HTTP. A local server you start with a command (`npx …`) is still installed directly in each agent.
- **Tools only.** An MCP server's resources and prompts aren't passed through yet.
- **Claude Code and Codex on this computer, and Claude Code on SSH hosts.** Codex on SSH hosts and Gemini don't get these servers yet.
