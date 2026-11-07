---
title: Hive
description: Give a goal to a Queen agent, and she plans the work and runs a crew of sandboxed Claude Code workers on it — each on its own branch, landing back after a check you set — while you see the whole effort on one screen.
---

One agent at a time is a conversation. A real goal — rewrite the auth layer, build the landing page and the admin panel, produce a set of product shots — is a dozen pieces of work, some of which can run at once and some of which can't, and running a dozen agents by hand means being their scheduler, their reviewer and the person who merges what they did.

A **Hive** hands that job to an agent. You give a goal to the **Queen**, a Claude Code agent who plans the work in waves, starts **workers** to do it, answers their questions, has their work reviewed and lands it. Workers run in Claude Code's sandbox, each on its own branch of your repository. You see all of it on one screen and are asked only what the Queen can't decide.

## Making a Hive

Right-click an empty workspace in the sidebar — one with no tabs, or just one fresh shell — and choose **Make this a Hive…**. A short wizard asks one question a step:

| Step | Asks |
|------|------|
| **Goal** | *What should this Hive get done?* The goal, a name, an optional project brief, files to attach, and rules for every agent |
| **Folders** | *Where does the work happen?* The primary folder and any others. A git repo gets its own copies per worker; a plain folder (footage, brand assets, documents) is shared read-only. Each repo can have a **Setup** command and a **Land check** — both can wait, and the Queen can set them as the project takes shape |
| **Crew** | *Who does the work, and how does it land?* **Workers at once**, the Queen's and the workers' models and effort, **How work lands**, **Push after each land**, and **Chats on your phone** |
| **Tools** | *What can it make things with?* [What it can make](#making-media) on this Mac, the skills and MCP servers the Hive may use, and a **Budget for paid generation** |
| **Stack** | *Does the work need anything running?* The [stack services](/features/stack/) its workers can run on their own branches |
| **Review** | *Ready to start* — everything at once, then **Create Hive** |

Every step but Goal starts from your [defaults](#your-defaults). **Workers at once** is a ceiling, not a target: 24 by default, at most 64, and the Queen starts as many as the plan needs. A folder with nothing in it is a new project: maiTerm makes it a git repo with a first commit and trusts it, and the Queen adds the setup and land check once the code has them.

### What done means

Before any worker starts, the Queen writes down what *done* means for the goal and asks you: **Are these the completion criteria for “…”?**, with **Confirm** and **Change them**. It's a question on her [task](/features/tasks/#when-an-agent-stops-on-a-question), so it reaches you wherever her questions do, and maiTerm refuses to start a worker until you've answered it. The same criteria decide when it's finished: maiTerm won't let the Queen call the Hive done while anything is still running, waiting or unlanded, or until every criterion has evidence maiTerm checked itself.

## One screen for the whole Hive

A Hive replaces the workspace's terminals with its own view:

- **The Queen's chat** on the left. You give her the goal here and talk to her as you would to any agent.
- **The board** in the middle: a card per worker, grouped in waves by what each waits on, showing its role (Frontend, UI reviewer…), model and effort, what it's doing right now, its diff and its context.
- **Worker columns** on the right. Click a card and that worker's chat opens beside the board; click another to switch to it, or **Shift+click** to open a second beside the first. A column can also show the worker's raw terminal under its chat, if you need it.

Across the top: the goal on one line (**More** shows the rest), **Board**, **Needs you**, **Gallery** and **Memory** (`Cmd+Shift+Y` switches), the Hive's status with **Pause** and **Resume**, and a bar of readouts — slots in use, budget, criteria, where work lands, autonomy and the run queue. The ⋯ menu holds **New goal…**, **Earlier goals**, **Land log** and **Delete Hive…**.

| Shortcut | Action |
|----------|--------|
| `Cmd+1` | The Queen |
| `Cmd+2`–`Cmd+9` | A worker, by the badge on its card |
| `Cmd+Shift+[` / `Cmd+Shift+]` | The previous / next column |
| `Cmd+Shift+Y` | The next view |
| `Cmd+W` twice | Close the focused worker column |

`Ctrl` instead of `Cmd` on Linux. The Hive's agents aren't in the tab strip: they live in this view.

### Is it doing anything?

While a Hive is running, its status says what is actually happening: **Working** while any of its agents is at work or a job of theirs is running, **Waiting on you** when the only thing left is something in **Needs you**, **Stalled?** when a worker has gone quiet, and **Idle** otherwise.

A worker has gone quiet when its turn ended more than 10 minutes ago, nothing has happened in its tab since, and nothing it could be waiting for is on its way: no job of its own running, no question of its own waiting on you. Its card says **Quiet · … · no report**, and the Queen is told once, so a worker waiting on something that won't come doesn't hold the Hive up unnoticed. A worker whose job is still running says **Waiting on its run** instead.

A job result that comes in while you have the Hive paused is kept on disk, not in memory, and goes to its worker when you **Resume**, so restarting maiTerm in between loses nothing.

### Needs you

**Needs you** gathers everything any of the Hive's agents is waiting on you for — questions, permissions and approvals, from the Queen or any worker — in one list, answerable in place. Below it, **With the queen** lists what she's holding for now, each with **Take over** to answer it yourself.

## The Queen answers her workers

A worker's permission prompts, questions and blockers go to the Queen first. She answers what she can and brings you only what she can't decide. Some things always come to you: anything you've set to **Ask me**, a permission row that would save a rule or change a mode, and anything she hasn't answered in time (five minutes by default, set in **Autonomy…**).

**Autonomy…** in the Hive's settings decides who answers each kind of decision, **Queen** or **Ask me**: *Plan*, *Workers' questions*, *Workers' permission prompts*, *Landing*, *Picking finals in the gallery*, *Adding a dependency, changing a public interface*, and *Changing Hive settings*. Everything starts with the Queen except changing the Hive's settings, which asks you — and Landing, when work lands through an integration branch.

When you answer something yourself, the Queen can record it as a **precedent** and cite it the next time the same question comes up, so you aren't asked twice. Precedents, and every decision she made, are under **Memory → Precedents** and **Memory → Decisions**, where you can edit a precedent, make it apply to every Hive, or override a decision.

## Workers

**Sandboxed.** Workers run in Claude Code's own sandbox, with extra rules from maiTerm that keep their writes to their own copies, and a gate that checks their shell and git commands before they run and fails closed: if maiTerm isn't answering, a worker can't run a command or write a file at all. The Queen is not sandboxed: she's an ordinary agent tab with your settings, your MCP servers and your permission prompts, but she can't push, add a remote or merge a pull request.

**Side by side.** In a git repository each worker gets its own copy (a worktree) on its own branch, started from the tip of where work lands, so a dozen of them never edit the same files.

**Their own app.** Workers don't install packages in their own shell; the folder's **Setup** installs from the lockfile, and a reinstall, a build or a test run is a job they queue with maiTerm, which runs it in a sandbox of its own and schedules jobs so a dozen builds don't swamp the machine. A worker can serve the app it's building on a port booked for it, fetch its pages and take screenshots of them, so it can check what it made before reporting.

**The network.** A worker reaches only the hosts the Queen has granted the Hive, plus the package registries for installs. A refused host goes to her; she grants or removes hosts herself, with a reason that's kept in her decision log, and you can edit the list under **Workers and access…**.

**Reviewed.** A finished builder's work is checked by a fresh reviewer worker, on a copy pinned at the commits it reported. Three failed reviews go to the Queen to settle.

## Landing work

When a reviewed worker's work is ready, the Queen lands it — the exact commits that were reviewed — into the folder's **Base**, after the **Land check** you or she set for that repository passes. **How work lands** chooses between **Straight into Base** and **Into an integration branch, each land approved by you**. Whichever you chose, a land that changes agent or build configuration — hooks, settings, scripts that would run on your machine outside any sandbox — asks you first.

Landing never touches your uncommitted work. **Land log** lists every land, undo and post-land check, newest first, and **Undo…** on a land writes a revert commit and lands it like any other: history is never rewritten. **Push after each land** pushes when you've asked for it; the Queen never pushes on her own.

## Shared memory

Every agent in a Hive reads and writes one memory, so what one worker learned about the codebase the next one knows. **Memory → Files** shows it: read and edit any file, add one, delete one, and **Pin** a file to keep its line in the part of the index maiTerm never rewrites.

## Making media

A Hive's goal doesn't have to be code. It can make video, images, voice, music and more, with the tools this Mac already has and the providers you set up for it, without leaving maiTerm.

### What it can make

**What it can make** lists, for each kind of media — **Video**, **Images**, **Voice**, **Music** and **Finishing** first, then under **More**: design, sound effects, lipsync, upscaling, 3D, transcription, motion graphics and ComfyUI workflows — which tool on this Mac already covers it, or which provider maiTerm recommends for it and what that spends. **Re-detect** looks again.

It's in three places: the wizard's **Tools** step, above the tool picker; **Tools** in the Hive's settings; and **Preferences → Hive**, for every Hive on this Mac. In the wizard it opens on its own when the goal names media (a video, a voiceover, a logo…) or when none of the Hive's folders is a git repository; otherwise it stays folded to one line, so a code Hive isn't slowed down.

### Setting a provider up

**Set up** on a row opens a sheet with the one step that provider needs, then **Verify**:

| The provider | What the sheet does |
|---|---|
| A remote MCP server with a sign-in (fal, ElevenLabs, Runway and others) | Adds it to [maiTerm's MCP servers](/features/mcp-servers/) and opens the sign-in in your browser |
| One that takes a key (Replicate) | Opens its key page, and keeps the key you paste in your system's credential store |
| MiniMax and Meshy | Takes your key and runs the provider's server itself ([below](#minimax-and-meshy)) |
| The Homebrew kit (ffmpeg, ImageMagick, SoX, whisper.cpp) | Installs it in a tab you can see — with Homebrew first if this Mac has none — and then downloads whisper.cpp's speech model |
| An app (ComfyUI, Blender) | A download link; it's found again when you come back to the window |
| Remotion, Google genmedia | The steps only: maiTerm can't set these up for you, and **Verify** finds them once you have |

**Verify** checks that the Hive's agents can really use the provider, without spending anything: that its server is on and offered to the Hive's workspace, that its entry reaches the agents, that its tools list, and, where the provider has one, that its balance or price check answers. It never calls a tool that generates. The answer is **Ready**, **Needs sign-in**, **Needs credit**, **Not reachable** or **Didn't work**, with what to do about it.

A server set up from a Hive is offered only to that Hive's workspace, so other tabs don't load its tools. From Preferences you choose **Hive workspaces only** or **Every workspace**. In the wizard, a tool a setup makes ready is ticked for the Hive.

### MiniMax and Meshy

MiniMax (video, images, voice) and Meshy (3D) publish their MCP servers to run on your computer, so maiTerm runs them for you: it keeps the key in your keychain, gives it to the server and to nothing else, and stops the server when it has been idle for ten minutes. The key never appears in an agent's configuration.

Each server writes into an output folder of its own, which joins the Hive's output folders so the gallery can take what it makes. Both servers run outside every sandbox, so maiTerm checks every call before it reaches them: a path outside that folder is refused, and so are MiniMax's option to fetch a file from any URL and its tool that plays audio on your Mac.

### Changing a Hive's tools

A Hive's tools aren't fixed when you create it. **Tools** in the Hive's settings → **Change…** adds or removes them, and offers the network hosts and output folders the new ones need. Workers launched after the change get it; a worker already running keeps what it started with. The Queen is told.

### When the Queen needs something it can't make

If a worker reports a tool missing, or her plan needs one, the Queen asks for it. You get a card headed **Set up a tool for this Hive?** in the [Loom](/features/loom/#decisions)'s Decisions and on your phone, in maiTerm's words: the provider it recommends, what that spends and the network hosts it adds, with her reason quoted. Approve it at the Mac and the setup sheet opens in the Hive. From the phone, a Homebrew install starts at once if Homebrew is already on the Mac; anything else — a sign-in, a key — waits for you at the Mac, and the card says so. Once the provider is ready, maiTerm adds it to the Hive's tools and tells her.

An agent [proposing a Hive](#an-agent-can-offer-you-a-hive) can also say what media it will need, and those rows are marked in the wizard.

In a Hive with media tools, the agents' briefs map what the Hive can make, with which tool, what each spends and how long its results last, so workers use what's there instead of improvising a substitute.

## The gallery

Images, video, audio and documents a Hive's agents make go into the **Gallery**, with how each was made: **Info** shows who made it, the **Recipe** and its **Lineage** — what it was made from, and what was made from it.

The gallery opens on **For you**: what the workers made for you. Their evidence — screenshots of the app they built, test reports, notes — is under **Proof**, and **All** shows both. **Move to Proof** and **Move to For you** fix anything filed on the wrong side; what you drop in yourself is never guessed to be proof.

Filter by kind (**All**, **Images**, **Video**, **Audio**, **Docs**) and more, group it, and select assets to **Compare** side by side, **Star**, **Pick**, **Mark final** or **Reject**, tag, add to a collection, **Export…**, **Iterate with the queen** or **Send to worker**. Paid generation counts against the Hive's **Budget for paid generation**, $10 unless you set it; raising it is always your call.

## When it's done

A finished Hive is **Done**, with the Queen's **Report**. Give a Done or paused Hive another goal with **New goal**, in the same Queen tab: the sheet takes the new goal, brief and files, and **Start the new goal** begins the next round; work from the last goal that hasn't landed yet stays in view over the board. **Earlier goals** keeps the ones before it.

Or just message her: a message to a Done Hive's Queen — from the desktop or from your phone — wakes her in her old session.

**Delete Hive…** lists what stops, which branches haven't landed, and what's kept. Clean copies are removed; anything holding work, the shared memory included, stays on disk, and no branch is ever deleted.

## An agent can offer you a Hive

An agent you've been planning a project with can propose it as a Hive with `proposeHive`. You get a request naming the agent — **Make this Hive?**, or **A new goal for this Hive?** when a Done or paused Hive already works in that folder — in the [Loom](/features/loom/#decisions)'s Decisions and on your phone. On the desktop, **Open in the sheet** opens the wizard filled in from the plan, and nothing starts until you press **Create Hive**. On the phone, **Approve** creates it from exactly the plan the card shows. Either way, it's your decision, never the agent's.

## On your phone

With [maiLink](/features/mailink/), a Hive's chats come to the phone like any other: each shows its role and status, and the Queen's shows her Done report. **Chats on your phone** chooses **The queen only** (the default), **The queen and its workers** or **Nobody**.

## Your defaults

**Preferences → Hive** holds the **Defaults for new Hives** the wizard starts from: **Tools**, **Models**, **Workers at once**, **Budget for paid generation**, **Chats on your phone**, **Push after each land** and **Rules for every agent**. The wizard's **Save as my defaults** saves its choices back here. No agent can change these.

## Requirements

- **Claude Code 2.1.295 or newer.** Every agent in a Hive is Claude Code.
- **macOS.** A Hive needs Claude Code's sandbox, which native Windows doesn't have, so Windows can't make one. On Linux a Hive can be made, but the jobs workers queue — installs, builds and tests, served apps and land checks — run only on macOS in this version.

:::note
A Hive is new in maiTerm 3.13.0. What it can make, setting providers up from it, changing a Hive's tools after it's made, and the gallery's **For you** and **Proof** are new in 3.14.0.
:::
