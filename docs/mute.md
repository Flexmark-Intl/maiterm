# Mute (2026-10-14)

The human can mute a tab, or a whole workspace, so its agents stop interrupting. A muted tab
notifies nothing: no toast, no desktop notification, no sound, no maiLink phone push. That
includes "needs permission" and "needs your input": the human chose a fully silent mute.

**What a mute never hides** is what waits on the human: the Loom's Decisions queue, the tab's
corner card and tab-strip badge (docs/loom.md, "On the terminal"), the tab's own state
indicators, and the phone's chat list. A muted agent that is stuck still shows as stuck. It just
doesn't interrupt anyone to say so.

## Where it lives

- **`Tab.muted`, `Workspace.muted`** (bool, `serde(default)`, omitted when false). A tab is muted
  if either is set: `AppData::tab_muted` in Rust, `workspacesStore.isTabMuted` in the frontend.
  Read those, never one flag alone.
- **Set by** `set_tab_muted` / `set_workspace_muted`, from the tab's and the workspace's
  right-click menus (Mute notifications / Unmute notifications), or by an agent through the
  `setMuted` MCP tool.
- **A reload keeps it** (the whole record carries), and so do a duplicate and a split, as with
  `overlord_exempt`. A share file never carries it (allowlist, docs/workspace-share.md).
- **Shown** as a struck-through bell: on a muted tab in the strip, and once on a muted
  workspace's sidebar row. The tabs of a muted workspace don't each show it.

## The gates

Each kind of notification goes through one gate:
- **Toasts, OS notifications and their sound** go through `dispatch()` in
  `notificationDispatch.ts`, which drops anything whose `source.tabId` is muted. Request toasts
  are dropped too. Callers must pass the tab: OSC 9 didn't, and was fixed. A notice shared by
  several tabs must name an unmuted one. The watch-script notice named only its newest script's
  tab, so a muted tab silenced everyone else's scripts, for good (`announceApprovals`, found in
  review).
- **The terminal bell** plays its own sound, so it has its own check in `TerminalPane`.
- **Phone pushes** all go through `ring_devices` (mailink/mod.rs), which returns early for a
  muted tab. The WS and the chat list are untouched (docs/mailink-protocol.md §6.2).

Toasts that answer the human's own action (an upload finished, a move refused) don't go through
`dispatch()` and aren't muted. They aren't interruptions.

## The agent's side

- **`setMuted`** `{ muted, scope?: 'tab' | 'workspace', target? }` mutes the caller's own tab by
  default, its workspace with `scope: 'workspace'`, or another tab or workspace in this window by
  `target`. Its description says to use it only on the human's instruction.
  - It is on `PEER_ADDRESSING_TOOLS`, because a mute aimed at a guessed tab silences
    notifications the human wants.
  - Unmuting a tab inside a muted workspace answers `stillMutedBy`, so the agent doesn't report
    "unmuted" to a human who will hear nothing.
- **`sendNotification`** from a muted tab answers `sent: false, muted: true` rather than claiming
  the human saw it.
- **`listWorkspaces`** shows `muted` on workspaces and tabs.
- **`/maiterm mute`**, `unmute`, `mute workspace` and `unmute workspace` are in the skill.
