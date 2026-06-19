/**
 * Mesh status-note helpers (docs/mesh-workspace.md §8) — pure string builders + the
 * NEEDS DECISION parser, factored out of the store so the parsing edge cases are unit-tested
 * (meshStatus.test.ts). Each mesh agent maintains ONE workspace note (done / needs-decision /
 * blocked); maiTerm scans it on write and raises a toast when a real decision is pending.
 */

/** Hidden marker tying a workspace note to a member's role, so the status note is reused
 *  (not duplicated) across re-prime / restart. Lives on the first line. */
export function statusMarker(role: string): string {
  return `<!-- mesh:status:${role} -->`;
}

export function buildStatusNoteTemplate(role: string, purpose: string | null): string {
  return (
    `${statusMarker(role)}\n` +
    `### ${role}\n` +
    `_${purpose && purpose.trim() ? purpose.trim() : 'purpose not set'}_\n\n` +
    `**Done:**\n- \n\n` +
    `**NEEDS DECISION:**\n- \n\n` +
    `**Blocked:**\n- \n`
  );
}

/**
 * Extract a section's list items from a status note: from the bold heading (e.g. `**Done:**`)
 * down to the next bold heading (or end), with list markers/whitespace stripped and empty
 * placeholder lines dropped. Anchored on the `**heading` so body text mentioning the word
 * can't be mistaken for the section.
 */
export function extractSection(content: string, heading: string): string[] {
  const escaped = heading.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`\\*\\*\\s*${escaped}`, 'i');
  const idx = content.search(re);
  if (idx === -1) return [];
  let block = content.slice(idx).replace(/^[^\n]*\n?/, ''); // drop the heading line itself
  const end = block.search(/\n\s*\*\*/); // stop at the next bold heading
  if (end !== -1) block = block.slice(0, end);
  return block
    .split('\n')
    .map((l) => l.replace(/^[-*\s]+/, '').trim())
    .filter(Boolean);
}

/**
 * The NEEDS DECISION block joined by "; ". Returns '' when there's no heading or only the
 * empty placeholder — so a freshly templated note (just `- `) raises nothing.
 */
export function parseNeedsDecision(content: string): string {
  return extractSection(content, 'NEEDS DECISION').join('; ');
}

/** Structured view of a status note for the cockpit board (each section's items). */
export function parseStatusNote(content: string): { done: string[]; needsDecision: string[]; blocked: string[] } {
  return {
    done: extractSection(content, 'Done'),
    needsDecision: extractSection(content, 'NEEDS DECISION'),
    blocked: extractSection(content, 'Blocked'),
  };
}
