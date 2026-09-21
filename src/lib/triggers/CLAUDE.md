# Triggers System

Triggers watch terminal output for regex patterns and fire actions. Configured in Preferences > Triggers.

## Architecture

- **Engine**: `src/lib/stores/triggers.svelte.ts` — `processOutput()` called from TerminalPane's PTY listener
- **Flow**: raw PTY bytes → redraw detection → ANSI-stripped → buffer (append or replace) → regex match → dedup check → fire
- **Match modes**: `regex` (default), `plain_text`, `variable` (evaluates variable-condition expressions)

## Variable Conditions

`src/lib/triggers/variableCondition.ts`: Expression parser supporting `a || b && c`, `!x`, `x == "value"`, `x != "value"`. Cached AST.

## Redraw Detection

Raw PTY data is tested for cursor-repositioning sequences (`\e[A`, `\e[H`, `\e[J`) before ANSI stripping. If detected, the buffer is **replaced** (not appended) with the current chunk's stripped text, since TUI redraws overwrite existing content.

## Dedup

Tracks last matched text + timestamp per trigger per tab. If the exact same text matches again within 10s (`DEDUP_WINDOW_MS`), the match is consumed from the buffer but the trigger doesn't fire. Prevents TUI apps (Claude Code / Ink) from re-triggering on redrawn content. On TUI redraws, the dedup timestamp is refreshed (`prev.ts = now`) so the window stays alive while redraws continue, but eventually expires for genuinely new matches.

## Auto-resume Suppression

Tabs with auto-resume commands use a 15s suppression window (vs 2s for normal tabs) before triggers start firing. This prevents false notifications from SSH + Claude auto-resume startup sequences.

## Buffer Consumption

Matched text is always consumed from the buffer, even when blocked by cooldown or dedup. This prevents stale matches from accumulating and re-firing after cooldown expires.

## Actions

- `notify` — dispatches via notification system
- `send_command` — writes to PTY
- `enable_auto_resume`
- `set_tab_state`

## Variables

Capture groups extracted into named variables (`%varName`), persisted per-tab via `trigger_variables`. `interpolateVariables(tabId, text)` replaces `%varName` tokens — used in tab titles, auto-resume commands, notification messages.

## Cooldown

Per-trigger per-tab, prevents rapid re-firing.

## Default Triggers

`src/lib/triggers/defaults.ts`: `DEFAULT_TRIGGERS` is currently empty — all Claude-related triggers have been replaced by hooks integration (PreToolUse, PostToolUse, PreCompact, SessionStart, Stop, Notification).

`seedDefaultTriggers()` runs at app startup (`+layout.svelte` onMount) and on Preferences page mount. It:
1. Handles triggers whose `default_id` has left `DEFAULT_TRIGGERS` — see the retirement rule below
2. Seeds any new defaults that don't exist yet
3. Auto-updates **unmodified** defaults to latest template values (`user_modified` freezes one)

Users can create custom triggers. Deleted defaults tracked in `hidden_default_triggers`.

### Retiring a default: never delete the human's work

**A retired template may delete maiTerm's work, never the user's.** Step 1 used to drop every
trigger whose `default_id` had left the map, which silently destroyed a default the user had
*edited* — no toast, no ledger, nothing in `hidden_default_triggers` to restore from. That was
live, not theoretical: `DEFAULT_TRIGGERS` was emptied wholesale when hooks replaced it, so
every default-linked trigger is stale and the filter took the lot on the next launch.

Now:
- **untouched copy** → dropped (every word in it is ours)
- **`user_modified` copy** → kept, as a plain user trigger: `default_id` and `user_modified`
  both cleared, everything else untouched. Clearing `user_modified` is required, not tidiness —
  the preferences pane enables "Reset to default" on that flag alone, so leaving it set offers a
  button whose handler looks up a template that no longer exists.
- **already deleted by the user** → the ids agree, so `pruneHiddenDefaultTriggers` forgets the
  deletion. A stale id also makes "Restore defaults" render forever and restore nothing.

`src/lib/overlord/defaults.ts` carries the identical rule for Overlord rules; both are covered
by `defaults.test.ts` next to each module.
