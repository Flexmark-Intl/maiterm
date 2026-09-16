/**
 * Comparison content.
 *
 * Ground rules, because a dishonest comparison page is worse than none:
 *
 *  - Describe the other tool the way its makers would recognise. No strawmen.
 *  - Every row states what the OTHER tool does well, not only where we win.
 *  - `theirEdge` is mandatory and must be real. If we cannot name something the
 *    alternative genuinely does better, we do not understand it well enough to
 *    publish a comparison of it.
 *  - Claims about the other tool are as of the date below and sourced from
 *    their own public docs and marketing. Re-check before editing.
 */

export const COMPARED_AS_OF = '2026-09';

/**
 * Inline `code` spans only — the copy below is hand-authored in this file, so
 * a full markdown dependency would be overkill, but raw backticks rendered as
 * text look like a bug. HTML is escaped first so the conversion cannot inject
 * anything beyond the <code> it is asked for.
 */
export function inlineCode(text: string): string {
  const escaped = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
  return escaped.replace(/`([^`]+)`/g, '<code>$1</code>');
}

export interface ComparisonRow {
  dimension: string;
  maiterm: string;
  theirs: string;
}

export interface Comparison {
  slug: string;
  name: string;
  /** One line for the index card. */
  summary: string;
  /** What the other tool is, in its own terms. */
  whatItIs: string;
  /** The honest one-paragraph answer to "which should I use". */
  verdict: string;
  /** Something the other tool genuinely does better. Required. */
  theirEdge: string;
  rows: ComparisonRow[];
}

export const COMPARISONS: Comparison[] = [
  {
    slug: 'warp',
    name: 'Warp',
    summary:
      'Warp is a terminal-shaped development platform with its own agent and cloud features. maiTerm has no agent of its own — it runs yours and keeps track of them.',
    whatItIs:
      'Warp is a Rust terminal with a GPU renderer, structured command blocks, saved workflows, and an AI agent built into the product, backed by team and cloud features. The agent, the terminal and the platform are one thing, sold together.',
    verdict:
      'If you want one vendor to supply the terminal and the agent, and you like working in blocks with cloud sync across machines, Warp is a coherent product and does that well. maiTerm is for the other case: you have already chosen Claude Code or Codex, you pay for them directly, and what you are missing is somewhere to run a dozen of them without losing track.',
    theirEdge:
      'Warp is a more polished terminal in the conventional sense — the block model, the command palette, the completions and the GPU renderer are genuinely nice to use, and its agent is tuned to the terminal it lives in. It also has a real team offering, which maiTerm has nothing equivalent to.',
    rows: [
      {
        dimension: 'The agent',
        maiterm:
          'None built in. Runs the Claude Code or Codex CLI already installed on your machine, on your own account.',
        theirs: 'Warp ships its own agent, billed by Warp, with your own model keys as an option.',
      },
      {
        dimension: 'Who sees your prompts',
        maiterm: 'The local CLI and its provider. maiTerm is not in the path.',
        theirs: 'Warp’s agent runs through Warp’s platform.',
      },
      {
        dimension: 'Many agents at once',
        maiterm:
          'The design centre: a shared kanban board, a rules-driven supervisor per window, and agent-to-agent messaging across repositories.',
        theirs: 'Supported, with its own multi-agent features; oriented around Warp’s own agent.',
      },
      {
        dimension: 'Account required',
        maiterm: 'No account, no sign-in, no licence check.',
        theirs: 'An account is part of the product.',
      },
      {
        dimension: 'Remote work',
        maiterm:
          'SSH is first class — remote agents get the same tab identity, task board, notes and MCP tools over a reverse tunnel.',
        theirs: 'SSH works, with Warp’s own remote features layered on.',
      },
      {
        dimension: 'Platforms',
        maiterm: 'macOS (Apple Silicon), Windows, Linux.',
        theirs: 'macOS, Linux and Windows.',
      },
    ],
  },
  {
    slug: 'iterm2',
    name: 'iTerm2',
    summary:
      'iTerm2 is the mature, deeply configurable macOS terminal. maiTerm is narrower: the same daily ergonomics plus state management for a desk full of agents.',
    whatItIs:
      'iTerm2 is the long-standing macOS terminal emulator: profiles, split panes, triggers, tmux integration, shell integration, and roughly two decades of accumulated configurability. Free, and about as battle-tested as a terminal gets.',
    verdict:
      'If you are not running coding agents, keep using iTerm2 — maiTerm is not trying to out-terminal it, and will not win on breadth of configuration or on maturity. The reason to switch is specific: you have several agent sessions going at once and you have started losing track of which one needs you, what it is working on, and what it was doing before the last restart.',
    theirEdge:
      'iTerm2 is more configurable in almost every dimension, has years more hardening, supports Intel Macs, and has an ecosystem of guides and dotfiles maiTerm cannot match. Its tmux integration in particular has no equivalent here.',
    rows: [
      {
        dimension: 'Agent awareness',
        maiterm:
          'Tracks each agent’s live state — working, waiting for permission, done — per tab, per workspace and in a global footer roll-up.',
        theirs: 'None. An agent is just a process writing to a terminal.',
      },
      {
        dimension: 'Shared task state',
        maiterm:
          'A kanban board the agent and you both work — seven lanes, workstreams, real dependencies, assignment — scoped to the project.',
        theirs: 'Not applicable.',
      },
      {
        dimension: 'Restart behaviour',
        maiterm:
          'Tabs, layout, scrollback, working directories, SSH sessions and agent sessions all come back, with each agent reconnected.',
        theirs: 'Window arrangements and profiles restore; running sessions do not.',
      },
      {
        dimension: 'Configurability',
        maiterm: 'A focused set of preferences, themes and triggers.',
        theirs: 'Far broader. Two decades of settings, and tmux integration.',
      },
      {
        dimension: 'Platforms',
        maiterm: 'macOS (Apple Silicon), Windows, Linux.',
        theirs: 'macOS only, Intel included.',
      },
    ],
  },
  {
    slug: 'solo',
    name: 'Solo',
    summary:
      'The closest comparison: both wrap the agent CLIs you already have, and both run your project’s processes. Solo leads on breadth of agent support and hands-on process control; maiTerm discovers the stack rather than asking you to declare it, and leads on supervising agents and reaching them remotely.',
    whatItIs:
      'Solo is a Tauri desktop app that describes itself as a meta-harness for coding agents. It runs the CLI agents already installed on your machine alongside your project’s processes — dev server, queue workers, tunnels — defined once in a shared `solo.yml`, and exposes the whole workspace to agents through MCP, HTTP and a CLI, with scratchpads, todos, prompt templates and agent spawning as primitives.',
    verdict:
      'These two overlap more than either does with anything else, and since maiTerm 2.4 they overlap on your dev stack too — nine processes, duplicate ports and rebuilding the same layout every morning are a problem both of them now answer. They come at it from opposite ends. Solo has you declare the processes once, in a file that commits with the repository. maiTerm reads what the project already declares — `package.json` scripts, a Procfile, a compose file, a justfile — and offers that back as a checklist, so setting a project up is a few ticks, or one instruction to an agent. Solo’s way is the better one when the processes are not written down anywhere yet; maiTerm’s avoids keeping a second copy of something the repo already says. Past that the question is what else is watching, and if the pain is the agents themselves — a dozen sessions, one about to hit a compaction wall, one stuck at a permission prompt, three of them on remote hosts — that is what maiTerm is built around.',
    theirEdge:
      'Solo does its own port and orphan handling. maiTerm has no socket discovery at all: a port reaches the sidebar only when an agent reads it in a service’s output and reports it, so run the stack without agents and you never see one. Solo also supports more agent CLIs out of the box, has prompt templates and git-worktree linking, and has a considerably larger community around it. And because its stack is a file in the repo rather than state in the app, a teammate gets the same processes on clone — which matters most for a project whose processes are not already declared in something maiTerm can import.',
    rows: [
      {
        dimension: 'The agent CLIs',
        maiterm:
          'Claude Code and Codex through one runtime-neutral pipeline. Others run in a tab, without the agent-aware features.',
        theirs:
          'Built-in support for a longer list — Claude Code, Codex, Amp, Gemini CLI, OpenCode, Copilot CLI and more — plus any interactive CLI as a custom tool.',
      },
      {
        dimension: 'Your dev stack',
        maiterm:
          'A workspace declares what its project runs, imported from `package.json`, a Procfile, compose or a justfile. Each service is a real tab maiTerm owns — kept out of the tab strip so it cannot be closed by accident, watched, restarted with backoff when it crashes — and every agent in the workspace sees and drives the same one.',
        theirs:
          'The centrepiece: `solo.yml` defines the processes and commits with the repo, humans and agents share them, no duplicate `npm run dev`.',
      },
      {
        dimension: 'Supervision',
        maiterm:
          'Overlord watches every agent tab in a window and acts on rules you write — deterministic, no model in the loop, every directive held for approval by default.',
        theirs:
          'Heuristic status detection — working, idle, waiting for permission, blocked — with optional auto-summaries, unread badges and an attention-jump shortcut.',
      },
      {
        dimension: 'Agents talking to each other',
        maiterm:
          'Mesh: every agent in a workspace addresses any other by role, across repositories, with topics and loop caps.',
        theirs:
          'Agent spawning: a lead agent spawns others, even in a different harness, waits for them and collects the result.',
      },
      {
        dimension: 'Remote hosts',
        maiterm:
          'First class. Remote agents over SSH get the same tab identity, task board, notes and MCP tools through a reverse tunnel.',
        theirs:
          'Not offered — their docs point you at SSH and tmux for work that must live on another machine. WSL is supported on Windows.',
      },
      {
        dimension: 'Phone',
        maiterm:
          'maiLink connects a phone directly to your machine over your LAN — watch, answer, approve, and work the board. No cloud in the data path.',
        theirs: 'None.',
      },
      {
        dimension: 'Terminal state',
        maiterm:
          'Scrollback persists to SQLite and comes back after a restart, along with layout and sessions.',
        theirs:
          'Process definitions persist and it can reattach to a running process; terminal output is kept for the current run rather than archived.',
      },
      {
        dimension: 'Platforms',
        maiterm: 'macOS (Apple Silicon), Windows, Linux.',
        theirs: 'macOS and Windows. Linux planned, not yet available.',
      },
      {
        dimension: 'Price',
        maiterm: 'Free, every feature, no account.',
        theirs: 'Free for up to four projects; Pro is $99/year for unlimited.',
      },
    ],
  },
];
