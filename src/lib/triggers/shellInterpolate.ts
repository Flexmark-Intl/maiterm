/**
 * `%varName` interpolation for text TYPED INTO A SHELL (auto-resume, relaunch, recovery,
 * trigger `send_command`).
 *
 * Trigger variables are captured from terminal output and set by agents, so a value is
 * untrusted: `%claudeSessionId` = `x; curl … | sh` would run at the tab's next spawn (Hive
 * spike 15). A value is substituted only if every character is from a set that is inert in
 * any shell quoting context; anything else counts as unresolved. Titles and notifications
 * keep the raw `interpolateVariables`.
 */

/** Session ids (UUIDs, Codex/Gemini ids), host names, `key=value`, paths without spaces. */
export const SHELL_SAFE_VALUE = /^[A-Za-z0-9._:@+=,/-]*$/;

export function isShellSafe(value: string): boolean {
  return SHELL_SAFE_VALUE.test(value);
}

/** Variables only maiTerm writes (the SSH bridge, `sshMcpBridge.svelte.ts`): agents and trigger
 *  captures are refused them (`isMaitermOwnedVar`), so a value under one of these names is
 *  maiTerm's own. `%maitermExport` is documented for auto-resume commands and has spaces and a
 *  `$HOME`, so it is checked against the one shape the bridge writes instead of the safe set:
 *  a different variable name or plugin folder would hand the remote agent a PATH or a mod. */
const MAITERM_EXPORT =
  /^export MAITERM_TAB_ID=[A-Za-z0-9._-]+ MAITERM_PORT=\d{1,5} MAITERM_AUTH=[A-Za-z0-9]+ CLAUDE_CODE_PLUGIN_DIRS=\$HOME\/\.maiterm\/claude-mod\/maiterm-tab$/;

const MAITERM_OWNED: Record<string, (value: string) => boolean> = {
  maitermTabId: isShellSafe,
  maitermPort: (v) => /^\d{1,5}$/.test(v),
  maitermExport: (v) => MAITERM_EXPORT.test(v),
};

/** A name an agent's `setTriggerVariable` or a trigger capture may not write. */
export function isMaitermOwnedVar(name: string): boolean {
  return Object.prototype.hasOwnProperty.call(MAITERM_OWNED, name);
}

function typeable(name: string, value: string): boolean {
  return isMaitermOwnedVar(name) ? MAITERM_OWNED[name](value) : isShellSafe(value);
}

/** Interpolate from `vars`. A value outside the safe set is treated like an unknown name:
 *  `''` when `clearUnresolved`, else the `%name` token left in place (so a caller's own
 *  "a `%` left means no session" check refuses rather than typing a broken command).
 *  `onUnsafe` hears each refused name. */
export function interpolateShellWith(
  vars: ReadonlyMap<string, string> | undefined,
  text: string,
  clearUnresolved = false,
  onUnsafe?: (name: string) => void,
): string {
  if (!text.includes('%')) return text;
  return text.replace(/%(\w+)/g, (m, name: string) => {
    const value = vars?.get(name);
    if (value !== undefined) {
      if (typeable(name, value)) return value;
      onUnsafe?.(name);
    }
    return clearUnresolved ? '' : m;
  });
}
