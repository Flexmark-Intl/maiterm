# Claude Code Projects (beta): research notes

Research for the Hive feature, 2026-10-23. This describes Anthropic's product as it
ships, not anything maiTerm does yet. The design for our local version comes later and lives
elsewhere.

**Confidence labels.** Unmarked statements come from Anthropic's own docs
(`code.claude.com/docs/en/claude-projects`) or launch post. **[reported]** marks something seen
only in third-party write-ups or hands-on reports. **[not documented]** marks a gap: something
Anthropic doesn't say, which our design has to decide for itself.

## 1. What it is

On 2026-09-17 Anthropic relaunched "Projects" in Claude Code as a public beta (Pro and Max
plans; the waitlist was opened to everyone on 2026-10-09 **[reported]**). An old Project was a
folder: instructions, files, and a memory that every chat in it could read. A new Project is
**one long-running conversation with a coordinator Claude**, which turns what you send it into
**threads**. A thread is a full Claude Code session, normally a cloud session, working in
parallel on its own branch.

Anthropic's own summary: *"Projects have threads that do the work and a coordinator that
directs them."* Its framing: brief the project "the way you'd brief a chief of staff."

Who it's for, in the docs' words: work "that has a goal that outlasts one session and keeps
producing tasks". For example, one goal across many repos, an area you keep feeding bugs into,
a build or migration too big for one session, or document work with no code.

It is available on the web (claude.ai/code), in the desktop app and in the mobile app, **but
not in the CLI, VS Code or JetBrains**, and not through Bedrock, Vertex or Foundry.

## 2. The parts

| Part | What it is |
|---|---|
| **Project conversation (coordinator)** | One long-running session. It takes your messages, decides what becomes a thread, and keeps track of every thread it started. **It sees what threads report back, not every step they take.** |
| **Threads (workers)** | Separate sessions, each with its own context window. Each does one piece of work and reports back when done. A cloud thread runs on its own branch and its own clone, and opens a PR when the work calls for one. |
| **Project memory** | Files Claude writes about the project (requirements, decisions, pitfalls), indexed by a `MEMORY.md`. Shared across threads. |
| **Project instructions** | Up to 16,000 chars of standing rules, sent to every new thread *and* to the coordinator. |
| **Library** | Files you uploaded plus files threads produced. |
| **Environment** | Repos every thread clones, uploaded files under `/mnt/project-files`, and a cloud environment (network allowlist, env vars, network secrets, setup script). |
| **Overview pane** | Thread board grouped by state, plus Library, Pull requests and Scheduled tasks tabs. |

## 3. The orchestrator (coordinator)

### 3.1 Routing

For each message you send, the coordinator decides one of three things:

- **Answer in place.** Quick questions get answered in the conversation.
- **Start a new thread.** Several unrelated tasks in one message become separate threads. Each
  new thread appears under your message as a **card** (title and status; click to open).
- **Send it to an existing thread** "already working in that area", and say which one.

It can also **propose** threads rather than start them: a "Suggested threads" list with a
start arrow on each and a "start all" button. On a brand-new project it takes a turn of its
own. It starts one *read-only exploration thread* that proposes next steps, and posts **Setup
recommendations** drawn from your recent cloud sessions: repos to add, scheduled tasks, and
threads it could start.

**[not documented]** How it decides that work "belongs" to an existing thread. Anthropic gives
no routing logic beyond "Claude decides".

### 3.2 Coordinator context

- It doesn't keep a full history. It "works from **recent messages, recent threads, and project
  memory** rather than its full history, so it keeps going for as long as the project runs."
  Anything that must never be dropped belongs in project memory.
- Threads compact themselves automatically. You never manage context windows.
- By default the coordinator runs on **Opus at low effort** and threads run on **Opus at high
  effort**. Each has its own separate model and effort setting.
- **The coordinator has no MCP connectors.** Work that needs one has to become a thread.
- Settings changes (instructions, repos, plugins, environment) reach **new** threads only, never
  threads already running.

### 3.3 Steering knobs are soft

You tune the coordinator in plain language: "propose threads and wait for my go-ahead", "at
most two threads at a time", "reuse an existing thread for follow-ups", "only post when
something finishes or is blocked", "answer that here instead of starting a thread". **Claude
saves these to project memory and follows them, but they are not enforced.** "A thread limit
you give this way isn't a hard cap." The only enforced limit is **200 new threads per day**
across all projects.

### 3.4 What the coordinator does *not* do

- **It can't unblock a thread.** A permission prompt lives inside its thread. "Telling Claude
  in the project conversation to go ahead doesn't reach it." Confirmed hands-on **[reported]**.
- A message typed into a thread's own box goes straight to that thread. A follow-up sent to
  the coordinator reaches the thread only if the coordinator matches it to that thread.
- It doesn't stream thread progress. A thread that's running with no news is "usually still
  working". Hands-on testers saw it sometimes fail to report progress at all **[reported]**.

## 4. Threads: "each agent with its own copy of the code"

### 4.1 Isolation

- **A cloud thread is a cloud session**: an isolated Anthropic-managed VM. It **clones** each of
  the project's GitHub repos into its own sandbox and works on a **new branch started from the
  repo's default branch**. The docs say plainly that worktrees aren't needed, because each
  thread has its own clone.
- GitHub credentials never enter the VM. A GitHub proxy attaches them on the server side.
  Network secrets work the same way.
- Overlap between threads is **not prevented**. Two threads touching the same code produce an
  ordinary git merge conflict on their PRs. There is no file locking or partitioning.
- A thread's sandbox **pauses between turns** and resumes later. If it can't be resumed, the
  thread continues from a **fresh clone and uncommitted work is lost**. The docs advise
  committing and pushing work in progress.
- A thread can add a repo the project doesn't have, but only for itself, and only from a GitHub
  owner the project already uses.

### 4.2 What a thread starts with

| Carried | Not carried |
|---|---|
| Project repos, files, instructions, and memory (MEMORY.md) | Anything installed only on your laptop |
| Root `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/` of each repo | Subdirectory `CLAUDE.md` and path-specific rules |
| Your claude.ai account's connectors (MCP) | The repo's `.mcp.json` |
| Account-enabled skills; plugins chosen in Project settings | Repo `.claude/skills/`, `.claude/agents/`, commands, plugins in `.claude/settings.json` |
| Cloud environment env vars and setup script | Repo `.claude/settings.json` permission rules, hooks and `env`: **none are applied** |

Inside its task, a thread can fan out further, using subagents, loops and dynamic workflows.

### 4.3 Permissions

Threads run in **auto mode** (a classifier approves tool calls). The auto-mode classifier
blocks, among other things, "merging a pull request no human has approved, approving Claude's
own pull request, or disabling CI checks", plus force pushes and production deploys. **That is
general auto-mode behaviour (from the permission-modes docs), not something specific to
Projects**, but it is what keeps a thread from landing its own work. When an approval is
needed, the thread waits. Approving it covers that one prompt, or the rest of the thread.

### 4.4 Lifecycle: the PR loop

1. Work on a branch.
2. Open a PR when you ask, or on its own for a concrete fix.
3. **Watch the PR with auto-fix on**: push fixes when CI fails, address review comments, and
   reply in the thread once checks pass. GitHub doesn't send an event for a base-branch
   conflict, so auto-fix can't react to one. You have to ask for a rebase.
4. The thread's card offers next-step buttons (**Resolve conflicts**, **Fix CI**, **Address
   comments**, **Merge it**), each of which sends that instruction to the thread *as a message
   from you*, plus **Review PR** and **Create PR**.
5. An idle thread wakes up, and spends usage, when CI fails or a comment lands on its PR.

The **Overview** board groups threads by state:

| Group | Meaning |
|---|---|
| Ready for review | PR open, awaiting review |
| Waiting on you | Needs your reply or approval, or failed |
| Working | Still running |
| Landing | PR approved or queued to merge |
| Idle | Finished, not waiting on anything |
| Resolved | Marked done: by you, by Claude after your last step (e.g. the merge), or automatically after a week idle |

In the desktop app you get a notification when the coordinator posts, a thread errors, or a
thread needs input. Optionally you also get one each time a thread finishes a turn.

### 4.5 Threads on your own machine

Since the launch post (which said "coming very soon"), a thread can run **locally**. You
connect a folder through Remote Control (the desktop app setting, or `claude remote-control`
in the folder, with `--spawn worktree` to give each thread its own git worktree), then ask with
**Work locally**. An "Allow Claude to work in a folder on your device" card confirms it. That
card warns that two threads in one folder overwrite each other unless **Worktree** is on.
Compared with a cloud thread, a local thread:

- uses that machine's files, tools, MCP servers, settings, **hooks and permission rules**;
- gets the project instructions but **not the memory files**;
- runs only while the machine is awake, and pauses when it sleeps.

Anthropic's own local design therefore uses **one worktree per thread**, which is the
local equivalent of "each agent has its own copy of the code".

## 5. Shared memory

What Anthropic says:

- Memory is **a set of files plus a `MEMORY.md` index**, the same shape as Claude Code's local
  auto memory but kept separately. Every cloud thread **reads the index when it starts and
  opens individual files only when it needs them** (progressive disclosure, not a full load).
- **Every thread and the coordinator write it.** You add to it by telling Claude to remember
  (or forget) a requirement, decision or pitfall, either in the conversation or in any
  thread. Claude also saves your stated preferences (thread count, update style) there on its
  own, and it "learns your working and communication style".
- The launch post's examples of what lands there: a changed release date, why a feature was
  dropped, who to consult before changing a service. Corrections made to one thread are meant
  to be remembered, so later threads start with them.
- You can read, edit and delete it in **Project settings → Memory** (listed as "Auto memory").
  Early coverage claimed it was unreadable. The current docs contradict that.
- **It is distinct from repo `CLAUDE.md`.** Facts about a repo belong in that repo's
  `CLAUDE.md`. Facts about the *project* belong in project memory.
- The **coordinator's continuity depends on it**, because the coordinator drops old history
  (§3.2).

**[not documented]**: whether a running thread sees memory written by another thread
mid-task (the docs say only "when it starts"); how concurrent writes are merged; whether
anything expires or gets pruned; how the coordinator decides what's worth remembering.

How it relates to the **Library**: memory holds *notes*, the Library holds *artifacts* (uploads
and thread outputs). Threads deliver non-code results as Library files.

## 6. How it differs from Claude Code's other multi-agent modes

| | Who coordinates | Workers | Isolation | Shared state | Lifetime |
|---|---|---|---|---|---|
| **Subagents** | The calling session | In-session, own context | Optional worktree | Result returned to caller | One task |
| **Agent view** (`claude agents`) | You | Background local sessions | Worktree per session | None (reports to you) | Per session |
| **Agent teams** (experimental, off) | A lead session | Teammate sessions | **None**: partition files | Shared task list (file-locked claims, dependencies) + per-agent JSON mailboxes in `~/.claude/teams/` | Ends with the lead's session; no nesting |
| **Dynamic workflows** | A script | Many subagents | Per agent | Script-held plan, cross-checked results | One run |
| **Projects** | A coordinator conversation | Full sessions (cloud or local) | Clone + branch (cloud) / worktree (local) | Memory files + instructions + Library | Days to weeks; survives your laptop |

The distinguishing traits of Projects: the coordinator **persists indefinitely** (rolling
context plus memory), workers are **whole sessions** that can themselves fan out, workers report
**only to the coordinator** (no peer-to-peer messaging is documented, unlike agent teams), and
the **unit of delivery is a PR**.

## 7. Limits and known problems

- Each project belongs to one user. It can't be shared, and there are no org controls during the beta.
- A thread belongs to one project and can't be moved out. Projects can't be merged.
- Code has to be on github.com with the Claude GitHub App installed (not GHES, GitLab or Bitbucket).
- Uploads are copies: a local file change doesn't reach the project until you upload it again.
  Folders are capped at the first 100 files / 200 MB, and a project holds at most 10 folders.
- Usage burns fast. Each thread is a full session, plus the coordinator's own tokens, plus PR
  watchers waking up. A thread that hits the plan limit **waits and resumes** in the next window
  on its own. Reviving a thread that has been idle for over an hour re-reads its whole
  transcript (the prompt cache has expired), so a fresh thread can be cheaper.
- Hands-on reports **[reported]**: an attached `spec.md` wasn't picked up and had to be
  restated; progress reports were sometimes missing; anything needing the local environment
  was awkward before local threads arrived.

## 8. What maps onto maiTerm today (orientation only)

This isn't a design, just which existing pieces each concept touches, so the design
discussion starts from what's already here.

| Projects concept | Closest maiTerm piece | Gap |
|---|---|---|
| Coordinator conversation | The Overlord agent (docs/overlord.md): per-window supervisor, `driveTab`, `replyToOverlord` | The Overlord supervises tabs the human started. It doesn't scope a goal and spawn its own workers. Agent-created tabs are an approval-gated request (docs/agent-requests.md) |
| Threads | Agent tabs (local or SSH), each a real PTY session | No "spawn N workers for this goal" path; no per-worker code copy |
| Own copy of the code | Nothing yet (the Files column only *recognises* worktrees) | One git worktree per worker, as Anthropic's own local mode does |
| Thread board / Overview | maiTerm Tasks + the Loom (Weave, Decisions) + the Overlord board | Board states are task lanes, not PR states (Ready for review / Landing) |
| Thread reports to coordinator | `replyToOverlord` ack/status, mesh messages | No structured "final report" contract |
| Shared memory | Workspace notes, per-tab notes; Claude's own `MEMORY.md` auto memory per account | No project-scoped memory that every worker reads at start and writes back |
| Project instructions | The initSession priming / mesh join text | Not one editable brief |
| Waiting on you | Loom Decisions, task `blocker`, permission/question cards, the phone | Already stronger than Projects: the coordinator here *can* see a worker's prompt |
| PR watch / auto-fix | Follow-ups with watch scripts (docs/follow-ups.md) | Nothing PR-specific |
| Keeps running with laptop closed | Not applicable (local), though SSH tabs run on a remote host | — |

The one thing a local version gets for free that Anthropic's cloud threads don't: workers
run with the machine's own hooks, MCP servers, skills and permission rules, which Projects'
cloud threads drop (§4.2).

## Sources

- Anthropic docs: [Let Claude coordinate ongoing work with Projects](https://code.claude.com/docs/en/claude-projects)
- Anthropic: [Projects redesigned: from folder to conversation](https://claude.com/blog/projects-redesigned)
- Anthropic docs: [Run agents in parallel](https://code.claude.com/docs/en/agents), [Agent teams](https://code.claude.com/docs/en/agent-teams), [Cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web), [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Hands-on: [まさお, note.com (2026-10-13)](https://note.com/masa_wunder/n/n1c1e7b06a4bf)
- Coverage: [MarkTechPost](https://www.marktechpost.com/2026/09/17/anthropic-launches-claude-code-projects-in-beta-parallel-cloud-sessions-that-keep-running-after-you-close-your-laptop/), [Unite.AI](https://www.unite.ai/anthropic-redesigns-claude-code-projects-to-coordinate-agent-threads/), [MindStudio](https://www.mindstudio.ai/blog/claude-projects-agentic-os-threads), [Cellcog](https://cellcog.ai/blog/claude-code-projects-redesign/)
